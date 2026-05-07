/// Dedicated decoder thread для VA-API VP9 decode.
///
/// Изолирует blocking decode + DMA map + texture upload от render thread,
/// чтобы UI не зависал на 300–500 мс на кадр.
///
/// Архитектура:
/// - Render thread отправляет video packets через `send_packet()`.
/// - Decoder thread вызывает `decode()`, обрабатывает `FrameReady`, upload'ит текстуры.
/// - Готовые `DecodedFrame` возвращаются через `try_recv_frame()`.
/// - Texture pool (Arc<Mutex<WgpuTexturePool>>) shared между потоками:
///   decoder thread делает upload (write),
///   render thread делает get_views / release (read/write).
use std::sync::{Arc, Mutex};
use std::time::Duration;

use media_core::{Packet, TrackId, TrackKind};
use tracing::{info, trace};
use video_core::{DecodedFrame, VideoDecoder};

use crate::texture_cache::TexturePoolStats;
use crate::upload_config::UploadConfig;

/// Команда для decoder thread.
pub enum ThreadMsg {
    /// Декодировать один VP9 packet.
    Packet(DecodePacket),

    /// Освободить decoded handle, удерживаемый zero-copy кадром.
    ReleaseZeroCopy(video_core::FrameTextureHandle),

    /// Сбросить decoder state и подтвердить завершение операции.
    Flush(std::sync::mpsc::Sender<()>),
}

/// Сырые данные видео-пакета для передачи в decoder thread.
pub struct DecodePacket {
    pub track_id: TrackId,
    pub pts: Duration,
    pub data: Vec<u8>,
    pub keyframe: bool,
}

/// Управляющая структура decoder thread.
///
/// Владеет sender/reciever каналов. Сама decoder thread запущена в фоне.
pub struct VideoDecodeThread {
    msg_tx: std::sync::mpsc::Sender<ThreadMsg>,
    frame_rx: std::sync::mpsc::Receiver<DecodedFrame>,
    queue: Arc<wgpu::Queue>,
    texture_pool: Arc<Mutex<crate::texture_cache::WgpuTexturePool>>,
    backend_name: &'static str,
}

