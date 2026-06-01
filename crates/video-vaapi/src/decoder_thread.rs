/// Dedicated decoder thread для VA-API hardware decode.
///
/// Изолирует blocking hardware decode и DMA-BUF export от render thread.
///
/// Архитектура:
/// - Render thread отправляет video packets через `send_packet()`.
/// - Decoder thread вызывает `decode()` и публикует только neutral DMA-BUF resource handle.
/// - Готовые `DecodedFrame` возвращаются через `try_recv_frame()`.
/// - Resource pool shared между потоками: decoder thread хранит exported descriptors,
///   render thread получает duplicated fd через provider boundary.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use codec_core::VideoColorMetadata;
use crossbeam_channel::{
    Receiver, RecvTimeoutError, Sender, TryRecvError, TrySendError, bounded, select, unbounded,
};
use media_core::{Packet, TrackId, TrackKind, TrackTimestamp};
use tracing::{info, trace};
use video_core::{
    DecodedFrame, VideoDecoder, VideoDecoderDiagnosticEvent, VideoFramePublishPressureDiagnostics,
};

use crate::decoder::VaapiDecoderRuntimeConfig;
use crate::resource_pool::{DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS, ResourcePoolStats};

/// Результат, которым decoder thread подтверждает завершение flush.
type FlushAck = std::result::Result<(), String>;

/// Подтверждение, что decoder thread уже обработал один packet из input channel.
type DecodePacketAck = ();

/// Bounded capacity diagnostics events от decoder thread.
const DECODER_DIAGNOSTIC_CHANNEL_CAPACITY: usize = 256;

/// Sender typed diagnostics events без зависимости decoder thread-а от player-core.
type DecoderDiagnosticSender = std::sync::mpsc::SyncSender<VideoDecoderDiagnosticEvent>;

/// Receiver typed diagnostics events для player-core drain boundary.
type DecoderDiagnosticReceiver = std::sync::mpsc::Receiver<VideoDecoderDiagnosticEvent>;

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

        Self {
            flush_timeout,
            ..Self::default()
        }
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

/// Snapshot давления на bounded decoder control channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoDecoderControlChannelPressureStats {
    /// Текущая глубина control channel на момент чтения snapshot-а.
    pub control_channel_len: usize,

    /// Bounded capacity control channel-а.
    pub control_channel_capacity: usize,

    /// Сколько send failures произошло именно из-за заполненного control channel-а.
    pub control_channel_full_count: u64,

    /// Сколько раз release path не смог отправить control message.
    pub release_control_send_fail_count: u64,

    /// Сколько раз flush path не смог отправить control message.
    pub flush_control_send_fail_count: u64,
}

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
    ReleaseZeroCopy(video_core::FrameResourceHandle),

    /// Сбросить decoder state и подтвердить завершение операции.
    Flush(Sender<FlushAck>),
}

/// Sender-side control operation для logs и раздельных counters.
#[derive(Debug, Clone, Copy)]
enum DecoderControlOperation {
    /// Возврат zero-copy surface после renderer/GPU ownership.
    Release,

    /// Синхронный flush decoder thread-а.
    Flush,
}

impl DecoderControlOperation {
    /// Возвращает стабильное имя операции для structured logs.
    const fn metric_name(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Flush => "flush",
        }
    }

    /// Возвращает прежний текстовый контекст fatal error-а.
    const fn fatal_context(self) -> &'static str {
        match self {
            Self::Release => "zero-copy release",
            Self::Flush => "decoder flush",
        }
    }
}

/// Shared sender-side counters decoder control channel-а.
#[derive(Debug, Default)]
struct DecoderControlChannelPressureCounters {
    /// Накопительное число Full отказов независимо от операции.
    full_count: AtomicU64,

    /// Накопительное число release send failures.
    release_send_fail_count: AtomicU64,

    /// Накопительное число flush send failures.
    flush_send_fail_count: AtomicU64,
}

impl DecoderControlChannelPressureCounters {
    /// Учитывает failed send до fail-closed перехода и возвращает актуальный snapshot.
    fn record_send_failure(
        &self,
        operation: DecoderControlOperation,
        control_tx: &Sender<ThreadControlMsg>,
        error: &TrySendError<ThreadControlMsg>,
    ) -> VideoDecoderControlChannelPressureStats {
        if matches!(error, TrySendError::Full(_)) {
            self.full_count.fetch_add(1, Ordering::Relaxed);
        }

        match operation {
            DecoderControlOperation::Release => {
                self.release_send_fail_count.fetch_add(1, Ordering::Relaxed);
            }
            DecoderControlOperation::Flush => {
                self.flush_send_fail_count.fetch_add(1, Ordering::Relaxed);
            }
        }

        self.snapshot(control_tx)
    }

    /// Снимает текущую глубину канала и накопительные counters.
    fn snapshot(
        &self,
        control_tx: &Sender<ThreadControlMsg>,
    ) -> VideoDecoderControlChannelPressureStats {
        VideoDecoderControlChannelPressureStats {
            control_channel_len: control_tx.len(),
            control_channel_capacity: control_tx.capacity().unwrap_or(0),
            control_channel_full_count: self.full_count.load(Ordering::Relaxed),
            release_control_send_fail_count: self.release_send_fail_count.load(Ordering::Relaxed),
            flush_control_send_fail_count: self.flush_send_fail_count.load(Ordering::Relaxed),
        }
    }
}

/// Диагностика получения VAAPI resource pool lock-а на render hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFrameResourceLockDiagnostics {
    /// Сколько render thread ждал mutex resource pool-а.
    pub wait: Duration,
}

