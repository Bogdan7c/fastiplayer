use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata};
use cros_codecs::DecodedFormat;
use cros_codecs::decoder::BlockingMode;
use cros_codecs::decoder::DecodedHandle;
use cros_codecs::decoder::DecoderEvent;
use cros_codecs::decoder::DynDecodedHandle;
use cros_codecs::decoder::stateless::DecodeError;
use cros_codecs::decoder::stateless::StatelessVideoDecoder;
use cros_codecs::libva::{
    VA_RT_FORMAT_YUV420, VA_RT_FORMAT_YUV420_10, VA_RT_FORMAT_YUV420_12, VA_RT_FORMAT_YUV422,
    VA_RT_FORMAT_YUV422_10, VA_RT_FORMAT_YUV422_12, VA_RT_FORMAT_YUV444, VA_RT_FORMAT_YUV444_10,
    VA_RT_FORMAT_YUV444_12,
};
use media_core::Packet;
use tracing::{debug, info, trace, warn};
use video_core::{
    DecodedFrame, DecodedPixelFormat, FrameMemoryPath, FrameTextureHandle, VideoDecoder,
};

use crate::frame_pool::DmaFramePool;
use crate::internal_vaapi_frame::InternalVaapiFrame;
use crate::texture_cache::WgpuTexturePool;

/// Количество кадров в DMA-пуле.
///
/// VP9 decoder может держать до 8 reference frames.
/// 16 буферов даёт 8 свободных для decode pipeline,
/// что предотвращает исчерпание пула при быстром decode.
const FRAME_POOL_SIZE: usize = 16;

/// Максимум кадров, которые decoder держит уже импортированными, но ещё не отданными UI.
///
/// Presentation queue живёт в app-egui, но cros-codecs может вернуть несколько
/// `FrameReady` events за один decode call. Этот лимит не даёт скрытой decoder
/// очереди занять все zero-copy texture slots.
const READY_QUEUE_LIMIT: usize = 3;

/// Максимум повторных submit попыток после `DecodeError::CheckEvents`.
///
/// `cros-codecs` использует `CheckEvents` как backpressure-сигнал:
/// вызывающий код должен обработать pending events и повторить тот же bitstream.
/// Лимит защищает decoder thread от бесконечного цикла при поломанном backend state.
const MAX_CHECK_EVENTS_RETRIES: usize = 4;

/// Начальное разрешение для создания пула кадров.
///
/// VA-API декодер требует выходные буферы до первого decode call.
/// Используем 1920x1080 как разумный default — при смене разрешения
/// пул будет пересоздан через `FormatChanged` event.
const INITIAL_WIDTH: u32 = 1920;
const INITIAL_HEIGHT: u32 = 1080;

/// Итог обработки pending decoder events.
///
/// Отдельный report нужен, чтобы retry-loop мог видеть, был ли `FormatChanged`,
/// и писать диагностический лог без знания деталей `FrameReady` upload path.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct DecoderDrainReport {
    /// Количество событий, прочитанных через `next_event()`.
    events_count: usize,

    /// Был ли среди событий `FormatChanged`.
    format_changed: bool,
}

/// Сводка одного вызова decode state machine.
///
/// `decode()` использует её только для логов и решения, был ли packet пропущен
/// как recoverable parse error. Все готовые кадры по-прежнему лежат в `ready_queue`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct DecodeLoopReport {
    /// Сколько раз packet был отправлен в `inner.decode()`.
    attempts: usize,

    /// Количество обработанных decoder events за весь вызов.
    events_count: usize,

    /// Был ли обработан `FormatChanged`.
    format_changed: bool,

    /// Сколько байт backend сообщил как обработанные.
    processed_bytes: usize,

    /// Был ли packet пропущен из-за recoverable parse error.
    skipped_packet: bool,

    /// Суммарное время внутри submit attempts.
    submit_elapsed: Duration,

    /// Суммарное время внутри drain events.
    drain_elapsed: Duration,
}

impl DecodeLoopReport {
    /// Добавляет результат одного drain прохода к общей сводке.
    fn record_drain(&mut self, drain_report: DecoderDrainReport, drain_elapsed: Duration) {
        self.events_count += drain_report.events_count;
        self.format_changed |= drain_report.format_changed;
        self.drain_elapsed += drain_elapsed;
    }
}

