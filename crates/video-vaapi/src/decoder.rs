use std::any::Any;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use cros_codecs::DecodedFormat;
use cros_codecs::decoder::BlockingMode;
use cros_codecs::decoder::DecodedHandle;
use cros_codecs::decoder::DecoderEvent;
use cros_codecs::decoder::DynDecodedHandle;
use cros_codecs::decoder::stateless::StatelessVideoDecoder;
use cros_codecs::libva::{
    VA_RT_FORMAT_YUV420, VA_RT_FORMAT_YUV420_10, VA_RT_FORMAT_YUV420_12, VA_RT_FORMAT_YUV422,
    VA_RT_FORMAT_YUV422_10, VA_RT_FORMAT_YUV422_12, VA_RT_FORMAT_YUV444, VA_RT_FORMAT_YUV444_10,
    VA_RT_FORMAT_YUV444_12,
};
use tracing::{debug, info, trace, warn};
use video_core::{ColorSpace, DecodedFrame, FrameTextureHandle, VideoDecoder};
use webm_demux::packet::Packet;

use crate::frame_pool::DmaFramePool;
use crate::internal_vaapi_frame::InternalVaapiFrame;
use crate::texture_cache::WgpuTexturePool;

/// Количество кадров в DMA-пуле.
///
/// VP9 decoder может держать до 8 reference frames.
/// 16 буферов даёт 8 свободных для decode pipeline,
/// что предотвращает исчерпание пула при быстром decode.
const FRAME_POOL_SIZE: usize = 16;

/// Начальное разрешение для создания пула кадров.
///
/// VA-API декодер требует выходные буферы до первого decode call.
/// Используем 1920x1080 как разумный default — при смене разрешения
/// пул будет пересоздан через `FormatChanged` event.
const INITIAL_WIDTH: u32 = 1920;
const INITIAL_HEIGHT: u32 = 1080;

/// Преобразует decoded output format из `StreamInfo` в VA RT format для surface pool.
fn rt_format_for_decoded_format(decoded_format: DecodedFormat) -> Result<u32> {
    match decoded_format {
        DecodedFormat::NV12 | DecodedFormat::I420 => Ok(VA_RT_FORMAT_YUV420),
        DecodedFormat::I010 => Ok(VA_RT_FORMAT_YUV420_10),
        DecodedFormat::I012 => Ok(VA_RT_FORMAT_YUV420_12),
        DecodedFormat::I422 => Ok(VA_RT_FORMAT_YUV422),
        DecodedFormat::I210 => Ok(VA_RT_FORMAT_YUV422_10),
        DecodedFormat::I212 => Ok(VA_RT_FORMAT_YUV422_12),
        DecodedFormat::I444 => Ok(VA_RT_FORMAT_YUV444),
        DecodedFormat::I410 => Ok(VA_RT_FORMAT_YUV444_10),
        DecodedFormat::I412 => Ok(VA_RT_FORMAT_YUV444_12),
        other => Err(anyhow::anyhow!(
            "Unsupported VA decoded format for internal surface pool: {:?}",
            other
        )),
    }
}

/// Загружает кадр стабильным CPU-путём: `map()` DMA-BUF и `Queue::write_texture()`.
fn upload_frame_with_cpu_fallback(
    texture_cache: &mut WgpuTexturePool,
    decoded_handle: &dyn DecodedHandle<Frame = InternalVaapiFrame>,
    queue: &wgpu::Queue,
    sync_elapsed: u128,
    upload_start: std::time::Instant,
) -> Result<FrameTextureHandle> {
    match texture_cache.upload_frame(decoded_handle, queue) {
        Ok(texture_handle) => {
            let elapsed = upload_start.elapsed().as_millis();
            debug!(
                handle_id = texture_handle.0,
                sync_ms = sync_elapsed,
                upload_ms = elapsed,
                "CPU texture upload successful"
            );
            Ok(texture_handle)
        }
        Err(upload_error) => {
            warn!(error = %upload_error, "Texture upload failed — dropping frame");
            Err(anyhow::anyhow!("Texture upload failed: {}", upload_error))
        }
    }
}