/// Результат playback-facing resource lookup-а без GPU handles.
pub enum VideoFrameResourceLookup {
    /// Resource pool доступен, handle валиден.
    Ready {
        /// Timing ожидания `FrameResourcePool` lock-а.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },

    /// Resource pool сейчас занят другим потоком, render hot path не должен ждать.
    Busy {
        /// Timing короткой non-blocking попытки получить `FrameResourcePool`.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },

    /// Resource pool доступен, но handle не указывает на active resource.
    Missing {
        /// Timing ожидания `FrameResourcePool` lock-а.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },

    /// Resource pool не может безопасно ответить из-за poisoned/fatal состояния.
    Fatal {
        /// Timing ожидания `FrameResourcePool` lock-а.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },
}

impl VideoFrameResourceLookup {
    /// Возвращает timing mutex boundary независимо от lookup outcome.
    #[must_use]
    pub const fn lock_diagnostics(&self) -> VideoFrameResourceLockDiagnostics {
        match self {
            Self::Ready { lock_diagnostics }
            | Self::Busy { lock_diagnostics }
            | Self::Missing { lock_diagnostics }
            | Self::Fatal { lock_diagnostics } => *lock_diagnostics,
        }
    }
}

/// Результат renderer-facing descriptor lookup-а с duplicated platform handles.
pub enum VideoFrameResourceDescriptorLookup {
    /// Descriptor duplicated успешно; renderer владеет returned fd.
    Ready {
        /// Neutral descriptor без VAAPI/cros/renderer API types.
        descriptor: video_core::FrameResourceDescriptor,

        /// Timing ожидания `FrameResourcePool` lock-а.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },

    /// Resource pool сейчас занят другим потоком, render hot path не должен ждать.
    Busy {
        /// Timing короткой non-blocking попытки получить `FrameResourcePool`.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },

    /// Resource pool доступен, но handle не указывает на active resource.
    Missing {
        /// Timing ожидания `FrameResourcePool` lock-а.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },

    /// Descriptor нельзя безопасно дублировать из-за poisoned/fatal состояния.
    Fatal {
        /// Timing ожидания `FrameResourcePool` lock-а.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },
}

impl VideoFrameResourceDescriptorLookup {
    /// Возвращает timing mutex boundary независимо от lookup outcome.
    #[must_use]
    pub const fn lock_diagnostics(&self) -> VideoFrameResourceLockDiagnostics {
        match self {
            Self::Ready {
                lock_diagnostics, ..
            }
            | Self::Busy { lock_diagnostics }
            | Self::Missing { lock_diagnostics }
            | Self::Fatal { lock_diagnostics } => *lock_diagnostics,
        }
    }
}

/// Узкий provider для VAAPI resource status, descriptor duplication и release.
#[derive(Clone)]
pub struct VideoFrameResourceProvider {
    /// Канал decoder thread для release zero-copy VA handles.
    control_tx: Sender<ThreadControlMsg>,

    /// Shared counters pressure/failure diagnostics для control channel.
    control_pressure: Arc<DecoderControlChannelPressureCounters>,

    /// Shared resource pool, из которого renderer получает duplicated descriptors.
    resource_pool: Arc<Mutex<crate::resource_pool::FrameResourcePool>>,

    /// Shared fatal state, чтобы release path мог сообщить о disconnect-е.
    thread_state: DecoderThreadState,
}

impl VideoFrameResourceProvider {
    /// Получает status и timing ожидания resource pool mutex-а.
    #[must_use]
    pub fn resource_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> VideoFrameResourceLookup {
        resource_lookup_from_pool(self.resource_pool.as_ref(), handle)
    }

    /// Пытается получить status без ожидания resource pool mutex-а.
    #[must_use]
    pub fn try_resource_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> VideoFrameResourceLookup {
        try_resource_lookup_from_pool(self.resource_pool.as_ref(), handle)
    }

    /// Пытается получить duplicated descriptor без ожидания resource pool mutex-а.
    #[must_use]
    pub fn try_resource_descriptor_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> VideoFrameResourceDescriptorLookup {
        try_resource_descriptor_lookup_from_pool(self.resource_pool.as_ref(), handle)
    }

    /// Освобождает frame после того, как caller уже дождался GPU completion.
    pub fn release_frame(&self, handle: video_core::FrameResourceHandle) {
        trace!(handle_id = handle.0, "Releasing zero-copy frame to decoder");
        match self.resource_pool.lock() {
            Ok(mut resource_pool) => {
                if let Err(error) = resource_pool.release_without_gpu_submission(handle) {
                    let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(
                        format!("Zero-copy surface release lifecycle violation: {error}"),
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
                    "Zero-copy resource pool mutex poisoned during release: {error}"
                )));
                tracing::warn!(
                    error = %error,
                    fatal = %fatal_error,
                    handle_id = handle.0,
                    "Resource pool mutex poisoned during release"
                );
                return;
            }
        }

        if let Err(error) = self
            .control_tx
            .try_send(ThreadControlMsg::ReleaseZeroCopy(handle))
        {
            let error_message = record_decoder_control_send_failure(
                DecoderControlOperation::Release,
                &self.control_tx,
                &self.control_pressure,
                &error,
            );
            let fatal_error = self
                .thread_state
                .mark_fatal(DecodeThreadError::new(error_message));
            tracing::warn!(
                error = %error,
                fatal = %fatal_error,
                handle_id = handle.0,
                "Failed to send zero-copy release to decoder thread"
            );
        }
    }
}

/// Измеряет ожидание resource pool mutex-а и сохраняет lookup семантику.
fn resource_lookup_from_pool(
    resource_pool: &Mutex<crate::resource_pool::FrameResourcePool>,
    handle: video_core::FrameResourceHandle,
) -> VideoFrameResourceLookup {
    resource_lookup_from_pool_started_at(resource_pool, handle, Instant::now())
}

/// Выполняет lookup от уже зафиксированного start time; используется для точного теста timing-а.
fn resource_lookup_from_pool_started_at(
    resource_pool: &Mutex<crate::resource_pool::FrameResourcePool>,
    handle: video_core::FrameResourceHandle,
    lock_started_at: Instant,
) -> VideoFrameResourceLookup {
    match resource_pool.lock() {
        Ok(resource_pool) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            resource_lookup_from_locked_pool(&resource_pool, handle, lock_diagnostics)
        }
        Err(error) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            tracing::warn!(error = %error, "Resource pool mutex poisoned during lookup");
            VideoFrameResourceLookup::Fatal { lock_diagnostics }
        }
    }
}

/// Неблокирующе выполняет lookup и отдельно возвращает transient busy state.
fn try_resource_lookup_from_pool(
    resource_pool: &Mutex<crate::resource_pool::FrameResourcePool>,
    handle: video_core::FrameResourceHandle,
) -> VideoFrameResourceLookup {
    try_resource_lookup_from_pool_started_at(resource_pool, handle, Instant::now())
}

/// Выполняет non-blocking lookup от уже зафиксированного start time для unit-тестов.
fn try_resource_lookup_from_pool_started_at(
    resource_pool: &Mutex<crate::resource_pool::FrameResourcePool>,
    handle: video_core::FrameResourceHandle,
    lock_started_at: Instant,
) -> VideoFrameResourceLookup {
    match resource_pool.try_lock() {
        Ok(resource_pool) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            resource_lookup_from_locked_pool(&resource_pool, handle, lock_diagnostics)
        }
        Err(TryLockError::WouldBlock) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            VideoFrameResourceLookup::Busy { lock_diagnostics }
        }
        Err(TryLockError::Poisoned(error)) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            tracing::warn!(error = %error, "Resource pool mutex poisoned during try_lookup");
            VideoFrameResourceLookup::Fatal { lock_diagnostics }
        }
    }
}

/// Преобразует доступный resource pool в typed lookup result без знания о mutex state.
fn resource_lookup_from_locked_pool(
    resource_pool: &crate::resource_pool::FrameResourcePool,
    handle: video_core::FrameResourceHandle,
    lock_diagnostics: VideoFrameResourceLockDiagnostics,
) -> VideoFrameResourceLookup {
    if resource_pool.is_registered_handle(handle) {
        VideoFrameResourceLookup::Ready { lock_diagnostics }
    } else {
        VideoFrameResourceLookup::Missing { lock_diagnostics }
    }
}

/// Неблокирующе дублирует descriptor и отдельно возвращает transient busy state.
fn try_resource_descriptor_lookup_from_pool(
    resource_pool: &Mutex<crate::resource_pool::FrameResourcePool>,
    handle: video_core::FrameResourceHandle,
) -> VideoFrameResourceDescriptorLookup {
    let lock_started_at = Instant::now();
    match resource_pool.try_lock() {
        Ok(resource_pool) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            match resource_pool.duplicate_descriptor(handle) {
                Ok(Some(descriptor)) => VideoFrameResourceDescriptorLookup::Ready {
                    descriptor,
                    lock_diagnostics,
                },
                Ok(None) => VideoFrameResourceDescriptorLookup::Missing { lock_diagnostics },
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        handle_id = handle.0,
                        "Failed to duplicate VAAPI DMA-BUF resource descriptor"
                    );
                    VideoFrameResourceDescriptorLookup::Fatal { lock_diagnostics }
                }
            }
        }
        Err(TryLockError::WouldBlock) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            VideoFrameResourceDescriptorLookup::Busy { lock_diagnostics }
        }
        Err(TryLockError::Poisoned(error)) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            tracing::warn!(error = %error, "Resource pool mutex poisoned during descriptor lookup");
            VideoFrameResourceDescriptorLookup::Fatal { lock_diagnostics }
        }
    }
}