/// Минимальный интерфейс, который нужен retry state machine.
///
/// Production implementation живёт на `VaapiVideoDecoder`, а unit test подставляет
/// fake driver без VA-API/wgpu, чтобы проверить контракт `CheckEvents -> retry same packet`.
trait DecoderRetryDriver {
    /// Отправляет один packet в backend decoder.
    fn submit_packet(
        &mut self,
        timestamp_us: u64,
        packet_data: &[u8],
    ) -> std::result::Result<usize, DecodeError>;

    /// Обрабатывает все pending decoder events.
    fn drain_events(&mut self) -> Result<DecoderDrainReport>;
}

/// Выполняет submit packet с bounded retry после `CheckEvents`.
///
/// Важно: `CheckEvents` не означает, что packet consumed. По контракту `cros-codecs`
/// вызывающий код обязан обработать события и повторить `decode()` с теми же данными.
fn run_decode_with_event_retry<D>(
    driver: &mut D,
    timestamp_us: u64,
    packet_data: &[u8],
    keyframe: bool,
) -> Result<DecodeLoopReport>
where
    D: DecoderRetryDriver + ?Sized,
{
    let pts_ms = timestamp_us / 1000;
    let mut report = DecodeLoopReport::default();

    loop {
        report.attempts += 1;
        let attempt = report.attempts;
        let submit_start = std::time::Instant::now();
        let submit_result = driver.submit_packet(timestamp_us, packet_data);
        report.submit_elapsed += submit_start.elapsed();

        match submit_result {
            Ok(processed_bytes) => {
                report.processed_bytes = processed_bytes;
                trace!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    processed_bytes = processed_bytes,
                    "decode() accepted bitstream"
                );
                let drain_start = std::time::Instant::now();
                let drain_report = driver.drain_events()?;
                report.record_drain(drain_report, drain_start.elapsed());
                return Ok(report);
            }
            Err(DecodeError::CheckEvents) => {
                let drain_start = std::time::Instant::now();
                let drain_report = driver.drain_events()?;
                let format_changed = drain_report.format_changed;
                report.record_drain(drain_report, drain_start.elapsed());

                if attempt > MAX_CHECK_EVENTS_RETRIES {
                    return Err(anyhow::anyhow!(
                        "Decoder repeatedly requested event drain after {attempt} attempts"
                    ));
                }

                debug!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    format_changed = format_changed,
                    "retrying same VP9 packet after decoder event drain"
                );
            }
            Err(DecodeError::NotEnoughOutputBuffers(needed)) => {
                warn!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    needed = needed,
                    "Decoder out of output buffers"
                );
                let drain_start = std::time::Instant::now();
                let drain_report = driver.drain_events()?;
                report.record_drain(drain_report, drain_start.elapsed());
                return Ok(report);
            }
            Err(DecodeError::ParseFrameError(message)) => {
                report.skipped_packet = true;
                warn!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    %message,
                    "VP9 parse error, skipping packet"
                );
                return Ok(report);
            }
            Err(error) => {
                warn!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    error = ?error,
                    "Decode error"
                );
                return Err(anyhow::anyhow!("Decode error: {:?}", error));
            }
        }
    }
}

/// Typed контракт decoded surface, который VA-API backend отдаёт renderer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DecodedSurfaceContract {
    /// Pixel format renderer boundary.
    format: DecodedPixelFormat,

    /// Bit depth decoded samples.
    bit_depth: BitDepth,

    /// Chroma subsampling decoded frame-а.
    chroma: ChromaSubsampling,
}

/// Фатальное нарушение P010 boundary contract.
///
/// P010 кадр нельзя безопасно отправлять в CPU fallback: так мы потеряем
/// 10-bit surface contract и замаскируем отсутствие zero-copy path. Поэтому
/// такие ошибки должны останавливать decoder thread, а не повторяться на
/// каждом следующем кадре.
#[derive(Debug)]
struct P010BoundaryViolation {
    /// Человекочитаемое объяснение конкретной причины отказа.
    detail: String,
}

