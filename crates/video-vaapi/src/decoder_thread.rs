/// Dedicated decoder thread для VA-API VP9 decode.
///
/// Изолирует blocking hardware decode и DMA-BUF export/import от render thread.
///
/// Архитектура:
/// - Render thread отправляет video packets через `send_packet()`.
/// - Decoder thread вызывает `decode()` и обрабатывает `FrameReady` только через zero-copy import.
/// - Готовые `DecodedFrame` возвращаются через `try_recv_frame()`.
/// - Texture pool (Arc<Mutex<WgpuTexturePool>>) shared между потоками:
///   decoder thread публикует imported DMA-BUF slots,
///   render thread делает get_views / release (read/write).
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use codec_core::VideoColorMetadata;
use media_core::{Packet, TrackId, TrackKind};
use tracing::{info, trace};
use video_core::{DecodedFrame, VideoDecoder};

use crate::texture_cache::TexturePoolStats;

/// Результат, которым decoder thread подтверждает завершение flush.
type FlushAck = std::result::Result<(), String>;

/// Ошибка decoder thread, которую нужно показать player layer как fatal runtime state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeThreadError {
    /// Человекочитаемая причина остановки decoder thread.
    message: String,
}

impl DecodeThreadError {
    /// Создаёт ошибку decoder thread без привязки к backend-specific типам.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Возвращает текст ошибки для player-core/UI.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for DecodeThreadError {
    /// Печатает только полезный текст ошибки без Debug-шума.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DecodeThreadError {}

/// Runtime-policy для decoder thread.
#[derive(Debug, Clone, Copy)]
struct DecodeThreadConfig {
    /// Максимальное время ожидания подтверждения flush от decoder thread.
    flush_timeout: Duration,
}

impl DecodeThreadConfig {
    /// Env-переменная для настройки timeout-а без перекомпиляции приложения.
    const FLUSH_TIMEOUT_ENV_VAR: &'static str = "VIDEOPLAYER_DECODER_FLUSH_TIMEOUT_MS";

    /// Production default: достаточно длинный для нормального VA flush, но не вечный.
    const DEFAULT_FLUSH_TIMEOUT_MS: u64 = 2_000;

    /// Загружает policy из окружения и явно логирует некорректный конфиг.
    fn from_env() -> Self {
        let flush_timeout = match std::env::var(Self::FLUSH_TIMEOUT_ENV_VAR) {
            Ok(raw_value) => match Self::parse_flush_timeout(&raw_value) {
                Ok(timeout) => timeout,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        env_var = Self::FLUSH_TIMEOUT_ENV_VAR,
                        default_timeout_ms = Self::DEFAULT_FLUSH_TIMEOUT_MS,
                        "Invalid decoder flush timeout config; using default"
                    );
                    Self::default_flush_timeout()
                }
            },
            Err(std::env::VarError::NotPresent) => Self::default_flush_timeout(),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    env_var = Self::FLUSH_TIMEOUT_ENV_VAR,
                    default_timeout_ms = Self::DEFAULT_FLUSH_TIMEOUT_MS,
                    "Cannot read decoder flush timeout config; using default"
                );
                Self::default_flush_timeout()
            }
        };

        Self { flush_timeout }
    }

    /// Возвращает default timeout как `Duration`.
    fn default_flush_timeout() -> Duration {
        Duration::from_millis(Self::DEFAULT_FLUSH_TIMEOUT_MS)
    }

    /// Парсит значение env-переменной в миллисекундах.
    fn parse_flush_timeout(raw_value: &str) -> anyhow::Result<Duration> {
        let timeout_ms = raw_value.trim().parse::<u64>().map_err(|error| {
            anyhow::anyhow!(
                "expected positive integer milliseconds, got {:?}: {}",
                raw_value,
                error
            )
        })?;
        if timeout_ms == 0 {
            anyhow::bail!("decoder flush timeout must be greater than 0 ms");
        }
        Ok(Duration::from_millis(timeout_ms))
    }
}

/// Shared fail-closed состояние decoder thread.
#[derive(Clone, Debug)]
struct DecoderThreadState {
    /// Mutex защищает sticky fatal error и флаг одноразовой доставки в player layer.
    inner: Arc<Mutex<DecoderThreadStateInner>>,
}