/// Сырые данные видео-пакета для передачи в decoder thread.
pub struct DecodePacket {
    /// Track ID выбранного video stream.
    pub track_id: TrackId,

    /// Presentation timestamp packet-а.
    pub pts: Duration,

    /// Decode timestamp packet-а, если container сообщил DTS.
    pub dts: Option<Duration>,

    /// Raw track DTS для backends, которым нужен decode-order timestamp.
    pub track_dts: Option<TrackTimestamp>,

    /// Seek generation player pipeline-а, которому принадлежит packet.
    pub generation: u64,

    /// Encoded video bytes, которые decoder thread передаёт hardware backend-у без повторной копии.
    pub encoded_bytes: Bytes,

    /// Keyframe flag из container/demuxer.
    pub keyframe: bool,

    /// Resolved color metadata из player/capability layer для decoded frame contract.
    pub resolved_color: Option<VideoColorMetadata>,
}

impl From<video_core::DecodePacket> for DecodePacket {
    /// Адаптирует neutral packet к текущему production VA-API backend-у.
    fn from(packet: video_core::DecodePacket) -> Self {
        Self {
            track_id: packet.track_id,
            pts: packet.pts,
            dts: packet.dts,
            track_dts: packet.track_dts,
            generation: packet.generation,
            encoded_bytes: packet.encoded_bytes,
            keyframe: packet.keyframe,
            resolved_color: packet.resolved_color,
        }
    }
}

impl From<DecodePacket> for video_core::DecodePacket {
    /// Возвращает VA-API packet в neutral форму для adapter coverage.
    fn from(packet: DecodePacket) -> Self {
        Self {
            track_id: packet.track_id,
            pts: packet.pts,
            dts: packet.dts,
            track_dts: packet.track_dts,
            generation: packet.generation,
            encoded_bytes: packet.encoded_bytes,
            keyframe: packet.keyframe,
            resolved_color: packet.resolved_color,
        }
    }
}

impl From<video_core::VideoDecoderThreadConfig> for VideoDecodeThreadConfig {
    /// Адаптирует neutral decoder-thread limits к текущему VA-API backend-у.
    fn from(config: video_core::VideoDecoderThreadConfig) -> Self {
        Self {
            packet_channel_frames: config.packet_channel_frames,
            frame_channel_frames: config.frame_channel_frames,
            control_channel_frames: config.control_channel_frames,
            decoder_ready_queue_frames: config.decoder_ready_queue_frames,
            decoder_surface_pool_frames: config.decoder_surface_pool_frames,
            zero_copy_surface_pool_slots: config.zero_copy_surface_pool_slots,
            flush_timeout: config.flush_timeout,
        }
    }
}

impl From<VideoDecodeThreadConfig> for video_core::VideoDecoderThreadConfig {
    /// Возвращает VA-API config в neutral форму для compatibility и adapter tests.
    fn from(config: VideoDecodeThreadConfig) -> Self {
        Self {
            packet_channel_frames: config.packet_channel_frames,
            frame_channel_frames: config.frame_channel_frames,
            control_channel_frames: config.control_channel_frames,
            decoder_ready_queue_frames: config.decoder_ready_queue_frames,
            decoder_surface_pool_frames: config.decoder_surface_pool_frames,
            zero_copy_surface_pool_slots: config.zero_copy_surface_pool_slots,
            flush_timeout: config.flush_timeout,
        }
    }
}

impl From<video_core::DecodeThreadError> for DecodeThreadError {
    /// Адаптирует neutral fatal error для VA-API-facing adapter paths.
    fn from(error: video_core::DecodeThreadError) -> Self {
        Self::new(error.message().to_owned())
    }
}

impl From<DecodeThreadError> for video_core::DecodeThreadError {
    /// Сохраняет текст fatal ошибки без привязки player-core к VA-API error type.
    fn from(error: DecodeThreadError) -> Self {
        Self::new(error.message().to_owned())
    }
}

impl From<video_core::DecodeBackpressureReason> for DecodeThreadBackpressureReason {
    /// Адаптирует neutral backpressure reason к текущему VA-API send error.
    fn from(reason: video_core::DecodeBackpressureReason) -> Self {
        match reason {
            video_core::DecodeBackpressureReason::PacketQueueFull {
                queued_packets,
                capacity,
            } => Self::PacketQueueFull {
                queued_packets,
                capacity,
            },
        }
    }
}

