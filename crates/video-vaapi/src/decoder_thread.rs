/// Dedicated decoder thread для VA-API VP9 decode.
///
/// Изолирует blocking hardware decode и DMA-BUF export/import от render thread.
///
/// Архитектура:
/// - Render thread отправляет video packets через `send_packet()`.
/// - Decoder thread вызывает `decode()` и обрабатывает `FrameReady` только через zero-copy import.
/// - Готовые `DecodedFrame` возвращаются через `try_recv_frame()`.
/// - Texture pool (Arc<Mutex<WgpuTexturePool>>) shared между потоками:
///   decoder thread публикует или переиспользует persistent DMA-BUF imports,
///   render thread делает get_views и отдаёт release через GPU completion ack.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use codec_core::VideoColorMetadata;
use crossbeam_channel::{
    Receiver, RecvTimeoutError, Sender, TryRecvError, TrySendError, bounded, select, unbounded,
};
use media_core::{Packet, TrackId, TrackKind};
use tracing::{info, trace};
use video_core::{DecodedFrame, VideoDecoder, VideoDecoderDiagnosticEvent};

use crate::decoder::VaapiDecoderRuntimeConfig;
use crate::texture_cache::{DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS, TexturePoolStats};

/// Результат, которым decoder thread подтверждает завершение flush.
type FlushAck = std::result::Result<(), String>;

/// Подтверждение, что decoder thread уже обработал один packet из input channel.
type DecodePacketAck = ();

/// Bounded capacity diagnostics events от decoder thread.
const DECODER_DIAGNOSTIC_CHANNEL_CAPACITY: usize = 256;

/// Production default packet channel между worker и decoder thread.
///
/// 32 packet-а дают decoder thread возможность пережить scene-change burst без
/// unbounded memory growth и без искусственного лимита в 2 packet-а на tick.
pub const DEFAULT_DECODER_PACKET_CHANNEL_FRAMES: usize = 32;

/// Production default decoded frame channel от decoder thread к worker.
///
/// 8 кадров совпадают с текущим target presentation queue и позволяют worker-у
/// принять burst готовых кадров за один tick без скрытой unbounded очереди.
pub const DEFAULT_DECODER_FRAME_CHANNEL_FRAMES: usize = 8;

/// Внутренний control/release channel: release не должен стоять за packet backlog.
const DEFAULT_DECODER_CONTROL_CHANNEL_FRAMES: usize = 32;

/// Небольшой timeout poll-а, пока decoder ждёт место в bounded frame channel.
const DECODER_FRAME_PUBLISH_RETRY_MS: u64 = 2;

/// Runtime limits decoder thread boundary.
///
/// Все очереди bounded: packet queue даёт демux/decode burst headroom, frame
/// queue даёт worker-у принять burst готовых кадров, control queue отделяет
/// release/flush от packet backlog-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoDecodeThreadConfig {
    /// Packet channel capacity между worker и decoder thread.
    pub packet_channel_frames: usize,

    /// Decoded frame channel capacity между decoder thread и worker.
    pub frame_channel_frames: usize,

    /// Control/release channel capacity для release/flush сообщений.
    pub control_channel_frames: usize,

    /// Backend-local ready queue capacity внутри VA-API decoder wrapper.
    pub decoder_ready_queue_frames: usize,

    /// VA output surface descriptor pool size.
    pub decoder_surface_pool_frames: usize,

    /// Zero-copy external import slot capacity.
    pub zero_copy_surface_pool_slots: usize,

    /// Максимальное время ожидания подтверждения flush от decoder thread.
    pub flush_timeout: Duration,
}

impl VideoDecodeThreadConfig {
    /// Env-переменная для настройки flush timeout-а без перекомпиляции приложения.
    const FLUSH_TIMEOUT_ENV_VAR: &'static str = "VIDEOPLAYER_DECODER_FLUSH_TIMEOUT_MS";

    /// Production default: достаточно длинный для нормального VA flush, но не вечный.
    const DEFAULT_FLUSH_TIMEOUT_MS: u64 = 2_000;