impl VideoDecodeThread {
    /// Создаёт decoder thread с VA-API VP9 decoder.
    ///
    /// # Аргументы
    /// * `device` — wgpu device для создания текстур.
    /// * `queue` — wgpu queue для загрузки данных в текстуры.
    /// * `instance` — wgpu instance (нужна для zero-copy Vulkan DMA-BUF import).
    /// * `adapter` — wgpu adapter (нужна для zero-copy Vulkan DMA-BUF import).
    ///
    /// # Ошибки
    /// Возвращает ошибку если не удалось создать VA-API decoder внутри потока.
    pub fn new(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
    ) -> anyhow::Result<Self> {
        let upload_config = UploadConfig::from_env();
        let dma_buf_importer = if upload_config.enable_dma_buf_zero_copy {
            info!(
                env_var = UploadConfig::ZERO_COPY_ENV_VAR,
                "DMA-BUF zero-copy upload enabled by default"
            );
            Some(crate::dma_buf_import::DmaBufImporter::new(
                (*device).clone(),
                instance,
                adapter,
            ))
        } else {
            info!(
                env_var = UploadConfig::ZERO_COPY_ENV_VAR,
                "Using CPU texture upload because DMA-BUF zero-copy was explicitly disabled"
            );
            None
        };
        let texture_pool = Arc::new(Mutex::new(crate::texture_cache::WgpuTexturePool::new(
            device.clone(),
            dma_buf_importer,
        )));
        let texture_pool_for_thread = texture_pool.clone();
        let queue_for_release_callbacks = queue.clone();

        let (msg_tx, msg_rx) = std::sync::mpsc::channel::<ThreadMsg>();
        let (frame_tx, frame_rx) = std::sync::mpsc::channel::<DecodedFrame>();

        std::thread::Builder::new()
            .name("video-decode".into())
            .spawn(move || {
                info!("Decoder thread started");

                let decoder = match crate::VaapiVideoDecoder::new_with_pool(
                    device,
                    queue,
                    texture_pool_for_thread,
                ) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::error!("Decoder thread: failed to create VA-API decoder: {}", e);
                        return;
                    }
                };

                decoder_thread_loop(decoder, msg_rx, frame_tx);
                info!("Decoder thread exiting");
            })
            .map_err(|e| anyhow::anyhow!("Failed to spawn decoder thread: {}", e))?;

        Ok(Self {
            msg_tx,
            frame_rx,
            queue: queue_for_release_callbacks,
            texture_pool,
            backend_name: "VA-API VP9",
        })
    }

    /// Отправляет video packet в decoder thread.
    pub fn send_packet(&self, packet: DecodePacket) -> anyhow::Result<()> {
        self.msg_tx
            .send(ThreadMsg::Packet(packet))
            .map_err(|_| anyhow::anyhow!("Decoder thread disconnected"))
    }

    /// Освобождает texture slot (вызывается из render thread).
    ///
    /// Релиз выполняется синхронно через shared `Arc<Mutex<WgpuTexturePool>>`,
    /// без отправки сообщения в decoder thread. Это критично для reuse слотов:
    /// если релиз был бы async (через channel), decoder thread мог бы
    /// обработать новый Packet ДО Release, создав новый слот вместо reuse.
    pub fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        trace!(handle_id = handle.0, "Releasing texture slot directly");
        let retired_slot = match self.texture_pool.lock() {
            Ok(mut texture_pool) => {
                let retired_slot = texture_pool.release_slot(handle);
                if retired_slot.is_some() {
                    trace!(
                        handle_id = handle.0,
                        "Zero-copy frame retired until submitted GPU work completes"
                    );
                }
                retired_slot
            }
            Err(error) => {
                tracing::warn!(error = %error, "Texture pool mutex poisoned during release");
                None
            }
        };

        let Some(retired_slot) = retired_slot else {
            return;
        };

        let msg_tx = self.msg_tx.clone();
        self.queue.on_submitted_work_done(move || {
            let ready_handle = retired_slot.frame_handle;
            drop(retired_slot);
            trace!(
                handle_id = ready_handle.0,
                "Submitted GPU work completed; releasing zero-copy VA handle"
            );
            if let Err(error) = msg_tx.send(ThreadMsg::ReleaseZeroCopy(ready_handle)) {
                tracing::warn!(
                    error = %error,
                    handle_id = ready_handle.0,
                    "Failed to send zero-copy release to decoder thread"
                );
            }
        });
    }

    /// Забирает готовый decoded frame из очереди (неблокирующий).
    pub fn try_recv_frame(&self) -> Option<DecodedFrame> {
        self.frame_rx.try_recv().ok()
    }

    /// Синхронно сбрасывает decoder thread и освобождает уже полученные кадры.
    pub fn flush(&self) -> anyhow::Result<()> {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        self.msg_tx
            .send(ThreadMsg::Flush(done_tx))
            .map_err(|_| anyhow::anyhow!("Decoder thread disconnected"))?;
        done_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("Decoder thread did not confirm flush"))?;

        while let Ok(frame) = self.frame_rx.try_recv() {
            self.release_frame(frame.texture_handle);
        }

        Ok(())
    }

    /// Возвращает Y/UV texture views для frame handle (вызывается из render thread).
    pub fn get_views(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> Option<(wgpu::TextureView, wgpu::TextureView)> {
        match self.texture_pool.lock() {
            Ok(texture_pool) => texture_pool.get_views(handle),
            Err(error) => {
                tracing::warn!(error = %error, "Texture pool mutex poisoned during get_views");
                None
            }
        }
    }

    /// Возвращает состояние texture pool для backpressure и UI.
    pub fn texture_pool_stats(&self) -> Option<TexturePoolStats> {
        match self.texture_pool.lock() {
            Ok(texture_pool) => Some(texture_pool.stats()),
            Err(error) => {
                tracing::warn!(error = %error, "Texture pool mutex poisoned during stats read");
                None
            }
        }
    }

    /// Имя бэкенда для UI.
    pub fn backend_name(&self) -> &'static str {
        self.backend_name
    }
}

/// Главный цикл decoder thread.
fn decoder_thread_loop(
    mut decoder: crate::VaapiVideoDecoder,
    msg_rx: std::sync::mpsc::Receiver<ThreadMsg>,
    frame_tx: std::sync::mpsc::Sender<DecodedFrame>,
) {
    while let Ok(msg) = msg_rx.recv() {
        match msg {
            ThreadMsg::Packet(packet) => {
                let pkt = Packet {
                    track_id: packet.track_id,
                    kind: TrackKind::Video,
                    pts: packet.pts,
                    dts: None,
                    keyframe: packet.keyframe,
                    data: bytes::Bytes::copy_from_slice(&packet.data),
                };

                match decoder.decode(&pkt) {
                    Ok(Some(frame)) => {
                        if frame_tx.send(frame).is_err() {
                            trace!("Render thread dropped — exiting decoder loop");
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "Decoder thread: decode error");
                    }
                }
            }
            ThreadMsg::ReleaseZeroCopy(handle) => {
                decoder.release_zero_copy_frame(handle);
            }
            ThreadMsg::Flush(done_tx) => {
                if let Err(error) = decoder.flush() {
                    tracing::warn!(error = %error, "Decoder thread: flush failed");
                }
                let _ = done_tx.send(());
            }
        }
    }
}
