//! Декларативная граница VA-API decoder thread.
//!
//! Модуль владеет bounded defaults, backend-neutral projections и shared
//! fail-closed состоянием frontend-а. Создание каналов, spawn и runtime loop
//! остаются у родительского `decoder_thread`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use codec_core::VideoColorMetadata;
use media_core::{TrackId, TrackTimestamp};
use video_core::VideoDecoderDiagnosticEvent;

use crate::decoder::VaapiDecoderRuntimeConfig;
use crate::resource_pool::{DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS, ResourcePoolStats};

/// Подтверждение, что decoder thread уже обработал один packet из input channel.
pub(super) type DecodePacketAck = ();

/// Bounded capacity diagnostics events от decoder thread.
pub(super) const DECODER_DIAGNOSTIC_CHANNEL_CAPACITY: usize = 256;

/// Sender typed diagnostics events без зависимости decoder thread-а от player-core.
pub(super) type DecoderDiagnosticSender = std::sync::mpsc::SyncSender<VideoDecoderDiagnosticEvent>;

/// Receiver typed diagnostics events для player-core drain boundary.
pub(super) type DecoderDiagnosticReceiver = std::sync::mpsc::Receiver<VideoDecoderDiagnosticEvent>;

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
pub(super) const DECODER_FRAME_PUBLISH_RETRY_MS: u64 = 2;

/// Runtime limits decoder thread boundary.
///
/// Все очереди bounded: packet queue даёт demux/decode burst headroom, frame
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
    pub(super) fn parse_flush_timeout(raw_value: &str) -> anyhow::Result<Duration> {
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
    pub(super) fn vaapi_decoder_config(self) -> VaapiDecoderRuntimeConfig {
        VaapiDecoderRuntimeConfig::from_surface_accounting(
            self.decoder_surface_pool_frames,
            self.decoder_ready_queue_frames,
        )
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

/// Shared fail-closed состояние decoder thread frontend-а.
#[derive(Clone, Debug)]
pub(super) struct DecoderThreadState {
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
    pub(super) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(DecoderThreadStateInner::default())),
        }
    }

    /// Сохраняет первую fatal ошибку и возвращает именно сохранённый root cause.
    pub(super) fn mark_fatal(&self, error: DecodeThreadError) -> DecodeThreadError {
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
    pub(super) fn current_error(&self) -> Option<DecodeThreadError> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.fatal_error.clone()
    }

    /// Отдаёт fatal ошибку в player layer ровно один раз.
    pub(super) fn take_pending_error(&self) -> Option<DecodeThreadError> {
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
            // VA-API декодирует по media PTS и не владеет raw container PTS metadata.
            track_pts: None,
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
            // software_frame_pool_frames — software-only limit; у VA-API нет
            // host-frame pool, поэтому возвращаем neutral default, а не VA surface
            // pool. Hardware-путь этот лимит не использует.
            software_frame_pool_frames: video_core::VideoDecoderThreadConfig::default()
                .software_frame_pool_frames,
            software_decode_thread_budget: video_core::SoftwareDecodeThreadBudget::auto(),
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