/// VA-API VP9 hardware decoder, реализующий трейт [`VideoDecoder`].
///
/// Оборачивает `cros-codecs::StatelessDecoder<Vp9, VaapiBackend<InternalVaapiFrame>>`
/// с internal VA surfaces, wgpu texture cache и drain events внутри `decode()`
/// для предоставления синхронного интерфейса.
pub struct VaapiVideoDecoder {
    /// Внутренний stateless decoder как trait object.
    ///
    /// Используем `DynStatelessVideoDecoder` чтобы избежать monomorphization
    /// и упростить тип (иначе пришлось бы тащить generic параметры через всё приложение).
    inner: cros_codecs::decoder::stateless::DynStatelessVideoDecoder<InternalVaapiFrame>,

    /// Пул lightweight frame descriptors для выходных VA surfaces.
    frame_pool: DmaFramePool,

    /// Пул wgpu-текстур для загруженных кадров.
    ///
    /// Arc<Mutex<>> потому что пул используется из decoder thread (upload)
    /// и из render thread (get_views / release).
    texture_cache: Arc<Mutex<WgpuTexturePool>>,

    /// Очередь готовых к отображению кадров.
    ///
    /// Кадры добавляются при обработке `FrameReady` event и возвращаются
    /// из `decode()` в порядке FIFO.
    ready_queue: VecDeque<DecodedFrame>,

    /// wgpu device — нужен для создания текстур в `WgpuTexturePool`.
    ///
    /// В настоящий момент device передаётся в `WgpuTexturePool` при создании,
    /// но храним ссылку здесь для будущих расширений (ленивая аллокация текстур).
    #[allow(dead_code)]
    device: Arc<wgpu::Device>,

    /// wgpu queue — нужен для `write_texture()` при загрузке декодированных кадров.
    queue: Arc<wgpu::Queue>,

    /// Имя бэкенда для отображения в UI.
    backend_name: &'static str,
}

impl VaapiVideoDecoder {
    /// Создаёт новый VA-API VP9 decoder.
    ///
    /// # Аргументы
    /// * `device` — [`Arc<wgpu::Device>`] для создания wgpu-текстур.
    /// * `queue` — [`Arc<wgpu::Queue>`] для загрузки данных в текстуры.
    ///
    /// # Ошибки
    /// Возвращает ошибку если:
    /// - VA-API display недоступен (`vainfo` не показывает VP9),
    /// - не удалось создать stateless decoder,
    /// - не удалось создать GBM frame pool.
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Result<Self> {
        let texture_cache = Arc::new(Mutex::new(WgpuTexturePool::new(device.clone(), None)));
        Self::new_with_pool(device, queue, texture_cache)
    }

    pub fn new_with_pool(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        texture_cache: Arc<Mutex<WgpuTexturePool>>,
    ) -> Result<Self> {
        info!("Opening VA-API display");

        // Открываем VA-API display. `Display::open()` возвращает `Option<Rc<Display>>`.
        // Если None — значит VA-API недоступна (нет драйвера, нет устройства).
        let display = cros_codecs::libva::Display::open()
            .ok_or_else(|| anyhow::anyhow!("Failed to open VA-API display: libva not available"))?;

        info!("Creating VA-API VP9 decoder");

        // Создаём stateless decoder для VP9 с VA-API backend.
        // `BlockingMode::Blocking` упрощает синхронизацию — decode ждёт завершения GPU.
        //
        // Используем turbofish для явного указания generic параметров,
        // так как `new_vaapi` определён для нескольких кодеков (AV1, H264, H265, VP8, VP9)
        // и компилятор не может выбрать нужный impl без подсказки.
        type VaapiVp9Decoder = cros_codecs::decoder::stateless::StatelessDecoder<
            cros_codecs::decoder::stateless::vp9::Vp9,
            cros_codecs::backend::vaapi::decoder::VaapiBackend<InternalVaapiFrame>,
        >;

        let decoder = VaapiVp9Decoder::new_vaapi(display, BlockingMode::Blocking)
            .map_err(|e| anyhow::anyhow!("Failed to create VA-API decoder: {:?}", e))?;

        // Преобразуем в trait object чтобы избежать generic типов в поле структуры.
        let inner = decoder.into_trait_object();

        info!("Creating internal VA frame pool");

        // Создаём пул выходных буферов. Декодер требует буферы до первого вызова decode().
        let frame_pool = DmaFramePool::new(INITIAL_WIDTH, INITIAL_HEIGHT, FRAME_POOL_SIZE)
            .map_err(|e| anyhow::anyhow!("Failed to create frame pool: {}", e))?;

        info!("VA-API VP9 decoder initialized successfully");

        Ok(Self {
            inner,
            frame_pool,
            texture_cache,
            ready_queue: VecDeque::new(),
            device,
            queue,
            backend_name: "VA-API VP9",
        })
    }