    /// Загружает config defaults и overlay локального backend timeout-а из окружения.
    #[must_use]
    pub fn from_env() -> Self {
        let mut config = Self::default();
        config.flush_timeout = match std::env::var(Self::FLUSH_TIMEOUT_ENV_VAR) {
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
        config
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

    /// Нормализует значения для direct API callers; public config validation остаётся выше.
    #[must_use]
    pub fn normalized(self) -> Self {
        Self {
            packet_channel_frames: self.packet_channel_frames.max(1),
            frame_channel_frames: self.frame_channel_frames.max(1),
            control_channel_frames: self.control_channel_frames.max(1),
            decoder_ready_queue_frames: self.decoder_ready_queue_frames.max(1),
            decoder_surface_pool_frames: self.decoder_surface_pool_frames.max(1),
            zero_copy_surface_pool_slots: self.zero_copy_surface_pool_slots.max(1),
            flush_timeout: self.flush_timeout.max(Duration::from_millis(1)),
        }
    }

    /// Возвращает backend-local config, который передаётся VA decoder wrapper-у.
    #[must_use]
    fn vaapi_decoder_config(self) -> VaapiDecoderRuntimeConfig {
        VaapiDecoderRuntimeConfig {
            surface_pool_frames: self.decoder_surface_pool_frames,
            ready_queue_frames: self.decoder_ready_queue_frames,
        }
    }
}

impl Default for VideoDecodeThreadConfig {
    /// Возвращает production defaults без unbounded очередей.
    fn default() -> Self {
        Self {
            packet_channel_frames: DEFAULT_DECODER_PACKET_CHANNEL_FRAMES,
            frame_channel_frames: DEFAULT_DECODER_FRAME_CHANNEL_FRAMES,
            control_channel_frames: DEFAULT_DECODER_CONTROL_CHANNEL_FRAMES,
            decoder_ready_queue_frames: crate::decoder::DEFAULT_DECODER_READY_QUEUE_FRAMES,
            decoder_surface_pool_frames: crate::decoder::DEFAULT_DECODER_SURFACE_POOL_FRAMES,
            zero_copy_surface_pool_slots: DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS,
            flush_timeout: Self::default_flush_timeout(),
        }
    }
}

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

/// Typed причина, по которой decoder thread временно не принимает packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeThreadBackpressureReason {
    /// Bounded packet channel заполнен: decoder ещё не забрал старые packets.
    PacketQueueFull {
        /// Текущая глубина packet channel.
        queued_packets: usize,

        /// Bounded capacity packet channel.
        capacity: usize,
    },
}

impl std::fmt::Display for DecodeThreadBackpressureReason {
    /// Печатает причину backpressure без потери чисел очереди.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PacketQueueFull {
                queued_packets,
                capacity,
            } => write!(
                formatter,
                "decoder packet channel is full: queued={queued_packets}, capacity={capacity}"
            ),
        }
    }
}

/// Ошибка постановки packet-а в decoder thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeThreadSendError {
    /// Decoder thread жив, но bounded queue сейчас заполнена.
    Backpressure(DecodeThreadBackpressureReason),

    /// Decoder thread уже fail-closed или receiver отключён.
    Fatal(DecodeThreadError),
}

impl std::fmt::Display for DecodeThreadSendError {
    /// Печатает machine-actionable причину отправки packet-а.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backpressure(reason) => write!(formatter, "{reason}"),
            Self::Fatal(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for DecodeThreadSendError {}

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

/// Control-команда для decoder thread.
enum ThreadControlMsg {
    /// Освободить decoded handle, удерживаемый zero-copy кадром.
    ReleaseZeroCopy(video_core::FrameTextureHandle),

    /// Сбросить decoder state и подтвердить завершение операции.
    Flush(Sender<FlushAck>),
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
    control_tx: Sender<ThreadControlMsg>,

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

    /// Освобождает renderer-owned frame после submitted GPU work.
    pub fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        trace!(handle_id = handle.0, "Releasing rendered zero-copy frame");
        let gpu_completion_lease = match self.texture_pool.lock() {
            Ok(mut texture_pool) => match texture_pool.release_after_gpu_submission(handle) {
                Ok(gpu_completion_lease) => gpu_completion_lease,
                Err(error) => {
                    let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(
                        format!("Zero-copy surface release lifecycle violation: {error}"),
                    ));
                    tracing::warn!(
                        error = %error,
                        fatal = %fatal_error,
                        handle_id = handle.0,
                        "Failed to move zero-copy surface into GPU wait state"
                    );
                    return;
                }
            },
            Err(error) => {
                let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(format!(
                    "Zero-copy texture pool mutex poisoned during rendered release: {error}"
                )));
                tracing::warn!(
                    error = %error,
                    fatal = %fatal_error,
                    handle_id = handle.0,
                    "Texture pool mutex poisoned during rendered release"
                );
                return;
            }
        };