#[derive(Debug, Default)]
struct DecoderThreadStateInner {
    /// Первая fatal ошибка: последующие причины не перетирают root cause.
    fatal_error: Option<DecodeThreadError>,
    /// Нужно ли ещё отдать fatal error через public `try_recv_error()`.
    pending_notification: bool,
}

impl DecoderThreadState {
    /// Создаёт чистое состояние без fatal ошибки.
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(DecoderThreadStateInner::default())),
        }
    }

    /// Сохраняет первую fatal ошибку и возвращает именно сохранённый root cause.
    fn mark_fatal(&self, error: DecodeThreadError) -> DecodeThreadError {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if let Some(existing_error) = &inner.fatal_error {
            return existing_error.clone();
        }

        inner.fatal_error = Some(error.clone());
        inner.pending_notification = true;
        error
    }

    /// Возвращает текущую fatal ошибку, если decoder thread уже fail-closed.
    fn current_error(&self) -> Option<DecodeThreadError> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.fatal_error.clone()
    }

    /// Отдаёт fatal ошибку в player layer ровно один раз.
    fn take_pending_error(&self) -> Option<DecodeThreadError> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if !inner.pending_notification {
            return None;
        }

        inner.pending_notification = false;
        inner.fatal_error.clone()
    }
}

/// Команда для decoder thread.
enum ThreadMsg {
    /// Декодировать один VP9 packet.
    Packet(DecodePacket),

    /// Освободить decoded handle, удерживаемый zero-copy кадром.
    ReleaseZeroCopy(video_core::FrameTextureHandle),

    /// Сбросить decoder state и подтвердить завершение операции.
    Flush(mpsc::Sender<FlushAck>),
}

/// Пара WGPU texture views для decoded YUV/P010 кадра.
pub struct VideoTextureViews {
    /// Texture view с luma/Y plane.
    pub y_view: wgpu::TextureView,

    /// Texture view с chroma/UV plane.
    pub uv_view: wgpu::TextureView,
}

/// Узкий render-side provider для доступа к texture views по opaque frame handle.
///
/// Provider не раскрывает decoder internals и не копирует pixels: render thread
/// получает views из уже импортированного или загруженного texture pool.
#[derive(Clone)]
pub struct VideoTextureViewProvider {
    /// Канал decoder thread для release zero-copy VA handles после GPU fence.
    msg_tx: std::sync::mpsc::Sender<ThreadMsg>,

    /// WGPU queue нужен только для callback-а завершения уже отправленной GPU work.
    queue: Arc<wgpu::Queue>,

    /// Shared texture pool, из которого render thread создаёт WGPU views.
    texture_pool: Arc<Mutex<crate::texture_cache::WgpuTexturePool>>,

    /// Shared fatal state, чтобы release callback мог сообщить о disconnect-е.
    thread_state: DecoderThreadState,
}

impl VideoTextureViewProvider {
    /// Получает Y/UV views для frame handle на render thread.
    #[must_use]
    pub fn texture_views(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> Option<VideoTextureViews> {
        match self.texture_pool.lock() {
            Ok(texture_pool) => texture_pool
                .get_views(handle)
                .map(|(y_view, uv_view)| VideoTextureViews { y_view, uv_view }),
            Err(error) => {
                tracing::warn!(error = %error, "Texture pool mutex poisoned during get_views");
                None
            }
        }
    }

    /// Освобождает texture slot через тот decoder/texture pool, который создал кадр.
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
        let thread_state = self.thread_state.clone();
        self.queue.on_submitted_work_done(move || {
            let ready_handle = retired_slot.frame_handle;
            drop(retired_slot);
            trace!(
                handle_id = ready_handle.0,
                "Submitted GPU work completed; releasing zero-copy VA handle"
            );
            if let Err(error) = msg_tx.send(ThreadMsg::ReleaseZeroCopy(ready_handle)) {
                let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(
                    "Decoder thread disconnected before zero-copy release",
                ));
                tracing::warn!(
                    error = %error,
                    fatal = %fatal_error,
                    handle_id = ready_handle.0,
                    "Failed to send zero-copy release to decoder thread"
                );
            }
        });
    }
}