    /// Возвращает Y/UV texture views для заданного frame handle.
    ///
    /// Используется в рендер-цикле для получения views по handle из [`DecodedFrame::texture_handle`].
    ///
    /// # Аргументы
    /// * `frame_handle` — [`FrameTextureHandle`], полученный из [`DecodedFrame`].
    ///
    /// # Возвращаемое значение
    /// `Some((y_view, uv_view))` если handle найден и слот занят.
    /// Возвращает Y/UV texture views для заданного frame handle.
    ///
    /// Thread-safe: вызывается из render thread.
    pub fn get_wgpu_texture_views(
        &self,
        frame_handle: FrameTextureHandle,
    ) -> Option<(wgpu::TextureView, wgpu::TextureView)> {
        self.texture_cache.lock().unwrap().get_views(frame_handle)
    }

    /// Освобождает texture slot, связанный с данным frame handle.
    ///
    /// Должен вызываться когда кадр больше не нужен (drop по A/V sync,
    /// замена present frame, очистка очереди и т.д.).
    /// Без этого texture pool исчерпается после 8 кадров.
    /// Освобождает texture slot.
    ///
    /// Thread-safe: вызывается из decoder thread (через channel) или render thread.
    pub fn release_frame(&mut self, texture_handle: FrameTextureHandle) {
        trace!(handle_id = texture_handle.0, "Releasing texture slot");
        self.texture_cache
            .lock()
            .unwrap()
            .release_slot(texture_handle);
    }

    /// Возвращает статистику texture pool для отладки.
    pub fn texture_pool_stats(&self) -> (usize, usize) {
        let cache = self.texture_cache.lock().unwrap();
        (cache.num_slots(), cache.num_in_use())
    }

    /// Обрабатывает готовый кадр от decoder: синхронизация, upload в wgpu, возврат буфера в пул.
    ///
    /// # Аргументы
    /// * `handle` — handle декодированного кадра от cros-codecs.
    ///
    /// # Ошибки
    /// Возвращает ошибку если sync или texture upload не удался.
    fn process_ready_frame(&mut self, handle: DynDecodedHandle<InternalVaapiFrame>) -> Result<()> {
        // Получаем разрешения кадра ДО sync (sync может потребовать mutable borrow).
        let resolution = handle.coded_resolution();
        let display_resolution = handle.display_resolution();
        let timestamp = handle.timestamp();

        debug!(
            pts_ms = timestamp / 1000,
            coded_width = resolution.width,
            coded_height = resolution.height,
            display_width = display_resolution.width,
            display_height = display_resolution.height,
            "FrameReady: processing decoded frame"
        );

        // Шаг 1: Синхронизируемся с завершением GPU-декодирования.
        // `sync()` блокируется до тех пор, пока VA-API не закончит decode job.
        let sync_start = std::time::Instant::now();
        if let Err(e) = handle.sync() {
            warn!(error = %e, "GPU decode sync failed — dropping frame");
            return Err(anyhow::anyhow!("GPU decode sync failed: {}", e));
        }
        let sync_elapsed = sync_start.elapsed().as_millis();

        // Шаг 2: Загружаем кадр через VA image readback.
        let upload_start = std::time::Instant::now();
        let mut cache = self.texture_cache.lock().unwrap();
        let texture_handle = upload_frame_with_cpu_fallback(
            &mut cache,
            &*handle,
            &self.queue,
            sync_elapsed,
            upload_start,
        )?;
        drop(cache);

        // Шаг 3: Возвращаем backing frame в пул для повторного использования.
        //
        // `handle.video_frame()` возвращает `Arc<InternalVaapiFrame>` (clone).
        // Чтобы `Arc::try_unwrap` сработал, нужно убедиться что refcount == 1.
        // Handle внутри держит `backing_frame: Arc<InternalVaapiFrame>`,
        // так что после clone refcount >= 2. Drop handle уменьшает refcount.
        // Если decoder не держит этот кадр как reference frame, refcount станет 1
        // и `Arc::try_unwrap` вернёт Ok — frame возвращается в пул.
        let frame_arc = handle.video_frame();
        drop(handle); // дропаем handle, уменьшаем refcount Arc
        if let Ok(frame) = Arc::try_unwrap(frame_arc) {
            self.frame_pool.return_frame(frame);
            trace!("Frame returned to pool");
        } else {
            debug!("Frame still referenced by decoder, cannot return to pool yet");
        }

        // Шаг 4: Добавляем кадр в очередь готовых к отображению.
        self.ready_queue.push_back(DecodedFrame {
            pts: Duration::from_micros(timestamp),
            width: resolution.width,
            height: resolution.height,
            render_width: display_resolution.width,
            render_height: display_resolution.height,
            color_space: ColorSpace::Bt709Limited,
            texture_handle,
        });

        trace!(pts_ms = timestamp / 1000, "Frame queued for presentation");

        Ok(())
    }
}