impl P010BoundaryViolation {
    /// Создаёт ошибку P010 boundary с понятной причиной для лога/UI.
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for P010BoundaryViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for P010BoundaryViolation {}

/// Проверяет, что decode error требует остановить decoder thread.
pub(crate) fn is_fatal_decoder_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<P010BoundaryViolation>().is_some()
}

/// Создаёт typed anyhow error для фатальной P010 boundary ошибки.
fn p010_boundary_violation(detail: impl Into<String>) -> anyhow::Error {
    P010BoundaryViolation::new(detail).into()
}

impl DecodedSurfaceContract {
    /// Создаёт контракт для текущего production NV12 path.
    const fn nv12() -> Self {
        Self {
            format: DecodedPixelFormat::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
        }
    }

    /// Создаёт контракт для P010 zero-copy boundary.
    const fn p010() -> Self {
        Self {
            format: DecodedPixelFormat::P010,
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv420,
        }
    }
}

/// Преобразует `cros-codecs` decoded format в внешний frame contract.
///
/// Важно: `cros-codecs::DecodedFormat::I010` здесь приходит из VA `P010`
/// image format mapping. Для renderer boundary это не planar I010 upload path,
/// а P010 DMA-BUF zero-copy contract.
fn decoded_contract_for_stream_format(
    decoded_format: DecodedFormat,
) -> Result<DecodedSurfaceContract> {
    match decoded_format {
        DecodedFormat::NV12 | DecodedFormat::I420 => Ok(DecodedSurfaceContract::nv12()),
        DecodedFormat::I010 => Ok(DecodedSurfaceContract::p010()),
        other => Err(anyhow::anyhow!(
            "Unsupported decoded stream format for VA-API renderer boundary: {:?}",
            other
        )),
    }
}

/// Преобразует VA RT format в тот же внешний frame contract.
fn decoded_contract_for_rt_format(rt_format: u32) -> Result<DecodedSurfaceContract> {
    match rt_format {
        VA_RT_FORMAT_YUV420 => Ok(DecodedSurfaceContract::nv12()),
        VA_RT_FORMAT_YUV420_10 => Ok(DecodedSurfaceContract::p010()),
        other => Err(anyhow::anyhow!(
            "Unsupported VA RT format for VA-API renderer boundary: {:#x}",
            other
        )),
    }
}

/// Проверяет zero-copy boundary requirement до CPU fallback.
fn ensure_zero_copy_importer_for_contract(
    decoded_contract: DecodedSurfaceContract,
    can_import_dma_buf: bool,
) -> Result<()> {
    if decoded_contract.format == DecodedPixelFormat::P010 && !can_import_dma_buf {
        return Err(p010_boundary_violation(
            "P010 decoded frame requires DMA-BUF zero-copy importer, but importer is unavailable",
        ));
    }

    Ok(())
}

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

/// Готовый кадр вместе с причиной, по которой он помещается в очередь.
enum ReadyFrameSource {
    /// Кадр пришёл через zero-copy DMA-BUF import.
    ZeroCopy,
    /// Кадр пришёл через стабильный CPU upload path.
    CpuUpload,
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

    /// Handles кадров, которые сейчас отображаются через zero-copy DMA-BUF.
    ///
    /// Пока handle находится в этой map, VA surface не возвращается в frame pool
    /// и decoder не может перезаписать память, которую семплит renderer.
    zero_copy_guards: HashMap<u64, DynDecodedHandle<InternalVaapiFrame>>,

    /// Был ли уже залогирован fallback с zero-copy на CPU upload.
    zero_copy_failure_logged: bool,

    /// Был ли уже залогирован первый успешный zero-copy кадр.
    zero_copy_success_logged: bool,