/// Сырые данные видео-пакета для передачи в decoder thread.
pub struct DecodePacket {
    /// Track ID выбранного video stream.
    pub track_id: TrackId,

    /// Presentation timestamp packet-а.
    pub pts: Duration,

    /// Encoded VP9 bytes, которые decoder thread передаёт hardware backend-у без повторной копии.
    pub encoded_bytes: Bytes,

    /// Keyframe flag из container/demuxer.
    pub keyframe: bool,

    /// Resolved color metadata из player/capability layer для decoded frame contract.
    pub resolved_color: Option<VideoColorMetadata>,
}

/// Управляющая структура decoder thread.
///
/// Владеет sender/reciever каналов. Сама decoder thread запущена в фоне.
pub struct VideoDecodeThread {
    msg_tx: std::sync::mpsc::Sender<ThreadMsg>,
    frame_rx: std::sync::mpsc::Receiver<DecodedFrame>,
    error_rx: std::sync::mpsc::Receiver<DecodeThreadError>,
    queue: Arc<wgpu::Queue>,
    texture_pool: Arc<Mutex<crate::texture_cache::WgpuTexturePool>>,
    thread_state: DecoderThreadState,
    config: DecodeThreadConfig,
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
        let config = DecodeThreadConfig::from_env();
        let dma_buf_importer = Some(crate::dma_buf_import::DmaBufImporter::new(
            (*device).clone(),
            instance,
            adapter,
        ));
        info!("DMA-BUF zero-copy import is required by production video policy");
        let texture_pool = Arc::new(Mutex::new(crate::texture_cache::WgpuTexturePool::new(
            dma_buf_importer,
        )));
        let texture_pool_for_thread = texture_pool.clone();
        let queue_for_release_callbacks = queue.clone();

        let (msg_tx, msg_rx) = std::sync::mpsc::channel::<ThreadMsg>();
        let (frame_tx, frame_rx) = std::sync::mpsc::channel::<DecodedFrame>();
        let (error_tx, error_rx) = std::sync::mpsc::channel::<DecodeThreadError>();
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<anyhow::Result<()>>(1);
        let thread_state = DecoderThreadState::new();

        std::thread::Builder::new()
            .name("video-decode".into())
            .spawn(move || {
                info!("Decoder thread started");

                let decoder = match crate::VaapiVideoDecoder::new_with_pool(
                    device,
                    queue,
                    texture_pool_for_thread,
                ) {
                    Ok(decoder) => {
                        if init_tx.send(Ok(())).is_err() {
                            trace!("Decoder thread init receiver dropped — exiting");
                            return;
                        }
                        decoder
                    }
                    Err(error) => {
                        tracing::error!(
                            error = %error,
                            "Decoder thread failed to create VA-API decoder"
                        );
                        let _ = init_tx.send(Err(
                            error.context("Decoder thread failed to create VA-API decoder")
                        ));
                        return;
                    }
                };

                decoder_thread_loop(decoder, msg_rx, frame_tx, error_tx);
                info!("Decoder thread exiting");
            })
            .map_err(|e| anyhow::anyhow!("Failed to spawn decoder thread: {}", e))?;