        let msg_tx = self.control_tx.clone();
        let thread_state = self.thread_state.clone();
        let texture_pool = self.texture_pool.clone();
        self.queue.on_submitted_work_done(move || {
            let ready_handle = gpu_completion_lease.frame_handle();
            match texture_pool.lock() {
                Ok(mut texture_pool) => {
                    if let Err(error) = texture_pool.acknowledge_gpu_completion(ready_handle) {
                        let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(format!(
                            "Zero-copy GPU completion lifecycle violation: {error}"
                        )));
                        tracing::warn!(
                            error = %error,
                            fatal = %fatal_error,
                            handle_id = ready_handle.0,
                            "Failed to acknowledge zero-copy GPU completion"
                        );
                        return;
                    }
                }
                Err(error) => {
                    let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(format!(
                        "Zero-copy texture pool mutex poisoned during GPU completion: {error}"
                    )));
                    tracing::warn!(
                        error = %error,
                        fatal = %fatal_error,
                        handle_id = ready_handle.0,
                        "Texture pool mutex poisoned during GPU completion"
                    );
                    return;
                }
            }
            trace!(
                handle_id = ready_handle.0,
                "Submitted GPU work completed; releasing decoded surface to decoder"
            );
            if let Err(error) = msg_tx.try_send(ThreadControlMsg::ReleaseZeroCopy(ready_handle)) {
                let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(
                    decoder_control_send_error_message("zero-copy release", &error),
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

/// Packet вместе с моментом попадания в bounded decoder channel.
struct QueuedDecodePacket {
    /// Encoded packet payload и metadata.
    packet: DecodePacket,

    /// Монотонный момент successful enqueue.
    enqueued_at: Instant,
}

/// Управляющая структура decoder thread.
///
/// Владеет sender/reciever каналов. Сама decoder thread запущена в фоне.
pub struct VideoDecodeThread {
    packet_tx: Sender<QueuedDecodePacket>,
    control_tx: Sender<ThreadControlMsg>,
    frame_rx: Receiver<DecodedFrame>,
    packet_ack_rx: Receiver<DecodePacketAck>,
    error_rx: Receiver<DecodeThreadError>,
    diagnostic_rx: std::sync::mpsc::Receiver<VideoDecoderDiagnosticEvent>,
    queue: Arc<wgpu::Queue>,
    texture_pool: Arc<Mutex<crate::texture_cache::WgpuTexturePool>>,
    thread_state: DecoderThreadState,
    config: VideoDecodeThreadConfig,
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
        Self::new_with_config(
            device,
            queue,
            instance,
            adapter,
            VideoDecodeThreadConfig::from_env(),
        )
    }

    /// Создаёт decoder thread с явно заданными bounded queue/runtime limits.
    pub fn new_with_config(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        config: VideoDecodeThreadConfig,
    ) -> anyhow::Result<Self> {
        let config = config.normalized();
        let dma_buf_importer = Some(crate::dma_buf_import::DmaBufImporter::new(
            (*device).clone(),
            instance,
            adapter,
        ));
        info!("DMA-BUF zero-copy import is required by production video policy");
        let texture_pool = Arc::new(Mutex::new(
            crate::texture_cache::WgpuTexturePool::new_with_capacity(
                dma_buf_importer,
                config.zero_copy_surface_pool_slots,
            ),
        ));
        let texture_pool_for_thread = texture_pool.clone();
        let queue_for_release_callbacks = queue.clone();

        let (packet_tx, packet_rx) = bounded::<QueuedDecodePacket>(config.packet_channel_frames);
        let (control_tx, control_rx) = bounded::<ThreadControlMsg>(config.control_channel_frames);
        let (frame_tx, frame_rx) = bounded::<DecodedFrame>(config.frame_channel_frames);
        let (packet_ack_tx, packet_ack_rx) = unbounded::<DecodePacketAck>();
        let (error_tx, error_rx) = bounded::<DecodeThreadError>(1);
        let (diagnostic_tx, diagnostic_rx) = std::sync::mpsc::sync_channel::<
            VideoDecoderDiagnosticEvent,
        >(DECODER_DIAGNOSTIC_CHANNEL_CAPACITY);
        let (init_tx, init_rx) = bounded::<anyhow::Result<()>>(1);
        let thread_state = DecoderThreadState::new();
        let decoder_runtime_config = config.vaapi_decoder_config();

        std::thread::Builder::new()
            .name("video-decode".into())
            .spawn(move || {
                info!("Decoder thread started");

                let decoder = match crate::VaapiVideoDecoder::new_with_pool(
                    device,
                    queue,
                    texture_pool_for_thread,
                    Some(diagnostic_tx),
                    decoder_runtime_config,
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

                decoder_thread_loop(
                    decoder,
                    packet_rx,
                    control_rx,
                    frame_tx,
                    packet_ack_tx,
                    error_tx,
                );
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
            packet_tx,
            control_tx,
            frame_rx,
            packet_ack_rx,
            error_rx,
            diagnostic_rx,
            queue: queue_for_release_callbacks,
            texture_pool,
            thread_state,
            config,
            backend_name: "VA-API VP9",
        })
    }

    /// Отправляет video packet в decoder thread.
    pub fn send_packet(&self, packet: DecodePacket) -> Result<(), DecodeThreadSendError> {
        self.ensure_thread_usable()
            .map_err(DecodeThreadSendError::Fatal)?;
        let queued_packet = QueuedDecodePacket {
            packet,
            enqueued_at: Instant::now(),
        };

        self.packet_tx
            .try_send(queued_packet)
            .map_err(|error| match error {
                TrySendError::Full(_) => DecodeThreadSendError::Backpressure(
                    DecodeThreadBackpressureReason::PacketQueueFull {
                        queued_packets: self.packet_tx.len(),
                        capacity: self.packet_tx.capacity().unwrap_or(0),
                    },
                ),
                TrySendError::Disconnected(_) => {
                    let fatal_error = self
                        .thread_state
                        .mark_fatal(DecodeThreadError::new("Decoder thread disconnected"));
                    DecodeThreadSendError::Fatal(fatal_error)
                }
            })
    }

    /// Освобождает frame, который не находится в renderer GPU work.
    ///
    /// Используется для queued/present frames без active render lease. Такой frame
    /// можно вернуть decoder-у сразу: GPU completion уже не требуется.
    pub fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        match self.texture_pool.lock() {
            Ok(mut texture_pool) => {
                if let Err(error) = texture_pool.release_without_gpu_submission(handle) {
                    let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(
                        format!("Zero-copy immediate release lifecycle violation: {error}"),
                    ));
                    tracing::warn!(
                        error = %error,
                        fatal = %fatal_error,
                        handle_id = handle.0,
                        "Failed to move zero-copy surface into decoder reuse state"
                    );
                    return;
                }
            }
            Err(error) => {
                let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(format!(
                    "Zero-copy texture pool mutex poisoned during immediate release: {error}"
                )));
                tracing::warn!(
                    error = %error,
                    fatal = %fatal_error,
                    handle_id = handle.0,
                    "Texture pool mutex poisoned during immediate release"
                );
                return;
            }
        }

        if let Err(error) = self
            .control_tx
            .try_send(ThreadControlMsg::ReleaseZeroCopy(handle))
        {
            let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(
                decoder_control_send_error_message("zero-copy release", &error),
            ));
            tracing::warn!(
                error = %error,
                fatal = %fatal_error,
                handle_id = handle.0,
                "Failed to send immediate zero-copy release to decoder thread"
            );
        }
    }

    /// Забирает готовый decoded frame из очереди (неблокирующий).
    pub fn try_recv_frame(&self) -> Option<DecodedFrame> {
        self.frame_rx.try_recv().ok()
    }

    /// Забирает backend diagnostics event без блокировки.
    pub fn try_recv_diagnostic_event(&self) -> Option<VideoDecoderDiagnosticEvent> {
        self.diagnostic_rx.try_recv().ok()
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
            return Err(anyhow::anyhow!("{}", error));
        }

        let (done_tx, done_rx) = bounded(1);
        if let Err(error) = self.control_tx.try_send(ThreadControlMsg::Flush(done_tx)) {
            self.release_received_frames();
            let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(
                decoder_control_send_error_message("decoder flush", &error),
            ));
            return Err(anyhow::anyhow!("{}", fatal_error));
        }

        let flush_result =
            wait_for_flush_ack(done_rx, self.config.flush_timeout, &self.thread_state);
        self.release_received_frames();
        self.drain_completed_packet_acks();
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
            control_tx: self.control_tx.clone(),
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

    /// Возвращает текущую глубину bounded packet channel.
    #[must_use]
    pub fn packet_queue_depth(&self) -> usize {
        self.packet_tx.len()
    }

    /// Забирает количество packets, которые decoder thread уже обработал.
    #[must_use]
    pub fn drain_completed_packet_count(&self) -> usize {
        self.drain_completed_packet_acks()
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
    fn ensure_thread_usable(&self) -> Result<(), DecodeThreadError> {
        self.absorb_decoder_thread_errors();
        if let Some(error) = self.thread_state.current_error() {
            return Err(error);
        }
        Ok(())
    }

    /// Освобождает кадры, которые уже пришли через frame channel до/во время flush.
    fn release_received_frames(&self) {
        while let Ok(frame) = self.frame_rx.try_recv() {
            self.release_frame(frame.texture_handle);
        }
    }

    /// Очищает packet-ack channel и возвращает число подтверждений.
    fn drain_completed_packet_acks(&self) -> usize {
        let mut completed_packet_count = 0usize;
        while self.packet_ack_rx.try_recv().is_ok() {
            completed_packet_count = completed_packet_count.saturating_add(1);
        }
        completed_packet_count
    }
}