impl From<DecodeThreadBackpressureReason> for video_core::DecodeBackpressureReason {
    /// Сохраняет typed backpressure reason и queue accounting.
    fn from(reason: DecodeThreadBackpressureReason) -> Self {
        match reason {
            DecodeThreadBackpressureReason::PacketQueueFull {
                queued_packets,
                capacity,
            } => Self::PacketQueueFull {
                queued_packets,
                capacity,
            },
        }
    }
}

impl From<video_core::DecodeSendError> for DecodeThreadSendError {
    /// Адаптирует neutral send error к VA-API-facing adapter paths.
    fn from(error: video_core::DecodeSendError) -> Self {
        match error {
            video_core::DecodeSendError::Backpressure(reason) => Self::Backpressure(reason.into()),
            video_core::DecodeSendError::Fatal(error) => Self::Fatal(error.into()),
        }
    }
}

impl From<DecodeThreadSendError> for video_core::DecodeSendError {
    /// Сохраняет различие backpressure/fatal на neutral decoder boundary.
    fn from(error: DecodeThreadSendError) -> Self {
        match error {
            DecodeThreadSendError::Backpressure(reason) => Self::Backpressure(reason.into()),
            DecodeThreadSendError::Fatal(error) => Self::Fatal(error.into()),
        }
    }
}

impl From<ResourcePoolStats> for video_core::DecoderResourceSnapshot {
    /// Копирует VA-API resource pool counters в backend-neutral diagnostics snapshot.
    fn from(stats: ResourcePoolStats) -> Self {
        Self {
            capacity: stats.capacity,
            slots: stats.slots,
            in_use: stats.in_use,
            free_surfaces: stats.free_surfaces,
            waiting_gpu_completion: stats.waiting_gpu_completion,
            waiting_decoder_reuse: stats.waiting_decoder_reuse,
            import_failures: stats.import_failures,
            imports_created: stats.imports_created,
            imports_reused: stats.imports_reused,
            imports_replaced: stats.imports_replaced,
        }
    }
}

impl From<video_core::DecoderResourceSnapshot> for ResourcePoolStats {
    /// Адаптирует neutral diagnostics snapshot обратно к текущему VA-API stats type.
    fn from(stats: video_core::DecoderResourceSnapshot) -> Self {
        Self {
            capacity: stats.capacity,
            slots: stats.slots,
            in_use: stats.in_use,
            free_surfaces: stats.free_surfaces,
            waiting_gpu_completion: stats.waiting_gpu_completion,
            waiting_decoder_reuse: stats.waiting_decoder_reuse,
            import_failures: stats.import_failures,
            imports_created: stats.imports_created,
            imports_reused: stats.imports_reused,
            imports_replaced: stats.imports_replaced,
        }
    }
}

impl From<VideoDecoderControlChannelPressureStats>
    for video_core::VideoDecoderControlChannelPressureSnapshot
{
    /// Копирует VA-API control-channel counters в neutral diagnostics snapshot.
    fn from(stats: VideoDecoderControlChannelPressureStats) -> Self {
        Self {
            control_channel_len: stats.control_channel_len,
            control_channel_capacity: stats.control_channel_capacity,
            control_channel_full_count: stats.control_channel_full_count,
            release_control_send_fail_count: stats.release_control_send_fail_count,
            flush_control_send_fail_count: stats.flush_control_send_fail_count,
        }
    }
}

impl From<video_core::VideoDecoderControlChannelPressureSnapshot>
    for VideoDecoderControlChannelPressureStats
{
    /// Адаптирует neutral control-channel snapshot обратно к VA-API stats type.
    fn from(stats: video_core::VideoDecoderControlChannelPressureSnapshot) -> Self {
        Self {
            control_channel_len: stats.control_channel_len,
            control_channel_capacity: stats.control_channel_capacity,
            control_channel_full_count: stats.control_channel_full_count,
            release_control_send_fail_count: stats.release_control_send_fail_count,
            flush_control_send_fail_count: stats.flush_control_send_fail_count,
        }
    }
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
    control_pressure: Arc<DecoderControlChannelPressureCounters>,
    frame_rx: Receiver<DecodedFrame>,
    packet_ack_rx: Receiver<DecodePacketAck>,
    error_rx: Receiver<DecodeThreadError>,
    diagnostic_rx: DecoderDiagnosticReceiver,
    resource_pool: Arc<Mutex<crate::resource_pool::FrameResourcePool>>,
    thread_state: DecoderThreadState,
    stream_config: Arc<Mutex<Option<video_core::VideoStreamDecodeConfig>>>,
    end_of_stream_drain_state: Arc<Mutex<video_core::VideoDecoderEndOfStreamDrainState>>,
    config: VideoDecodeThreadConfig,
    backend_name: &'static str,
}

impl VideoDecodeThread {
    /// Создаёт decoder thread с VA-API hardware decoder.
    pub fn new() -> anyhow::Result<Self> {
        Self::new_with_config(VideoDecodeThreadConfig::from_env())
    }