impl VideoDecoder for VaapiVideoDecoder {
    /// Декодирует один VP9 packet.
    ///
    /// Pipeline:
    /// 1. Submit bitstream в cros-codecs decoder.
    /// 2. Drain все pending events (FrameReady / FormatChanged).
    /// 3. Вернуть самый старый готовый кадр из очереди.
    fn decode(&mut self, packet: &Packet) -> Result<Option<DecodedFrame>> {
        // Конвертируем PTS из Duration в микросекунды (u64).
        // cros-codecs использует u64 timestamp для идентификации кадров.
        let timestamp_us = packet.pts.as_micros() as u64;
        let decode_start = std::time::Instant::now();

        trace!(
            timestamp_us = timestamp_us,
            data_len = packet.data.len(),
            "decode() called"
        );

        // Closure, предоставляющий свободный frame из пула декодеру.
        // Декодер вызывает этот callback когда ему нужен новый выходной буфер.
        //
        // Используем `alloc_or_allocate` чтобы не останавливать decode
        // если пул исчерпан (reference frames держатся decoder'ом).
        let mut alloc_cb = || {
            let frame = self.frame_pool.alloc_or_allocate();
            if frame.is_none() {
                warn!("Frame pool exhausted — decoder needs more output buffers");
            }
            frame
        };

        // Шаг 1: Отправляем bitstream в decoder.
        let inner_decode_start = std::time::Instant::now();
        match self.inner.decode(timestamp_us, &packet.data, &mut alloc_cb) {
            Ok(_processed_bytes) => {
                trace!("decode() accepted bitstream");
            }
            Err(cros_codecs::decoder::stateless::DecodeError::CheckEvents) => {
                // Decoder требует drain events перед продолжением.
                // Это нормально — мы всегда drain events после decode().
                debug!("Decoder requested event drain");
            }
            Err(cros_codecs::decoder::stateless::DecodeError::NotEnoughOutputBuffers(n)) => {
                // Пул исчерпан. Это не должно произойти при нормальной работе
                // (приложение держит ~3 кадра, у нас 12 в пуле).
                warn!(needed = n, "Decoder out of output buffers");
            }
            Err(cros_codecs::decoder::stateless::DecodeError::ParseFrameError(msg)) => {
                // Ошибка парсинга VP9 — пропускаем пакет, продолжаем декодирование.
                warn!(%msg, "VP9 parse error, skipping packet");
                return Ok(None);
            }
            Err(e) => {
                // Прочие ошибки (backend error и т.д.) — возвращаем upstream.
                warn!(error = ?e, "Decode error");
                return Err(anyhow::anyhow!("Decode error: {:?}", e));
            }
        }
        let inner_decode_elapsed = inner_decode_start.elapsed().as_millis();

        // Шаг 2: Drain все pending events.
        //
        // Обрабатываем:
        // - FrameReady: sync + map + upload → ready_queue
        // - FormatChanged: сбрасываем texture cache (старое разрешение больше не валидно)
        let drain_start = std::time::Instant::now();
        let mut events_count = 0;
        while let Some(event) = self.inner.next_event() {
            events_count += 1;
            match event {
                DecoderEvent::FrameReady(handle) => {
                    let pts_ms = handle.timestamp() / 1000;
                    trace!(pts_ms = pts_ms, "DecoderEvent::FrameReady");
                    if let Err(e) = self.process_ready_frame(handle) {
                        warn!(error = %e, "Failed to process ready frame");
                    }
                }
                DecoderEvent::FormatChanged => {
                    info!("Format changed, invalidating texture cache and frame pool");
                    // Очищаем ready_queue — кадры в ней имеют невалидные texture handles
                    // после invalidate_all().
                    let dropped = self.ready_queue.len();
                    if dropped > 0 {
                        info!(dropped, "Clearing ready_queue after FormatChanged");
                        self.ready_queue.clear();
                    }
                    self.texture_cache.lock().unwrap().invalidate_all();

                    // Пересоздаём frame pool под новое разрешение.
                    // Decoder уже вызвал `flush()` и сбросил reference frames,
                    // поэтому старые буферы можно безопасно дропать.
                    // `stream_info()` уже обновлён через `new_sequence()` внутри decode().
                    if let Some(stream_info) = self.inner.stream_info() {
                        let res = stream_info.coded_resolution;
                        let rt_format = match rt_format_for_decoded_format(stream_info.format) {
                            Ok(rt_format) => rt_format,
                            Err(error) => {
                                warn!(
                                    error = %error,
                                    decoded_format = ?stream_info.format,
                                    "Cannot map decoded format to VA RT format"
                                );
                                continue;
                            }
                        };
                        if let Err(e) = self.frame_pool.resize_with_rt_format(
                            res.width,
                            res.height,
                            FRAME_POOL_SIZE,
                            rt_format,
                        ) {
                            warn!(
                                error = %e,
                                width = res.width,
                                height = res.height,
                                rt_format,
                                "Failed to resize frame pool after format change"
                            );
                        } else {
                            info!(
                                width = res.width,
                                height = res.height,
                                decoded_format = ?stream_info.format,
                                rt_format,
                                "Frame pool resized for new format"
                            );
                        }
                    } else {
                        warn!("FormatChanged event without stream_info — cannot resize frame pool");
                    }
                }
            }
        }

        // Шаг 3: Возвращаем самый старый готовый кадр (FIFO).
        let result = self.ready_queue.pop_front();
        let decode_elapsed = decode_start.elapsed().as_millis();
        let drain_elapsed = drain_start.elapsed().as_millis();
        if let Some(ref frame) = result {
            debug!(
                pts_ms = frame.pts.as_millis(),
                width = frame.width,
                height = frame.height,
                inner_decode_ms = inner_decode_elapsed,
                drain_ms = drain_elapsed,
                decode_ms = decode_elapsed,
                "decode() returning frame"
            );
        } else if events_count > 0 {
            debug!(
                inner_decode_ms = inner_decode_elapsed,
                drain_ms = drain_elapsed,
                decode_ms = decode_elapsed,
                "decode() completed: no frame ready (events={})",
                events_count
            );
        } else {
            trace!(
                inner_decode_ms = inner_decode_elapsed,
                drain_ms = drain_elapsed,
                decode_ms = decode_elapsed,
                "decode() completed: no events, no frame"
            );
        }
        Ok(result)
    }

    /// Сбрасывает decoder: завершает все pending decode requests.
    ///
    /// После flush decoder требует keyframe перед возобновлением декодирования.
    fn flush(&mut self) -> Result<()> {
        self.inner
            .flush()
            .map_err(|e| anyhow::anyhow!("Flush error: {:?}", e))?;
        self.ready_queue.clear();
        Ok(())
    }

    /// Возвращает имя бэкенда для отображения в UI.
    fn backend_name(&self) -> &'static str {
        self.backend_name
    }

    /// Downcast к конкретному типу для backend-specific операций.
    fn as_any(&self) -> &dyn Any {
        self
    }

    /// Mutable downcast к конкретному типу.
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