/// Ждёт flush ACK ограниченное время и переводит thread state в fatal при срыве.
fn wait_for_flush_ack(
    done_rx: Receiver<FlushAck>,
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

/// Decoded frame, который уже готов, но ещё ждёт место в bounded frame channel.
struct PendingFramePublish {
    /// Frame metadata и zero-copy texture handle.
    frame: DecodedFrame,

    /// Монотонный момент начала publish stage.
    publish_started_at: Instant,
}

impl PendingFramePublish {
    /// Создаёт pending publish item и начинает измерять decoded-frame publish latency.
    fn new(frame: DecodedFrame) -> Self {
        Self {
            frame,
            publish_started_at: Instant::now(),
        }
    }
}

/// Печатает control-channel failure как fatal lifecycle ошибку.
fn decoder_control_send_error_message<T>(operation: &str, error: &TrySendError<T>) -> String {
    match error {
        TrySendError::Full(_) => format!("Decoder control channel is full before {operation}"),
        TrySendError::Disconnected(_) => {
            format!("Decoder thread disconnected before {operation}")
        }
    }
}

/// Главный цикл decoder thread.
fn decoder_thread_loop(
    mut decoder: crate::VaapiVideoDecoder,
    packet_rx: Receiver<QueuedDecodePacket>,
    control_rx: Receiver<ThreadControlMsg>,
    frame_tx: Sender<DecodedFrame>,
    packet_ack_tx: Sender<DecodePacketAck>,
    error_tx: Sender<DecodeThreadError>,
) {
    let mut pending_publish: Option<PendingFramePublish> = None;
    let mut latest_color_metadata: Option<VideoColorMetadata> = None;

    loop {
        if !drain_decoder_control_messages(
            &mut decoder,
            &packet_rx,
            &control_rx,
            &error_tx,
            &mut pending_publish,
        ) {
            break;
        }

        if !publish_pending_frame(&frame_tx, &mut pending_publish) {
            break;
        }

        if pending_publish.is_some() {
            match control_rx.recv_timeout(Duration::from_millis(DECODER_FRAME_PUBLISH_RETRY_MS)) {
                Ok(control_message) => {
                    if !handle_decoder_control_message(
                        &mut decoder,
                        control_message,
                        &packet_rx,
                        &error_tx,
                        &mut pending_publish,
                    ) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            continue;
        }

        if let Some(mut frame) = decoder.take_ready_frame() {
            if let Some(color_metadata) = &latest_color_metadata {
                frame.color = color_metadata.clone();
            }
            pending_publish = Some(PendingFramePublish::new(frame));
            continue;
        }

        select! {
            recv(control_rx) -> control_result => {
                match control_result {
                    Ok(control_message) => {
                        if !handle_decoder_control_message(
                            &mut decoder,
                            control_message,
                            &packet_rx,
                            &error_tx,
                            &mut pending_publish,
                        ) {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            recv(packet_rx) -> packet_result => {
                match packet_result {
                    Ok(queued_packet) => {
                        if !decode_queued_packet(
                            &mut decoder,
                            queued_packet,
                            &frame_tx,
                            &packet_ack_tx,
                            &error_tx,
                            &mut pending_publish,
                            &mut latest_color_metadata,
                        ) {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    release_pending_publish_frame(&mut decoder, pending_publish);
}

/// Обрабатывает все pending control messages перед packet receive.
fn drain_decoder_control_messages(
    decoder: &mut crate::VaapiVideoDecoder,
    packet_rx: &Receiver<QueuedDecodePacket>,
    control_rx: &Receiver<ThreadControlMsg>,
    error_tx: &Sender<DecodeThreadError>,
    pending_publish: &mut Option<PendingFramePublish>,
) -> bool {
    loop {
        match control_rx.try_recv() {
            Ok(control_message) => {
                if !handle_decoder_control_message(
                    decoder,
                    control_message,
                    packet_rx,
                    error_tx,
                    pending_publish,
                ) {
                    return false;
                }
            }
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => return false,
        }
    }
}

/// Обрабатывает release/flush control message без ожидания packet channel.
fn handle_decoder_control_message(
    decoder: &mut crate::VaapiVideoDecoder,
    control_message: ThreadControlMsg,
    packet_rx: &Receiver<QueuedDecodePacket>,
    error_tx: &Sender<DecodeThreadError>,
    pending_publish: &mut Option<PendingFramePublish>,
) -> bool {
    match control_message {
        ThreadControlMsg::ReleaseZeroCopy(handle) => {
            if let Err(error) = decoder.release_zero_copy_frame(handle) {
                let message = format!("Video decoder zero-copy release failed: {error:#}");
                tracing::warn!(
                    error = %message,
                    handle_id = handle.0,
                    "Decoder thread: fatal zero-copy release error"
                );
                send_decoder_thread_error(error_tx, message);
                return false;
            }
            true
        }
        ThreadControlMsg::Flush(done_tx) => {
            release_pending_publish_frame(decoder, pending_publish.take());
            let dropped_packet_count = drain_queued_decode_packets(packet_rx);
            if dropped_packet_count > 0 {
                tracing::debug!(
                    dropped_packet_count,
                    "Dropped queued decoder packets during flush"
                );
            }
            let flush_result = decoder.flush().map_err(|error| format!("{error:#}"));
            let flush_failed = flush_result.is_err();

            if let Err(error) = &flush_result {
                let message = format!("Video decoder stopped after flush error: {error}");
                tracing::warn!(
                    error = %message,
                    "Decoder thread: fatal flush error, exiting"
                );
                send_decoder_thread_error(error_tx, message);
            }

            if done_tx.send(flush_result).is_err() {
                tracing::warn!("Decoder thread: flush completed, but caller dropped receiver");
            }

            !flush_failed
        }
    }
}

/// Очищает packet backlog, который был поставлен в decoder до flush/seek.
///
/// Важно чистить именно receiver-side queue: worker после `flush()` уже очистит
/// свои pending packets, но packets, которые успели попасть в decoder channel,
/// иначе будут декодированы после backend flush без старых reference frames.
fn drain_queued_decode_packets(packet_rx: &Receiver<QueuedDecodePacket>) -> usize {
    let mut dropped_packet_count = 0usize;
    loop {
        match packet_rx.try_recv() {
            Ok(_queued_packet) => {
                dropped_packet_count = dropped_packet_count.saturating_add(1);
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                return dropped_packet_count;
            }
        }
    }
}

/// Декодирует один queued packet и ставит первый готовый frame в publish stage.
fn decode_queued_packet(
    decoder: &mut crate::VaapiVideoDecoder,
    queued_packet: QueuedDecodePacket,
    frame_tx: &Sender<DecodedFrame>,
    packet_ack_tx: &Sender<DecodePacketAck>,
    error_tx: &Sender<DecodeThreadError>,
    pending_publish: &mut Option<PendingFramePublish>,
    latest_color_metadata: &mut Option<VideoColorMetadata>,
) -> bool {
    let packet_receive_latency = queued_packet.enqueued_at.elapsed();
    let DecodePacket {
        track_id,
        pts,
        encoded_bytes,
        keyframe,
        resolved_color,
    } = queued_packet.packet;

    *latest_color_metadata = resolved_color.clone();
    let packet = Packet {
        track_id,
        kind: TrackKind::Video,
        pts,
        dts: None,
        keyframe,
        byte_offset: None,
        data: encoded_bytes,
    };

    let decode_result = decoder.decode(&packet);
    let _ = packet_ack_tx.try_send(());

    match decode_result {
        Ok(Some(mut frame)) => {
            if let Some(color_metadata) = &resolved_color {
                frame.color = color_metadata.clone();
            }
            frame.diagnostics.timings.decoder_packet_receive_latency = Some(packet_receive_latency);
            *pending_publish = Some(PendingFramePublish::new(frame));
            publish_pending_frame(frame_tx, pending_publish)
        }
        Ok(None) => true,
        Err(error) => {
            if crate::decoder::is_fatal_decoder_error(&error) {
                let message = format!("Video decoder stopped after fatal error: {error:#}");
                tracing::warn!(
                    error = %message,
                    "Decoder thread: fatal decode error, exiting"
                );
                send_decoder_thread_error(error_tx, message);
                return false;
            }
            tracing::warn!(error = %error, "Decoder thread: decode error");
            true
        }
    }
}

/// Пытается передать pending frame worker-у, не блокируя release/flush control path.
fn publish_pending_frame(
    frame_tx: &Sender<DecodedFrame>,
    pending_publish: &mut Option<PendingFramePublish>,
) -> bool {
    let Some(mut pending_frame) = pending_publish.take() else {
        return true;
    };

    pending_frame
        .frame
        .diagnostics
        .timings
        .decoded_frame_publish_latency = Some(pending_frame.publish_started_at.elapsed());

    match frame_tx.try_send(pending_frame.frame) {
        Ok(()) => true,
        Err(TrySendError::Full(frame)) => {
            *pending_publish = Some(PendingFramePublish {
                frame,
                publish_started_at: pending_frame.publish_started_at,
            });
            true
        }
        Err(TrySendError::Disconnected(frame)) => {
            tracing::warn!(
                handle_id = frame.texture_handle.0,
                "Player thread dropped decoded frame receiver"
            );
            *pending_publish = Some(PendingFramePublish {
                frame,
                publish_started_at: pending_frame.publish_started_at,
            });
            false
        }
    }
}

/// Освобождает frame, который decoder уже импортировал, но не успел отдать worker-у.
fn release_pending_publish_frame(
    decoder: &mut crate::VaapiVideoDecoder,
    pending_publish: Option<PendingFramePublish>,
) {
    let Some(pending_frame) = pending_publish else {
        return;
    };

    if let Err(error) = decoder.release_frame(pending_frame.frame.texture_handle) {
        tracing::warn!(
            error = %error,
            handle_id = pending_frame.frame.texture_handle.0,
            "Failed to release pending decoded frame during decoder thread shutdown/flush"
        );
    }
}

/// Отправляет fatal decoder-thread error без блокировки.
fn send_decoder_thread_error(error_tx: &Sender<DecodeThreadError>, message: String) {
    if error_tx.try_send(DecodeThreadError::new(message)).is_err() {
        trace!("Player thread dropped decoder error receiver");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata};
    use video_core::{DecodedPixelFormat, FrameMemoryPath, FrameTextureHandle};

    /// Создаёт decoded frame без реальных GPU resources для channel-level тестов.
    fn decoded_frame_for_tests(handle_id: u64) -> DecodedFrame {
        DecodedFrame {
            pts: Duration::ZERO,
            format: DecodedPixelFormat::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            memory_path: FrameMemoryPath::DmaBufZeroCopy,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            color: VideoColorMetadata::sdr_bt709_limited(),
            texture_handle: FrameTextureHandle(handle_id),
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }

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
        assert!(VideoDecodeThreadConfig::parse_flush_timeout("0").is_err());
        assert!(VideoDecodeThreadConfig::parse_flush_timeout("abc").is_err());
        assert_eq!(
            VideoDecodeThreadConfig::parse_flush_timeout("25").unwrap(),
            Duration::from_millis(25)
        );
    }

    /// Проверяет, что direct API caller не может случайно создать unbounded/zero queues.
    #[test]
    fn decoder_thread_config_normalizes_zero_queue_limits() {
        let config = VideoDecodeThreadConfig {
            packet_channel_frames: 0,
            frame_channel_frames: 0,
            control_channel_frames: 0,
            decoder_ready_queue_frames: 0,
            decoder_surface_pool_frames: 0,
            zero_copy_surface_pool_slots: 0,
            flush_timeout: Duration::ZERO,
        }
        .normalized();

        assert_eq!(config.packet_channel_frames, 1);
        assert_eq!(config.frame_channel_frames, 1);
        assert_eq!(config.control_channel_frames, 1);
        assert_eq!(config.decoder_ready_queue_frames, 1);
        assert_eq!(config.decoder_surface_pool_frames, 1);
        assert_eq!(config.zero_copy_surface_pool_slots, 1);
        assert_eq!(config.flush_timeout, Duration::from_millis(1));
    }

    /// Проверяет bounded decoded-frame publish: full channel не дропает frame молча.
    #[test]
    fn frame_publish_keeps_pending_frame_when_channel_is_full() {
        let (frame_tx, frame_rx) = bounded(1);
        frame_tx
            .try_send(decoded_frame_for_tests(1))
            .expect("test channel has one free slot");
        let mut pending_publish = Some(PendingFramePublish::new(decoded_frame_for_tests(2)));

        assert!(publish_pending_frame(&frame_tx, &mut pending_publish));
        assert!(pending_publish.is_some());
        assert_eq!(
            frame_rx.try_recv().unwrap().texture_handle,
            FrameTextureHandle(1)
        );

        assert!(publish_pending_frame(&frame_tx, &mut pending_publish));
        assert!(pending_publish.is_none());
        let published_frame = frame_rx.try_recv().unwrap();
        assert_eq!(published_frame.texture_handle, FrameTextureHandle(2));
        assert!(
            published_frame
                .diagnostics
                .timings
                .decoded_frame_publish_latency
                .is_some()
        );
    }

    /// Проверяет seek/flush cancellation: старые packets не остаются после backend flush.
    #[test]
    fn flush_drops_queued_decode_packets() {
        let (packet_tx, packet_rx) = bounded(4);
        for packet_index in 0..3u64 {
            packet_tx
                .try_send(QueuedDecodePacket {
                    packet: DecodePacket {
                        track_id: media_core::TrackId::new(1),
                        pts: Duration::from_millis(packet_index),
                        encoded_bytes: Bytes::from_static(b"vp9"),
                        keyframe: packet_index == 0,
                        resolved_color: None,
                    },
                    enqueued_at: Instant::now(),
                })
                .expect("test packet channel has capacity");
        }

        assert_eq!(drain_queued_decode_packets(&packet_rx), 3);
        assert!(matches!(packet_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    /// Проверяет, что timeout не блокируется бесконечно и становится fatal state.
    #[test]
    fn flush_ack_timeout_marks_thread_fatal_once() {
        let (_done_tx, done_rx) = bounded(1);
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