        match init_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error),
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "Decoder thread exited before initialization completed: {}",
                    error
                ));
            }
        }

        Ok(Self {
            msg_tx,
            frame_rx,
            error_rx,
            queue: queue_for_release_callbacks,
            texture_pool,
            thread_state,
            config,
            backend_name: "VA-API VP9",
        })
    }

    /// Отправляет video packet в decoder thread.
    pub fn send_packet(&self, packet: DecodePacket) -> anyhow::Result<()> {
        self.ensure_thread_usable()?;
        self.msg_tx.send(ThreadMsg::Packet(packet)).map_err(|_| {
            let fatal_error = self
                .thread_state
                .mark_fatal(DecodeThreadError::new("Decoder thread disconnected"));
            anyhow::anyhow!("{}", fatal_error)
        })
    }

    /// Освобождает texture slot (вызывается из render thread).
    ///
    /// Релиз выполняется синхронно через shared `Arc<Mutex<WgpuTexturePool>>`,
    /// без отправки сообщения в decoder thread. Это критично для reuse слотов:
    /// если релиз был бы async (через channel), decoder thread мог бы
    /// обработать новый Packet ДО Release, создав новый слот вместо reuse.
    pub fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        self.texture_view_provider().release_frame(handle);
    }

    /// Забирает готовый decoded frame из очереди (неблокирующий).
    pub fn try_recv_frame(&self) -> Option<DecodedFrame> {
        self.frame_rx.try_recv().ok()
    }

    /// Забирает fatal error из decoder thread, если backend остановился fail-closed.
    pub fn try_recv_error(&self) -> Option<DecodeThreadError> {
        self.absorb_decoder_thread_errors();
        self.thread_state.take_pending_error()
    }

    /// Синхронно сбрасывает decoder thread и освобождает уже полученные кадры.
    pub fn flush(&self) -> anyhow::Result<()> {
        if let Err(error) = self.ensure_thread_usable() {
            self.release_received_frames();
            return Err(error);
        }

        let (done_tx, done_rx) = mpsc::channel();
        if self.msg_tx.send(ThreadMsg::Flush(done_tx)).is_err() {
            let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(
                "Decoder thread disconnected before flush",
            ));
            self.release_received_frames();
            return Err(anyhow::anyhow!("{}", fatal_error));
        }

        let flush_result =
            wait_for_flush_ack(done_rx, self.config.flush_timeout, &self.thread_state);
        self.release_received_frames();
        flush_result
    }

    /// Возвращает Y/UV texture views для frame handle (вызывается из render thread).
    pub fn get_views(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> Option<(wgpu::TextureView, wgpu::TextureView)> {
        self.texture_view_provider()
            .texture_views(handle)
            .map(|views| (views.y_view, views.uv_view))
    }

    /// Возвращает cloneable provider, который render thread использует для texture views.
    #[must_use]
    pub fn texture_view_provider(&self) -> VideoTextureViewProvider {
        VideoTextureViewProvider {
            msg_tx: self.msg_tx.clone(),
            queue: self.queue.clone(),
            texture_pool: self.texture_pool.clone(),
            thread_state: self.thread_state.clone(),
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

    /// Переносит fatal ошибки из decoder thread channel в sticky state.
    fn absorb_decoder_thread_errors(&self) {
        loop {
            match self.error_rx.try_recv() {
                Ok(error) => {
                    self.thread_state.mark_fatal(error);
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    /// Проверяет, что decoder thread ещё можно использовать для новых команд.
    fn ensure_thread_usable(&self) -> anyhow::Result<()> {
        self.absorb_decoder_thread_errors();
        if let Some(error) = self.thread_state.current_error() {
            return Err(anyhow::anyhow!("{}", error));
        }
        Ok(())
    }

    /// Освобождает кадры, которые уже пришли через frame channel до/во время flush.
    fn release_received_frames(&self) {
        while let Ok(frame) = self.frame_rx.try_recv() {
            self.release_frame(frame.texture_handle);
        }
    }
}

/// Ждёт flush ACK ограниченное время и переводит thread state в fatal при срыве.
fn wait_for_flush_ack(
    done_rx: mpsc::Receiver<FlushAck>,
    timeout: Duration,
    thread_state: &DecoderThreadState,
) -> anyhow::Result<()> {
    match done_rx.recv_timeout(timeout) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(message)) => {
            let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(format!(
                "Decoder thread flush failed: {message}"
            )));
            Err(anyhow::anyhow!("{}", fatal_error))
        }
        Err(RecvTimeoutError::Timeout) => {
            let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(format!(
                "Decoder thread did not confirm flush within {} ms",
                timeout.as_millis()
            )));
            Err(anyhow::anyhow!("{}", fatal_error))
        }
        Err(RecvTimeoutError::Disconnected) => {
            let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(
                "Decoder thread did not confirm flush",
            ));
            Err(anyhow::anyhow!("{}", fatal_error))
        }
    }
}

/// Главный цикл decoder thread.
fn decoder_thread_loop(
    mut decoder: crate::VaapiVideoDecoder,
    msg_rx: std::sync::mpsc::Receiver<ThreadMsg>,
    frame_tx: std::sync::mpsc::Sender<DecodedFrame>,
    error_tx: std::sync::mpsc::Sender<DecodeThreadError>,
) {
    while let Ok(msg) = msg_rx.recv() {
        match msg {
            ThreadMsg::Packet(packet) => {
                let DecodePacket {
                    track_id,
                    pts,
                    encoded_bytes,
                    keyframe,
                    resolved_color,
                } = packet;

                let pkt = Packet {
                    track_id,
                    kind: TrackKind::Video,
                    pts,
                    dts: None,
                    keyframe,
                    byte_offset: None,
                    data: encoded_bytes,
                };

                match decoder.decode(&pkt) {
                    Ok(Some(mut frame)) => {
                        if let Some(resolved_color) = &resolved_color {
                            frame.color = resolved_color.clone();
                        }
                        if frame_tx.send(frame).is_err() {
                            trace!("Render thread dropped — exiting decoder loop");
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        if crate::decoder::is_fatal_decoder_error(&e) {
                            let message = format!("Video decoder stopped after fatal error: {e:#}");
                            tracing::warn!(
                                error = %message,
                                "Decoder thread: fatal decode error, exiting"
                            );
                            if error_tx.send(DecodeThreadError::new(message)).is_err() {
                                trace!("Player thread dropped decoder error receiver");
                            }
                            break;
                        }
                        tracing::warn!(error = %e, "Decoder thread: decode error");
                    }
                }
            }
            ThreadMsg::ReleaseZeroCopy(handle) => {
                decoder.release_zero_copy_frame(handle);
            }
            ThreadMsg::Flush(done_tx) => {
                let flush_result = decoder.flush().map_err(|error| format!("{error:#}"));
                match &flush_result {
                    Ok(()) => {}
                    Err(error) => {
                        let message = format!("Video decoder stopped after flush error: {error}");
                        tracing::warn!(
                            error = %message,
                            "Decoder thread: fatal flush error, exiting"
                        );
                        if error_tx.send(DecodeThreadError::new(message)).is_err() {
                            trace!("Player thread dropped decoder error receiver");
                        }
                    }
                }

                let flush_failed = flush_result.is_err();
                if done_tx.send(flush_result).is_err() {
                    tracing::warn!("Decoder thread: flush completed, but caller dropped receiver");
                }
                if flush_failed {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что public error contract сохраняет причину fatal остановки thread-а.
    #[test]
    fn decode_thread_error_exposes_message_for_player_layer() {
        let error = DecodeThreadError::new("P010 DMA-BUF zero-copy import failed");

        assert_eq!(error.message(), "P010 DMA-BUF zero-copy import failed");
        assert_eq!(error.to_string(), "P010 DMA-BUF zero-copy import failed");
    }

    /// Проверяет parsing policy без изменения process env в параллельных тестах.
    #[test]
    fn flush_timeout_config_rejects_zero_and_non_numeric_values() {
        assert!(DecodeThreadConfig::parse_flush_timeout("0").is_err());
        assert!(DecodeThreadConfig::parse_flush_timeout("abc").is_err());
        assert_eq!(
            DecodeThreadConfig::parse_flush_timeout("25").unwrap(),
            Duration::from_millis(25)
        );
    }

    /// Проверяет, что timeout не блокируется бесконечно и становится fatal state.
    #[test]
    fn flush_ack_timeout_marks_thread_fatal_once() {
        let (_done_tx, done_rx) = mpsc::channel();
        let thread_state = DecoderThreadState::new();

        let error = wait_for_flush_ack(done_rx, Duration::from_millis(1), &thread_state)
            .expect_err("empty ACK channel must timeout");

        assert!(
            error
                .to_string()
                .contains("Decoder thread did not confirm flush within")
        );
        assert!(thread_state.current_error().is_some());
        assert!(thread_state.take_pending_error().is_some());
        assert!(thread_state.take_pending_error().is_none());
    }
}