    /// Создаёт decoder thread с явно заданными bounded queue/runtime limits.
    pub fn new_with_config(config: VideoDecodeThreadConfig) -> anyhow::Result<Self> {
        let config = config.normalized();
        let resource_pool = Arc::new(Mutex::new(
            crate::resource_pool::FrameResourcePool::new_with_capacity(
                config.zero_copy_surface_pool_slots,
            ),
        ));
        let resource_pool_for_thread = resource_pool.clone();

        let (packet_tx, packet_rx) = bounded::<QueuedDecodePacket>(config.packet_channel_frames);
        let (control_tx, control_rx) = bounded::<ThreadControlMsg>(config.control_channel_frames);
        let control_pressure = Arc::new(DecoderControlChannelPressureCounters::default());
        let (frame_tx, frame_rx) = bounded::<DecodedFrame>(config.frame_channel_frames);
        let (packet_ack_tx, packet_ack_rx) = unbounded::<DecodePacketAck>();
        let (error_tx, error_rx) = bounded::<DecodeThreadError>(1);
        let (diagnostic_tx, diagnostic_rx) =
            std::sync::mpsc::sync_channel(DECODER_DIAGNOSTIC_CHANNEL_CAPACITY);
        let thread_diagnostic_tx = diagnostic_tx.clone();
        let (init_tx, init_rx) = bounded::<anyhow::Result<()>>(1);
        let thread_state = DecoderThreadState::new();
        let decoder_runtime_config = config.vaapi_decoder_config();

        std::thread::Builder::new()
            .name("video-decode".into())
            .spawn(move || {
                info!("Decoder thread started");

                let decoder = match crate::VaapiVideoDecoder::new_with_pool(
                    resource_pool_for_thread,
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
                    thread_diagnostic_tx,
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
            control_pressure,
            frame_rx,
            packet_ack_rx,
            error_rx,
            diagnostic_rx,
            resource_pool,
            thread_state,
            stream_config: Arc::new(Mutex::new(None)),
            end_of_stream_drain_state: Arc::new(Mutex::new(
                video_core::VideoDecoderEndOfStreamDrainState::Idle,
            )),
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

    /// Принимает stream config для текущей VA-API adapter matrix.
    pub fn configure_stream(
        &self,
        config: video_core::VideoStreamDecodeConfig,
    ) -> video_core::VideoStreamConfigResult {
        if let Err(error) = self.ensure_thread_usable() {
            return video_core::VideoStreamConfigResult::Fatal(error.into());
        }
        if let Some(rejection) = reject_unsupported_vaapi_stream_config(&config) {
            return video_core::VideoStreamConfigResult::Unsupported(rejection);
        }

        let mut stream_config = match self.stream_config.lock() {
            Ok(stream_config) => stream_config,
            Err(error) => {
                let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(format!(
                    "VA-API stream config mutex poisoned: {error}"
                )));
                return video_core::VideoStreamConfigResult::Fatal(fatal_error.into());
            }
        };

        if stream_config.as_ref() == Some(&config) {
            return video_core::VideoStreamConfigResult::Unchanged;
        }

        *stream_config = Some(config);
        self.reset_end_of_stream_drain_state();
        video_core::VideoStreamConfigResult::Configured
    }

    /// Очищает stream config как explicit media-switch lifecycle step.
    pub fn clear_stream(&self) -> video_core::VideoStreamConfigResult {
        if let Err(error) = self.ensure_thread_usable() {
            return video_core::VideoStreamConfigResult::Fatal(error.into());
        }

        let mut stream_config = match self.stream_config.lock() {
            Ok(stream_config) => stream_config,
            Err(error) => {
                let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(format!(
                    "VA-API stream config mutex poisoned during clear: {error}"
                )));
                return video_core::VideoStreamConfigResult::Fatal(fatal_error.into());
            }
        };

        self.reset_end_of_stream_drain_state();
        if stream_config.take().is_some() {
            video_core::VideoStreamConfigResult::Cleared
        } else {
            video_core::VideoStreamConfigResult::Unchanged
        }
    }

    /// Запускает explicit EOF drain; VP9 stateless path не имеет отложенного DPB tail.
    pub fn begin_end_of_stream_drain(
        &self,
        generation: u64,
    ) -> video_core::VideoDecoderEndOfStreamDrainResult {
        if let Err(error) = self.ensure_thread_usable() {
            return video_core::VideoDecoderEndOfStreamDrainResult::Fatal(error.into());
        }

        let mut drain_state = match self.end_of_stream_drain_state.lock() {
            Ok(drain_state) => drain_state,
            Err(error) => {
                let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(format!(
                    "VA-API EOF drain state mutex poisoned: {error}"
                )));
                return video_core::VideoDecoderEndOfStreamDrainResult::Fatal(fatal_error.into());
            }
        };

        if matches!(
            *drain_state,
            video_core::VideoDecoderEndOfStreamDrainState::Draining {
                generation: active_generation,
            } | video_core::VideoDecoderEndOfStreamDrainState::Drained {
                generation: active_generation,
            } if active_generation == generation
        ) {
            return video_core::VideoDecoderEndOfStreamDrainResult::Unchanged(drain_state.clone());
        }

        *drain_state = video_core::VideoDecoderEndOfStreamDrainState::Drained { generation };
        video_core::VideoDecoderEndOfStreamDrainResult::Started(drain_state.clone())
    }

    /// Возвращает текущее explicit EOF drain state без блокировки decoder thread loop-а.
    pub fn end_of_stream_drain_state(&self) -> video_core::VideoDecoderEndOfStreamDrainState {
        match self.end_of_stream_drain_state.lock() {
            Ok(drain_state) => drain_state.clone(),
            Err(error) => {
                let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(format!(
                    "VA-API EOF drain state mutex poisoned during read: {error}"
                )));
                video_core::VideoDecoderEndOfStreamDrainState::Fatal {
                    generation: None,
                    error: fatal_error.into(),
                }
            }
        }
    }

    /// Сбрасывает EOF-drain marker после смены stream-а или media.
    fn reset_end_of_stream_drain_state(&self) {
        if let Ok(mut drain_state) = self.end_of_stream_drain_state.lock() {
            *drain_state = video_core::VideoDecoderEndOfStreamDrainState::Idle;
        }
    }