    /// Была ли уже залогирована проверенная P010 zero-copy boundary.
    p010_boundary_verified_logged: bool,

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
            zero_copy_guards: HashMap::new(),
            zero_copy_failure_logged: false,
            zero_copy_success_logged: false,
            p010_boundary_verified_logged: false,
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
        let retired_slot = self
            .texture_cache
            .lock()
            .unwrap()
            .release_slot(texture_handle);
        if let Some(retired_slot) = retired_slot {
            let ready_handle = retired_slot.frame_handle;
            drop(retired_slot);
            self.release_zero_copy_frame(ready_handle);
        }
    }

    /// Возвращает статистику texture pool для отладки.
    pub fn texture_pool_stats(&self) -> (usize, usize) {
        let cache = self.texture_cache.lock().unwrap();
        (cache.num_slots(), cache.num_in_use())
    }

    /// Определяет decoded surface contract для текущего ready handle.
    fn current_decoded_contract(
        &self,
        handle: &DynDecodedHandle<InternalVaapiFrame>,
    ) -> Result<DecodedSurfaceContract> {
        if let Some(stream_info) = self.inner.stream_info() {
            return decoded_contract_for_stream_format(stream_info.format);
        }

        let backing_frame = handle.video_frame();
        decoded_contract_for_rt_format(backing_frame.rt_format())
    }

    /// Освобождает VA handle, удерживаемый zero-copy кадром.
    pub fn release_zero_copy_frame(&mut self, texture_handle: FrameTextureHandle) {
        let Some(handle) = self.zero_copy_guards.remove(&texture_handle.0) else {
            trace!(
                handle_id = texture_handle.0,
                "No zero-copy guard found for released frame"
            );
            return;
        };

        self.return_frame_from_handle(handle);
    }

    /// Возвращает backing frame в pool после того, как decoded handle больше не нужен.
    fn return_frame_from_handle(&mut self, handle: DynDecodedHandle<InternalVaapiFrame>) {
        let frame_arc = handle.video_frame();
        drop(handle);

        if let Ok(frame) = Arc::try_unwrap(frame_arc) {
            self.frame_pool.return_frame(frame);
            trace!("Frame returned to pool");
        } else {
            debug!("Frame still referenced by decoder, cannot return to pool yet");
        }
    }

    /// Добавляет готовый кадр в decoder ready queue, сбрасывая самый старый backlog.
    ///
    /// UI получает кадры через отдельный канал, поэтому decoder может временно
    /// накопить уже импортированные textures внутри `ready_queue`. Для видео важнее
    /// держать низкую задержку и не исчерпывать GPU slots, чем сохранить каждый
    /// промежуточный кадр при backlog.
    fn push_ready_frame(&mut self, frame: DecodedFrame, source: ReadyFrameSource) {
        if let Err(error) = frame.validate_contract() {
            let invalid_texture_handle = frame.texture_handle;
            warn!(
                error = %error,
                format = %frame.format,
                memory_path = %frame.memory_path,
                "Decoded frame contract validation failed before ready queue"
            );
            self.release_frame(invalid_texture_handle);
            return;
        }

        while self.ready_queue.len() >= READY_QUEUE_LIMIT {
            let Some(stale_frame) = self.ready_queue.pop_front() else {
                break;
            };
            let stale_pts_ms = stale_frame.pts.as_millis();
            self.release_frame(stale_frame.texture_handle);
            debug!(
                stale_pts_ms,
                ready_queue_limit = READY_QUEUE_LIMIT,
                "Dropping stale decoded frame from internal ready queue"
            );
        }

        match source {
            ReadyFrameSource::ZeroCopy => trace!(
                pts_ms = frame.pts.as_millis(),
                handle_id = frame.texture_handle.0,
                format = %frame.format,
                bit_depth = %frame.bit_depth,
                chroma = %frame.chroma,
                color_origin = ?frame.color.origin,
                color_confidence = ?frame.color.confidence,
                queue_len = self.ready_queue.len() + 1,
                "Zero-copy frame queued for presentation"
            ),
            ReadyFrameSource::CpuUpload => trace!(
                pts_ms = frame.pts.as_millis(),
                handle_id = frame.texture_handle.0,
                format = %frame.format,
                bit_depth = %frame.bit_depth,
                chroma = %frame.chroma,
                color_origin = ?frame.color.origin,
                color_confidence = ?frame.color.confidence,
                queue_len = self.ready_queue.len() + 1,
                "CPU-upload frame queued for presentation"
            ),
        }

        self.ready_queue.push_back(frame);
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
        let decoded_contract = self.current_decoded_contract(&handle)?;

        // Шаг 2: Если включён zero-copy importer, пробуем экспортировать VA surface
        // как DMA-BUF и импортировать её в Vulkan/wgpu texture без CPU readback.
        let can_import_dma_buf = self.texture_cache.lock().unwrap().can_import_dma_buf();
        if can_import_dma_buf {
            match handle.dma_buf_image() {
                Ok(Some(dma_buf_image)) => {
                    let import_result = self
                        .texture_cache
                        .lock()
                        .unwrap()
                        .import_dma_buf_image(&dma_buf_image);

                    match import_result {
                        Ok(texture_handle) => {
                            if !self.zero_copy_success_logged {
                                self.zero_copy_success_logged = true;
                                info!(
                                    handle_id = texture_handle.0,
                                    format = %decoded_contract.format,
                                    "Zero-copy DMA-BUF import succeeded"
                                );
                            }
                            if decoded_contract.format == DecodedPixelFormat::P010
                                && !self.p010_boundary_verified_logged
                            {
                                self.p010_boundary_verified_logged = true;
                                info!(
                                    handle_id = texture_handle.0,
                                    width = resolution.width,
                                    height = resolution.height,
                                    bit_depth = %decoded_contract.bit_depth,
                                    chroma = %decoded_contract.chroma,
                                    "P010 zero-copy boundary verified"
                                );
                            }
                            self.zero_copy_guards.insert(texture_handle.0, handle);
                            self.push_ready_frame(
                                DecodedFrame {
                                    pts: Duration::from_micros(timestamp),
                                    format: decoded_contract.format,
                                    bit_depth: decoded_contract.bit_depth,
                                    chroma: decoded_contract.chroma,
                                    memory_path: FrameMemoryPath::DmaBufZeroCopy,
                                    width: resolution.width,
                                    height: resolution.height,
                                    render_width: display_resolution.width,
                                    render_height: display_resolution.height,
                                    color: VideoColorMetadata::sdr_bt709_limited(),
                                    texture_handle,
                                },
                                ReadyFrameSource::ZeroCopy,
                            );
                            return Ok(());
                        }
                        Err(import_error) => {
                            if decoded_contract.format == DecodedPixelFormat::P010 {
                                let import_error_chain = format!("{:#}", import_error);
                                warn!(
                                    error = %import_error_chain,
                                    "P010 DMA-BUF zero-copy import failed; CPU fallback is disabled for P010"
                                );
                                return Err(p010_boundary_violation(format!(
                                    "P010 DMA-BUF zero-copy import failed: {}",
                                    import_error_chain
                                )));
                            }
                            if self.zero_copy_failure_logged {
                                debug!(
                                    error = %import_error,
                                    "Zero-copy DMA-BUF import failed — falling back to VA image readback"
                                );
                            } else {
                                self.zero_copy_failure_logged = true;
                                warn!(
                                    error = %import_error,
                                    "Zero-copy DMA-BUF import failed — falling back to VA image readback"
                                );
                            }
                        }
                    }
                }
                Ok(None) => {
                    if decoded_contract.format == DecodedPixelFormat::P010 {
                        warn!(
                            "P010 decoded handle does not expose DMA-BUF export; CPU fallback is disabled for P010"
                        );
                        return Err(p010_boundary_violation(
                            "P010 decoded handle does not expose DMA-BUF export",
                        ));
                    }
                    debug!("Decoded handle does not expose DMA-BUF export");
                }
                Err(export_error) => {
                    if decoded_contract.format == DecodedPixelFormat::P010 {
                        let export_error_chain = format!("{:#}", export_error);
                        warn!(
                            error = %export_error_chain,
                            "P010 VA surface DMA-BUF export failed; CPU fallback is disabled for P010"
                        );
                        return Err(p010_boundary_violation(format!(
                            "P010 VA surface DMA-BUF export failed: {}",
                            export_error_chain
                        )));
                    }
                    if self.zero_copy_failure_logged {
                        debug!(
                            error = %export_error,
                            "VA surface DMA-BUF export failed — falling back to VA image readback"
                        );
                    } else {
                        self.zero_copy_failure_logged = true;
                        warn!(
                            error = %export_error,
                            "VA surface DMA-BUF export failed — falling back to VA image readback"
                        );
                    }
                }
            }
        } else if let Err(error) =
            ensure_zero_copy_importer_for_contract(decoded_contract, can_import_dma_buf)
        {
            warn!(
                "P010 decoded frame requires DMA-BUF zero-copy importer; CPU fallback is disabled for P010"
            );
            return Err(error);
        }

        // Шаг 3: Загружаем кадр через стабильный VA image readback.
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

        // Шаг 4: Возвращаем backing frame в пул для повторного использования.
        //
        // `handle.video_frame()` возвращает `Arc<InternalVaapiFrame>` (clone).
        // Чтобы `Arc::try_unwrap` сработал, нужно убедиться что refcount == 1.
        // Handle внутри держит `backing_frame: Arc<InternalVaapiFrame>`,
        // так что после clone refcount >= 2. Drop handle уменьшает refcount.
        // Если decoder не держит этот кадр как reference frame, refcount станет 1
        // и `Arc::try_unwrap` вернёт Ok — frame возвращается в пул.
        self.return_frame_from_handle(handle);

        // Шаг 5: Добавляем кадр в очередь готовых к отображению.
        self.push_ready_frame(
            DecodedFrame {
                pts: Duration::from_micros(timestamp),
                format: decoded_contract.format,
                bit_depth: decoded_contract.bit_depth,
                chroma: decoded_contract.chroma,
                memory_path: FrameMemoryPath::CpuUpload,
                width: resolution.width,
                height: resolution.height,
                render_width: display_resolution.width,
                render_height: display_resolution.height,
                color: VideoColorMetadata::sdr_bt709_limited(),
                texture_handle,
            },
            ReadyFrameSource::CpuUpload,
        );

        Ok(())
    }

    /// Обрабатывает все pending events из `cros-codecs`.
    ///
    /// `FrameReady` превращается в `DecodedFrame` и кладётся в `ready_queue`.
    /// `FormatChanged` инвалидирует старые textures и пересоздаёт frame pool
    /// под новое coded resolution/decoded format.
    fn drain_decoder_events(&mut self) -> Result<DecoderDrainReport> {
        let mut report = DecoderDrainReport::default();

        while let Some(event) = self.inner.next_event() {
            report.events_count += 1;
            match event {
                DecoderEvent::FrameReady(handle) => {
                    let pts_ms = handle.timestamp() / 1000;
                    trace!(pts_ms = pts_ms, "DecoderEvent::FrameReady");
                    if let Err(error) = self.process_ready_frame(handle) {
                        if is_fatal_decoder_error(&error) {
                            return Err(error);
                        }
                        warn!(error = %error, "Failed to process ready frame");
                    }
                }
                DecoderEvent::FormatChanged => {
                    report.format_changed = true;
                    info!("Format changed, invalidating texture cache and frame pool");

                    // Очищаем ready_queue: после invalidate_all() старые texture handles
                    // больше нельзя безопасно отдавать renderer-у.
                    let dropped = self.ready_queue.len();
                    if dropped > 0 {
                        info!(dropped, "Clearing ready_queue after FormatChanged");
                        self.ready_queue.clear();
                    }
                    self.texture_cache.lock().unwrap().invalidate_all();

                    // Пересоздаём frame pool под новое разрешение/формат.
                    // `stream_info()` уже обновлён внутри cros-codecs перед event-ом.
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
                        if let Err(error) = self.frame_pool.resize_with_rt_format(
                            res.width,
                            res.height,
                            FRAME_POOL_SIZE,
                            rt_format,
                        ) {
                            warn!(
                                error = %error,
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

        Ok(report)
    }
}

impl DecoderRetryDriver for VaapiVideoDecoder {
    /// Отправляет packet в реальный VA-API decoder.
    fn submit_packet(
        &mut self,
        timestamp_us: u64,
        packet_data: &[u8],
    ) -> std::result::Result<usize, DecodeError> {
        // Декодер вызывает callback, когда ему нужен новый выходной VA surface.
        // `alloc_or_allocate` сохраняет forward progress, если reference frames
        // временно удерживают больше surfaces, чем ожидалось.
        let frame_pool = &mut self.frame_pool;
        let mut alloc_cb = || {
            let frame = frame_pool.alloc_or_allocate();
            if frame.is_none() {
                warn!("Frame pool exhausted — decoder needs more output buffers");
            }
            frame
        };

        self.inner.decode(timestamp_us, packet_data, &mut alloc_cb)
    }

    /// Обрабатывает pending events реального decoder-а.
    fn drain_events(&mut self) -> Result<DecoderDrainReport> {
        self.drain_decoder_events()
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
            pts_ms = packet.pts.as_millis(),
            keyframe = packet.keyframe,
            data_len = packet.data.len(),
            "decode() called"
        );

        // Шаг 1-2: submit packet и drain pending events.
        // При `CheckEvents` тот же packet отправляется повторно после drain,
        // потому что нижний decoder ещё не обязан был consume-ить bitstream.
        let loop_report =
            run_decode_with_event_retry(self, timestamp_us, &packet.data, packet.keyframe)?;
        if loop_report.skipped_packet {
            return Ok(None);
        }

        // Шаг 3: Возвращаем самый старый готовый кадр (FIFO).
        let result = self.ready_queue.pop_front();
        let decode_elapsed = decode_start.elapsed().as_millis();
        let submit_elapsed = loop_report.submit_elapsed.as_millis();
        let drain_elapsed = loop_report.drain_elapsed.as_millis();
        if let Some(ref frame) = result {
            debug!(
                pts_ms = frame.pts.as_millis(),
                width = frame.width,
                height = frame.height,
                decode_attempts = loop_report.attempts,
                processed_bytes = loop_report.processed_bytes,
                events = loop_report.events_count,
                format_changed = loop_report.format_changed,
                submit_ms = submit_elapsed,
                drain_ms = drain_elapsed,
                decode_ms = decode_elapsed,
                "decode() returning frame"
            );
        } else if loop_report.events_count > 0 {
            debug!(
                decode_attempts = loop_report.attempts,
                processed_bytes = loop_report.processed_bytes,
                events = loop_report.events_count,
                format_changed = loop_report.format_changed,
                submit_ms = submit_elapsed,
                drain_ms = drain_elapsed,
                decode_ms = decode_elapsed,
                "decode() completed: no frame ready"
            );
        } else {
            trace!(
                decode_attempts = loop_report.attempts,
                processed_bytes = loop_report.processed_bytes,
                submit_ms = submit_elapsed,
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
        let guards = std::mem::take(&mut self.zero_copy_guards);
        for (_, handle) in guards {
            self.return_frame_from_handle(handle);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only event type для проверки retry state machine без реальной VA-API.
    enum FakeDecoderEvent {
        /// Имитирует `DecoderEvent::FormatChanged`.
        FormatChanged,

        /// Имитирует `DecoderEvent::FrameReady` с исходным PTS.
        FrameReady { pts: Duration },
    }

    /// Минимальный fake-драйвер для `CheckEvents -> drain -> retry same packet`.
    struct FakeRetryDriver {
        /// Заранее заданные ответы fake `decode()`.
        decode_results: VecDeque<std::result::Result<usize, DecodeError>>,

        /// Пакеты событий, которые вернёт каждый fake drain.
        drain_batches: VecDeque<Vec<FakeDecoderEvent>>,

        /// История submit-ов: timestamp и копия bitstream-а.
        submissions: Vec<(u64, Vec<u8>)>,

        /// Очередь fake ready frames, по которой проверяем сохранение PTS.
        ready_pts: VecDeque<Duration>,
    }

    impl FakeRetryDriver {
        /// Создаёт fake-драйвер с управляемыми decode/drain шагами.
        fn new(
            decode_results: Vec<std::result::Result<usize, DecodeError>>,
            drain_batches: Vec<Vec<FakeDecoderEvent>>,
        ) -> Self {
            Self {
                decode_results: VecDeque::from(decode_results),
                drain_batches: VecDeque::from(drain_batches),
                submissions: Vec::new(),
                ready_pts: VecDeque::new(),
            }
        }
    }

    impl DecoderRetryDriver for FakeRetryDriver {
        /// Записывает submit и возвращает следующий заранее заданный результат.
        fn submit_packet(
            &mut self,
            timestamp_us: u64,
            packet_data: &[u8],
        ) -> std::result::Result<usize, DecodeError> {
            self.submissions.push((timestamp_us, packet_data.to_vec()));
            self.decode_results
                .pop_front()
                .expect("fake decode result must be provided")
        }

        /// Обрабатывает один пакет fake events.
        fn drain_events(&mut self) -> Result<DecoderDrainReport> {
            let mut report = DecoderDrainReport::default();
            let Some(events) = self.drain_batches.pop_front() else {
                return Ok(report);
            };

            for event in events {
                report.events_count += 1;
                match event {
                    FakeDecoderEvent::FormatChanged => {
                        report.format_changed = true;
                    }
                    FakeDecoderEvent::FrameReady { pts } => {
                        self.ready_pts.push_back(pts);
                    }
                }
            }

            Ok(report)
        }
    }

    /// Проверяет, что `CheckEvents` не теряет стартовый packet после `FormatChanged`.
    #[test]
    fn check_events_format_change_retries_same_packet_and_preserves_pts() {
        let packet_data = vec![0x82, 0x49, 0x83, 0x42];
        let mut driver = FakeRetryDriver::new(
            vec![Err(DecodeError::CheckEvents), Ok(packet_data.len())],
            vec![
                vec![FakeDecoderEvent::FormatChanged],
                vec![FakeDecoderEvent::FrameReady {
                    pts: Duration::ZERO,
                }],
            ],
        );

        let report = run_decode_with_event_retry(&mut driver, 0, &packet_data, true).unwrap();

        assert_eq!(report.attempts, 2);
        assert_eq!(report.events_count, 2);
        assert!(report.format_changed);
        assert!(!report.skipped_packet);
        assert_eq!(
            driver.submissions,
            vec![(0, packet_data.clone()), (0, packet_data)]
        );
        assert_eq!(driver.ready_pts.pop_front(), Some(Duration::ZERO));
    }

    /// Проверяет, что VA 10-bit 4:2:0 surface становится P010 boundary contract.
    #[test]
    fn va_yuv420_10_rt_format_maps_to_p010_decoded_contract() {
        let contract = decoded_contract_for_rt_format(VA_RT_FORMAT_YUV420_10).unwrap();

        assert_eq!(contract.format, DecodedPixelFormat::P010);
        assert_eq!(contract.bit_depth, BitDepth::Ten);
        assert_eq!(contract.chroma, ChromaSubsampling::Yuv420);
    }

    /// Проверяет `cros-codecs` I010 alias, который VA-API отдаёт для P010 FourCC.
    #[test]
    fn i010_stream_format_maps_to_p010_decoded_contract() {
        let contract = decoded_contract_for_stream_format(DecodedFormat::I010).unwrap();

        assert_eq!(contract.format, DecodedPixelFormat::P010);
        assert_eq!(contract.bit_depth, BitDepth::Ten);
        assert_eq!(contract.chroma, ChromaSubsampling::Yuv420);
    }

    /// Проверяет, что 12-bit VA format не маскируется под P010.
    #[test]
    fn va_yuv420_12_rt_format_is_not_p010_contract() {
        let error = decoded_contract_for_rt_format(VA_RT_FORMAT_YUV420_12).unwrap_err();

        assert!(
            error.to_string().contains("Unsupported VA RT format"),
            "unexpected error: {error}"
        );
    }

    /// Проверяет, что P010 boundary не уходит в CPU fallback без importer-а.
    #[test]
    fn p010_boundary_rejects_missing_zero_copy_importer() {
        let error = ensure_zero_copy_importer_for_contract(DecodedSurfaceContract::p010(), false)
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("requires DMA-BUF zero-copy importer"),
            "unexpected error: {error}"
        );
    }

    /// Проверяет, что P010 без zero-copy считается фатальной ошибкой decoder thread.
    #[test]
    fn p010_boundary_missing_importer_is_fatal_decoder_error() {
        let error = ensure_zero_copy_importer_for_contract(DecodedSurfaceContract::p010(), false)
            .unwrap_err();

        assert!(is_fatal_decoder_error(&error));
    }

    /// Проверяет, что NV12 не требует zero-copy importer-а и может остаться на CPU upload path.
    #[test]
    fn nv12_boundary_allows_missing_zero_copy_importer() {
        ensure_zero_copy_importer_for_contract(DecodedSurfaceContract::nv12(), false).unwrap();
    }
}