    /// Освобождает frame, который не находится в renderer GPU work.
    ///
    /// Используется для queued/present frames без active render lease. Такой frame
    /// можно вернуть decoder-у сразу: GPU completion уже не требуется.
    pub fn release_frame(&self, handle: video_core::FrameResourceHandle) {
        match self.resource_pool.lock() {
            Ok(mut resource_pool) => {
                if let Err(error) = resource_pool.release_without_gpu_submission(handle) {
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
                    "Zero-copy resource pool mutex poisoned during immediate release: {error}"
                )));
                tracing::warn!(
                    error = %error,
                    fatal = %fatal_error,
                    handle_id = handle.0,
                    "Resource pool mutex poisoned during immediate release"
                );
                return;
            }
        }

        if let Err(error) = self
            .control_tx
            .try_send(ThreadControlMsg::ReleaseZeroCopy(handle))
        {
            let error_message = record_decoder_control_send_failure(
                DecoderControlOperation::Release,
                &self.control_tx,
                &self.control_pressure,
                &error,
            );
            let fatal_error = self
                .thread_state
                .mark_fatal(DecodeThreadError::new(error_message));
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
            let error_message = record_decoder_control_send_failure(
                DecoderControlOperation::Flush,
                &self.control_tx,
                &self.control_pressure,
                &error,
            );
            let fatal_error = self
                .thread_state
                .mark_fatal(DecodeThreadError::new(error_message));
            return Err(anyhow::anyhow!("{}", fatal_error));
        }

        let flush_result =
            wait_for_flush_ack(done_rx, self.config.flush_timeout, &self.thread_state);
        self.release_received_frames();
        self.drain_completed_packet_acks();
        flush_result
    }

    /// Возвращает cloneable provider для resource lookup/descriptor/release.
    #[must_use]
    pub fn frame_resource_provider(&self) -> VideoFrameResourceProvider {
        VideoFrameResourceProvider {
            control_tx: self.control_tx.clone(),
            control_pressure: self.control_pressure.clone(),
            resource_pool: self.resource_pool.clone(),
            thread_state: self.thread_state.clone(),
        }
    }

    /// Возвращает состояние resource pool для backpressure и UI.
    pub fn resource_pool_stats(&self) -> Option<ResourcePoolStats> {
        match self.resource_pool.lock() {
            Ok(resource_pool) => Some(resource_pool.stats()),
            Err(error) => {
                tracing::warn!(error = %error, "Resource pool mutex poisoned during stats read");
                None
            }
        }
    }

    /// Возвращает sender-side pressure snapshot bounded control channel-а.
    #[must_use]
    pub fn control_channel_pressure_stats(&self) -> VideoDecoderControlChannelPressureStats {
        self.control_pressure.snapshot(&self.control_tx)
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
            self.release_frame(frame.resource_handle);
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

/// Проверяет stream config на реализованный в этой фазе VA-API adapter intersection.
fn reject_unsupported_vaapi_stream_config(
    config: &video_core::VideoStreamDecodeConfig,
) -> Option<video_core::VideoStreamConfigRejection> {
    crate::codec_adapter::VaapiCodecAdapterFactory::stream_config_rejection(config)
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

    /// Был ли этот frame уже остановлен заполненным bounded frame channel.
    has_seen_channel_full: bool,
}

impl PendingFramePublish {
    /// Создаёт pending publish item и начинает измерять decoded-frame publish latency.
    fn new(frame: DecodedFrame) -> Self {
        Self {
            frame,
            publish_started_at: Instant::now(),
            has_seen_channel_full: false,
        }
    }

    /// Помечает frame как ожидающий свободного места в bounded frame channel.
    fn mark_channel_full(&mut self) {
        self.has_seen_channel_full = true;
    }
}

/// Локальные counters decoder thread-а для decoded-frame publish boundary.
#[derive(Debug, Default)]
struct FramePublishPressureCounters {
    /// Накопительный snapshot, который можно отправлять через diagnostics event.
    pressure: VideoFramePublishPressureDiagnostics,
}

impl FramePublishPressureCounters {
    /// Учитывает заполненный bounded frame channel без изменения publish lifecycle.
    fn record_channel_full(&mut self) {
        self.pressure.frame_publish_channel_full_count = self
            .pressure
            .frame_publish_channel_full_count
            .saturating_add(1);
    }

    /// Учитывает повторную попытку публикации уже pending frame.
    fn record_pending_retry(&mut self) {
        self.pressure.pending_publish_retry_count =
            self.pressure.pending_publish_retry_count.saturating_add(1);
    }

    /// Учитывает latency только один раз: когда frame реально опубликован worker-у.
    fn record_published_latency(&mut self, latency: Duration) {
        self.pressure.total_decoded_frame_publish_latency = self
            .pressure
            .total_decoded_frame_publish_latency
            .saturating_add(latency);
        if latency > self.pressure.max_decoded_frame_publish_latency {
            self.pressure.max_decoded_frame_publish_latency = latency;
        }
    }

    /// Возвращает копию counters для неблокирующей отправки в diagnostics channel.
    fn snapshot(&self) -> VideoFramePublishPressureDiagnostics {
        self.pressure
    }
}

/// Учитывает failed control send и пишет pressure fields перед fail-closed переходом.
fn record_decoder_control_send_failure(
    operation: DecoderControlOperation,
    control_tx: &Sender<ThreadControlMsg>,
    pressure_counters: &DecoderControlChannelPressureCounters,
    error: &TrySendError<ThreadControlMsg>,
) -> String {
    let pressure = pressure_counters.record_send_failure(operation, control_tx, error);
    tracing::debug!(
        operation = operation.metric_name(),
        len = pressure.control_channel_len,
        capacity = pressure.control_channel_capacity,
        control_channel_full_count = pressure.control_channel_full_count,
        release_control_send_fail_count = pressure.release_control_send_fail_count,
        flush_control_send_fail_count = pressure.flush_control_send_fail_count,
        error = %error,
        "Decoder control channel send failed before fail-closed transition"
    );
    decoder_control_send_error_message(operation.fatal_context(), error)
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
    diagnostic_tx: DecoderDiagnosticSender,
) {
    let mut pending_publish: Option<PendingFramePublish> = None;
    let mut publish_pressure = FramePublishPressureCounters::default();
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

        if !publish_pending_frame(
            &frame_tx,
            &mut pending_publish,
            &mut publish_pressure,
            &diagnostic_tx,
        ) {
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
                            DecodeQueuedPacketContext {
                                frame_tx: &frame_tx,
                                packet_ack_tx: &packet_ack_tx,
                                error_tx: &error_tx,
                                pending_publish: &mut pending_publish,
                                publish_pressure: &mut publish_pressure,
                                diagnostic_tx: &diagnostic_tx,
                                latest_color_metadata: &mut latest_color_metadata,
                            },
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

/// Собирает decoder-thread state, который нужен одному packet decode step.
struct DecodeQueuedPacketContext<'a> {
    frame_tx: &'a Sender<DecodedFrame>,
    packet_ack_tx: &'a Sender<DecodePacketAck>,
    error_tx: &'a Sender<DecodeThreadError>,
    pending_publish: &'a mut Option<PendingFramePublish>,
    publish_pressure: &'a mut FramePublishPressureCounters,
    diagnostic_tx: &'a DecoderDiagnosticSender,
    latest_color_metadata: &'a mut Option<VideoColorMetadata>,
}

/// Декодирует один queued packet и ставит первый готовый frame в publish stage.
fn decode_queued_packet(
    decoder: &mut crate::VaapiVideoDecoder,
    queued_packet: QueuedDecodePacket,
    decode_context: DecodeQueuedPacketContext<'_>,
) -> bool {
    let packet_receive_latency = queued_packet.enqueued_at.elapsed();
    let DecodePacket {
        track_id,
        pts,
        dts,
        track_dts,
        generation,
        encoded_bytes,
        keyframe,
        resolved_color,
    } = queued_packet.packet;

    *decode_context.latest_color_metadata = resolved_color.clone();
    let packet = Packet {
        track_id,
        kind: TrackKind::Video,
        pts,
        track_pts: None,
        dts,
        track_dts,
        duration: None,
        track_duration: None,
        keyframe: keyframe.into(),
        byte_offset: None,
        data: encoded_bytes,
    };

    let decode_result = decoder.decode(&packet);
    let _ = decode_context.packet_ack_tx.try_send(());

    match decode_result {
        Ok(Some(mut frame)) => {
            frame.generation = generation;
            if let Some(color_metadata) = &resolved_color {
                frame.color = color_metadata.clone();
            }
            frame.diagnostics.timings.decoder_packet_receive_latency = Some(packet_receive_latency);
            *decode_context.pending_publish = Some(PendingFramePublish::new(frame));
            publish_pending_frame(
                decode_context.frame_tx,
                decode_context.pending_publish,
                decode_context.publish_pressure,
                decode_context.diagnostic_tx,
            )
        }
        Ok(None) => true,
        Err(error) => {
            if crate::decoder::is_fatal_decoder_error(&error) {
                let message = format!("Video decoder stopped after fatal error: {error:#}");
                tracing::warn!(
                    error = %message,
                    "Decoder thread: fatal decode error, exiting"
                );
                send_decoder_thread_error(decode_context.error_tx, message);
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
    publish_pressure: &mut FramePublishPressureCounters,
    diagnostic_tx: &DecoderDiagnosticSender,
) -> bool {
    let Some(mut pending_frame) = pending_publish.take() else {
        return true;
    };

    let is_retry = pending_frame.has_seen_channel_full;
    let publish_latency = pending_frame.publish_started_at.elapsed();
    pending_frame
        .frame
        .diagnostics
        .timings
        .decoded_frame_publish_latency = Some(publish_latency);

    match frame_tx.try_send(pending_frame.frame) {
        Ok(()) => {
            if is_retry {
                publish_pressure.record_pending_retry();
            }
            publish_pressure.record_published_latency(publish_latency);
            if is_retry {
                send_frame_publish_pressure_event(diagnostic_tx, publish_pressure.snapshot());
            }
            true
        }
        Err(TrySendError::Full(frame)) => {
            if is_retry {
                publish_pressure.record_pending_retry();
            }
            publish_pressure.record_channel_full();
            let mut blocked_frame = PendingFramePublish {
                frame,
                publish_started_at: pending_frame.publish_started_at,
                has_seen_channel_full: pending_frame.has_seen_channel_full,
            };
            blocked_frame.mark_channel_full();
            *pending_publish = Some(blocked_frame);
            send_frame_publish_pressure_event(diagnostic_tx, publish_pressure.snapshot());
            true
        }
        Err(TrySendError::Disconnected(frame)) => {
            if is_retry {
                publish_pressure.record_pending_retry();
                send_frame_publish_pressure_event(diagnostic_tx, publish_pressure.snapshot());
            }
            tracing::warn!(
                handle_id = frame.resource_handle.0,
                "Player thread dropped decoded frame receiver"
            );
            *pending_publish = Some(PendingFramePublish {
                frame,
                publish_started_at: pending_frame.publish_started_at,
                has_seen_channel_full: pending_frame.has_seen_channel_full,
            });
            false
        }
    }
}

/// Отправляет cumulative publish-pressure snapshot без блокировки decoder thread-а.
fn send_frame_publish_pressure_event(
    diagnostic_tx: &DecoderDiagnosticSender,
    pressure: VideoFramePublishPressureDiagnostics,
) {
    let _ = diagnostic_tx
        .try_send(VideoDecoderDiagnosticEvent::DecodedFramePublishPressure { pressure });
}

/// Освобождает frame, который decoder уже импортировал, но не успел отдать worker-у.
fn release_pending_publish_frame(
    decoder: &mut crate::VaapiVideoDecoder,
    pending_publish: Option<PendingFramePublish>,
) {
    let Some(pending_frame) = pending_publish else {
        return;
    };

    if let Err(error) = decoder.release_frame(pending_frame.frame.resource_handle) {
        tracing::warn!(
            error = %error,
            handle_id = pending_frame.frame.resource_handle.0,
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
    use video_core::{DecodedPixelFormat, FrameMemoryPath, FrameResourceHandle};

    /// Создаёт decoded frame без реальных GPU resources для channel-level тестов.
    fn decoded_frame_for_tests(handle_id: u64) -> DecodedFrame {
        DecodedFrame {
            generation: 0,
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
            resource_handle: FrameResourceHandle(handle_id),
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

    /// Проверяет, что texture view lookup считает lock wait даже при missing views.
    #[test]
    fn resource_lookup_reports_lock_wait_without_changing_missing_views_semantics() {
        let resource_pool = Mutex::new(crate::resource_pool::FrameResourcePool::new());
        let lock_started_at = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("test start instant should allow small subtraction");

        let lookup = resource_lookup_from_pool_started_at(
            &resource_pool,
            FrameResourceHandle(99),
            lock_started_at,
        );

        match lookup {
            VideoFrameResourceLookup::Missing { lock_diagnostics } => {
                assert!(lock_diagnostics.wait >= Duration::from_millis(1));
            }
            VideoFrameResourceLookup::Ready { .. }
            | VideoFrameResourceLookup::Busy { .. }
            | VideoFrameResourceLookup::Fatal { .. } => {
                panic!("missing handle should keep missing semantics");
            }
        }
    }

    /// Проверяет, что non-blocking lookup возвращает Busy, пока resource pool lock удержан.
    #[test]
    fn try_resource_lookup_reports_busy_when_resource_pool_lock_is_held() {
        let resource_pool = Mutex::new(crate::resource_pool::FrameResourcePool::new());
        let _held_resource_pool_lock = resource_pool
            .lock()
            .expect("test mutex should lock before try lookup");
        let lock_started_at = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("test start instant should allow small subtraction");

        let lookup = try_resource_lookup_from_pool_started_at(
            &resource_pool,
            FrameResourceHandle(99),
            lock_started_at,
        );

        match lookup {
            VideoFrameResourceLookup::Busy { lock_diagnostics } => {
                assert!(lock_diagnostics.wait >= Duration::from_millis(1));
            }
            VideoFrameResourceLookup::Ready { .. }
            | VideoFrameResourceLookup::Missing { .. }
            | VideoFrameResourceLookup::Fatal { .. } => {
                panic!("held mutex should produce busy without get_views");
            }
        }
    }

    /// Проверяет, что non-blocking Missing остаётся отличимым от Busy.
    #[test]
    fn try_resource_lookup_keeps_missing_distinct_from_busy() {
        let resource_pool = Mutex::new(crate::resource_pool::FrameResourcePool::new());

        let lookup = try_resource_lookup_from_pool(&resource_pool, FrameResourceHandle(123));

        match lookup {
            VideoFrameResourceLookup::Missing { .. } => {}
            VideoFrameResourceLookup::Ready { .. }
            | VideoFrameResourceLookup::Busy { .. }
            | VideoFrameResourceLookup::Fatal { .. } => {
                panic!("available pool with unknown handle should be missing");
            }
        }
    }

    /// Проверяет, что poisoned mutex остаётся ошибочным состоянием, а не Busy/Missing.
    #[test]
    fn try_resource_lookup_reports_fatal_when_resource_pool_mutex_is_poisoned() {
        let resource_pool = Arc::new(Mutex::new(crate::resource_pool::FrameResourcePool::new()));
        let poison_resource_pool = Arc::clone(&resource_pool);
        let _ = std::thread::spawn(move || {
            let _held_resource_pool_lock = poison_resource_pool
                .lock()
                .expect("test mutex should lock before poisoning");
            panic!("poison resource pool mutex for lookup test");
        })
        .join();

        let lookup =
            try_resource_lookup_from_pool(resource_pool.as_ref(), FrameResourceHandle(123));

        match lookup {
            VideoFrameResourceLookup::Fatal { .. } => {}
            VideoFrameResourceLookup::Ready { .. }
            | VideoFrameResourceLookup::Busy { .. }
            | VideoFrameResourceLookup::Missing { .. } => {
                panic!("poisoned mutex should preserve error semantics");
            }
        }
    }

    /// Проверяет bounded decoded-frame publish: full channel не дропает frame молча.
    #[test]
    fn frame_publish_keeps_pending_frame_when_channel_is_full() {
        let (frame_tx, frame_rx) = bounded(1);
        let (diagnostic_tx, diagnostic_rx) = std::sync::mpsc::sync_channel(4);
        let mut publish_pressure = FramePublishPressureCounters::default();
        frame_tx
            .try_send(decoded_frame_for_tests(1))
            .expect("test channel has one free slot");
        let mut pending_publish = Some(PendingFramePublish::new(decoded_frame_for_tests(2)));

        assert!(publish_pending_frame(
            &frame_tx,
            &mut pending_publish,
            &mut publish_pressure,
            &diagnostic_tx,
        ));
        assert!(pending_publish.is_some());
        let first_pressure = match diagnostic_rx
            .try_recv()
            .expect("full frame channel should emit pressure diagnostics")
        {
            VideoDecoderDiagnosticEvent::DecodedFramePublishPressure { pressure } => pressure,
            VideoDecoderDiagnosticEvent::FrameDropped { .. } => {
                panic!("publish pressure test should not emit frame drop diagnostics")
            }
        };
        assert_eq!(first_pressure.frame_publish_channel_full_count, 1);
        assert_eq!(first_pressure.pending_publish_retry_count, 0);
        assert_eq!(
            frame_rx.try_recv().unwrap().resource_handle,
            FrameResourceHandle(1)
        );

        assert!(publish_pending_frame(
            &frame_tx,
            &mut pending_publish,
            &mut publish_pressure,
            &diagnostic_tx,
        ));
        assert!(pending_publish.is_none());
        let retry_pressure = match diagnostic_rx
            .try_recv()
            .expect("successful retry should emit retry diagnostics")
        {
            VideoDecoderDiagnosticEvent::DecodedFramePublishPressure { pressure } => pressure,
            VideoDecoderDiagnosticEvent::FrameDropped { .. } => {
                panic!("publish retry test should not emit frame drop diagnostics")
            }
        };
        assert_eq!(retry_pressure.frame_publish_channel_full_count, 1);
        assert_eq!(retry_pressure.pending_publish_retry_count, 1);
        let published_frame = frame_rx.try_recv().unwrap();
        assert_eq!(published_frame.resource_handle, FrameResourceHandle(2));
        assert!(
            published_frame
                .diagnostics
                .timings
                .decoded_frame_publish_latency
                .is_some()
        );
        assert_eq!(
            retry_pressure.max_decoded_frame_publish_latency,
            retry_pressure.total_decoded_frame_publish_latency
        );
    }

    /// Проверяет, что control-channel pressure counters различают release и flush failures.
    #[test]
    fn control_channel_pressure_counts_full_failures_by_operation() {
        let (control_tx, _control_rx) = bounded(1);
        let pressure_counters = DecoderControlChannelPressureCounters::default();
        if control_tx
            .try_send(ThreadControlMsg::ReleaseZeroCopy(FrameResourceHandle(1)))
            .is_err()
        {
            panic!("test control channel has one free slot before pressure setup");
        }

        let release_error =
            match control_tx.try_send(ThreadControlMsg::ReleaseZeroCopy(FrameResourceHandle(2))) {
                Ok(()) => panic!("full control channel must reject release message"),
                Err(error) => error,
            };
        let release_message = record_decoder_control_send_failure(
            DecoderControlOperation::Release,
            &control_tx,
            &pressure_counters,
            &release_error,
        );

        assert!(release_message.contains("zero-copy release"));
        let after_release = pressure_counters.snapshot(&control_tx);
        assert_eq!(after_release.control_channel_len, 1);
        assert_eq!(after_release.control_channel_capacity, 1);
        assert_eq!(after_release.control_channel_full_count, 1);
        assert_eq!(after_release.release_control_send_fail_count, 1);
        assert_eq!(after_release.flush_control_send_fail_count, 0);

        let (done_tx, _done_rx) = bounded(1);
        let flush_error = match control_tx.try_send(ThreadControlMsg::Flush(done_tx)) {
            Ok(()) => panic!("full control channel must reject flush message"),
            Err(error) => error,
        };
        let flush_message = record_decoder_control_send_failure(
            DecoderControlOperation::Flush,
            &control_tx,
            &pressure_counters,
            &flush_error,
        );

        assert!(flush_message.contains("decoder flush"));
        let after_flush = pressure_counters.snapshot(&control_tx);
        assert_eq!(after_flush.control_channel_len, 1);
        assert_eq!(after_flush.control_channel_capacity, 1);
        assert_eq!(after_flush.control_channel_full_count, 2);
        assert_eq!(after_flush.release_control_send_fail_count, 1);
        assert_eq!(after_flush.flush_control_send_fail_count, 1);
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
                        dts: None,
                        track_dts: None,
                        generation: packet_index,
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
