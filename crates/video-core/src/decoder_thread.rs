use std::time::Duration;

use bytes::Bytes;
use codec_core::VideoColorMetadata;
use media_core::TrackId;

/// Encoded video packet на границе player/session -> decoder backend.
#[derive(Debug, Clone)]
pub struct DecodePacket {
    /// Track ID выбранного video stream.
    pub track_id: TrackId,

    /// Presentation timestamp packet-а на media timeline.
    pub pts: Duration,

    /// Seek generation player pipeline-а, которому принадлежит packet.
    pub generation: u64,

    /// Encoded bytes без привязки к конкретному hardware backend-у.
    pub encoded_bytes: Bytes,

    /// Keyframe flag из container/demuxer.
    pub keyframe: bool,

    /// Resolved color metadata, которую decoder переносит в decoded frame contract.
    pub resolved_color: Option<VideoColorMetadata>,
}

/// Fatal ошибка decoder thread, которую player layer должен показать как runtime failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeThreadError {
    /// Человекочитаемая причина остановки decoder backend-а.
    message: String,
}

impl DecodeThreadError {
    /// Создаёт ошибку decoder boundary без backend-specific типа.
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

/// Typed причина, по которой decoder backend временно не принимает packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeBackpressureReason {
    /// Bounded packet queue заполнена: decoder ещё не забрал старые packets.
    PacketQueueFull {
        /// Текущая глубина packet queue.
        queued_packets: usize,

        /// Bounded capacity packet queue.
        capacity: usize,
    },
}

impl std::fmt::Display for DecodeBackpressureReason {
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

/// Ошибка постановки packet-а в decoder backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeSendError {
    /// Decoder backend жив, но bounded queue сейчас заполнена.
    Backpressure(DecodeBackpressureReason),

    /// Decoder backend уже fail-closed или receiver отключён.
    Fatal(DecodeThreadError),
}

impl std::fmt::Display for DecodeSendError {
    /// Печатает machine-actionable причину отправки packet-а.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backpressure(reason) => write!(formatter, "{reason}"),
            Self::Fatal(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for DecodeSendError {}

/// Backend-neutral snapshot decoder/render ресурсов для diagnostics и backpressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderResourceSnapshot {
    /// Максимальное число persistent texture/import slots.
    pub capacity: usize,

    /// Сколько persistent texture/import slots сейчас создано.
    pub slots: usize,

    /// Сколько surfaces сейчас нельзя переиспользовать decoder-у.
    pub in_use: usize,

    /// Сколько imported surfaces свободно для reuse.
    pub free_surfaces: usize,

    /// Сколько releases ждёт GPU completion callback.
    pub waiting_gpu_completion: usize,

    /// Сколько releases ждёт возврата decoded handle в decoder pool.
    pub waiting_decoder_reuse: usize,

    /// Сколько external imports завершилось ошибкой.
    pub import_failures: u64,

    /// Сколько external imports реально создано.
    pub imports_created: u64,

    /// Сколько кадров переиспользовало existing import.
    pub imports_reused: u64,

    /// Сколько free imports было заменено из-за смены backing object/layout.
    pub imports_replaced: u64,
}

impl DecoderResourceSnapshot {
    /// Возвращает число slots, которые ещё можно занять или переиспользовать.
    #[must_use]
    pub const fn available_slots(self) -> usize {
        self.capacity.saturating_sub(self.in_use)
    }
}

/// Snapshot давления на decoder control channel без backend-specific типов.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoDecoderControlChannelPressureSnapshot {
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

/// Production default packet channel capacity между worker и decoder thread.
const DEFAULT_DECODER_PACKET_CHANNEL_FRAMES: usize = 32;

/// Production default decoded frame channel capacity между decoder thread и worker.
const DEFAULT_DECODER_FRAME_CHANNEL_FRAMES: usize = 8;

/// Production default control/release channel capacity decoder thread-а.
const DEFAULT_DECODER_CONTROL_CHANNEL_FRAMES: usize = 32;

/// Production default backend-local ready queue capacity.
const DEFAULT_DECODER_READY_QUEUE_FRAMES: usize = 8;

/// Production default output surface descriptor pool size.
const DEFAULT_DECODER_SURFACE_POOL_FRAMES: usize = 24;

/// Production default zero-copy external import slot capacity.
const DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS: usize = 24;

/// Env-переменная для настройки flush timeout-а без перекомпиляции приложения.
const DECODER_FLUSH_TIMEOUT_ENV_VAR: &str = "VIDEOPLAYER_DECODER_FLUSH_TIMEOUT_MS";

/// Production default flush timeout decoder thread-а в миллисекундах.
const DEFAULT_DECODER_FLUSH_TIMEOUT_MS: u64 = 2_000;

/// Backend-neutral runtime limits decoder thread-а.
///
/// Тип живёт в `video-core`, чтобы player/session code зависел от contract,
/// а конкретный backend получал эти значения через adapter conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoDecoderThreadConfig {
    /// Packet channel capacity между worker и decoder thread.
    pub packet_channel_frames: usize,

    /// Decoded frame channel capacity между decoder thread и worker.
    pub frame_channel_frames: usize,

    /// Control/release channel capacity для release/flush сообщений.
    pub control_channel_frames: usize,

    /// Backend-local ready queue capacity внутри decoder wrapper-а.
    pub decoder_ready_queue_frames: usize,

    /// Hardware decoder output surface descriptor pool size.
    pub decoder_surface_pool_frames: usize,

    /// Zero-copy external import slot capacity.
    pub zero_copy_surface_pool_slots: usize,

    /// Максимальное время ожидания подтверждения flush от decoder thread.
    pub flush_timeout: Duration,
}

impl VideoDecoderThreadConfig {
    /// Загружает production defaults и overlay flush timeout-а из окружения.
    #[must_use]
    pub fn from_env() -> Self {
        let flush_timeout = match std::env::var(DECODER_FLUSH_TIMEOUT_ENV_VAR) {
            Ok(raw_value) => match Self::parse_flush_timeout(&raw_value) {
                Ok(timeout) => timeout,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        env_var = DECODER_FLUSH_TIMEOUT_ENV_VAR,
                        default_timeout_ms = DEFAULT_DECODER_FLUSH_TIMEOUT_MS,
                        "Invalid decoder flush timeout config; using default"
                    );
                    Self::default_flush_timeout()
                }
            },
            Err(std::env::VarError::NotPresent) => Self::default_flush_timeout(),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    env_var = DECODER_FLUSH_TIMEOUT_ENV_VAR,
                    default_timeout_ms = DEFAULT_DECODER_FLUSH_TIMEOUT_MS,
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
    const fn default_flush_timeout() -> Duration {
        Duration::from_millis(DEFAULT_DECODER_FLUSH_TIMEOUT_MS)
    }

    /// Парсит env timeout в миллисекундах без изменения process env в тестах.
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

    /// Нормализует direct API values, чтобы startup не получил zero-capacity queues.
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
}

impl Default for VideoDecoderThreadConfig {
    /// Возвращает production defaults без unbounded очередей.
    fn default() -> Self {
        Self {
            packet_channel_frames: DEFAULT_DECODER_PACKET_CHANNEL_FRAMES,
            frame_channel_frames: DEFAULT_DECODER_FRAME_CHANNEL_FRAMES,
            control_channel_frames: DEFAULT_DECODER_CONTROL_CHANNEL_FRAMES,
            decoder_ready_queue_frames: DEFAULT_DECODER_READY_QUEUE_FRAMES,
            decoder_surface_pool_frames: DEFAULT_DECODER_SURFACE_POOL_FRAMES,
            zero_copy_surface_pool_slots: DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS,
            flush_timeout: VideoDecoderThreadConfig::default_flush_timeout(),
        }
    }
}

/// Decoder-thread contract, который не зависит от конкретного hardware API.
pub trait VideoDecoderThreadHandle: Send {
    /// Renderer/resource provider, который decoder отдаёт владельцу presentation path.
    type ResourceProvider: Clone + Send + Sync + 'static;

    /// Возвращает человекочитаемое имя backend-а для snapshot/diagnostics.
    fn backend_name(&self) -> &'static str;

    /// Отправляет encoded packet в decoder thread.
    fn send_packet(&self, packet: DecodePacket) -> Result<(), DecodeSendError>;

    /// Освобождает texture/surface handle после presentation/drop.
    fn release_frame(&self, handle: crate::FrameTextureHandle);

    /// Забирает следующий decoded frame без блокировки worker-а.
    fn try_recv_frame(&self) -> Option<crate::DecodedFrame>;

    /// Забирает backend diagnostics event без блокировки worker-а.
    fn try_recv_diagnostic_event(&self) -> Option<crate::VideoDecoderDiagnosticEvent>;

    /// Забирает fatal decoder-thread error, если backend остановился.
    fn try_recv_error(&self) -> Option<DecodeThreadError>;

    /// Сбрасывает decoder state перед seek transaction.
    fn flush(&self) -> anyhow::Result<()>;

    /// Возвращает provider для renderer-side resource lookup/release path.
    fn resource_provider(&self) -> Self::ResourceProvider;

    /// Возвращает snapshot texture/resource pool-а для UI/backpressure diagnostics.
    fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot>;

    /// Возвращает snapshot bounded control channel-а для diagnostics.
    fn decoder_control_channel_pressure(
        &self,
    ) -> Option<VideoDecoderControlChannelPressureSnapshot> {
        None
    }

    /// Возвращает глубину packet channel-а внутри decoder thread.
    fn packet_queue_depth(&self) -> usize;

    /// Забирает количество packets, обработанных decoder thread-ом.
    fn drain_completed_packet_count(&self) -> usize;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что direct API caller не может создать zero-capacity очереди.
    #[test]
    fn decoder_thread_config_normalizes_zero_limits() {
        let config = VideoDecoderThreadConfig {
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

    /// Проверяет parsing policy env timeout-а без изменения process env.
    #[test]
    fn decoder_thread_config_flush_timeout_parser_rejects_invalid_values() {
        assert!(VideoDecoderThreadConfig::parse_flush_timeout("0").is_err());
        assert!(VideoDecoderThreadConfig::parse_flush_timeout("abc").is_err());
        assert_eq!(
            VideoDecoderThreadConfig::parse_flush_timeout("25").unwrap(),
            Duration::from_millis(25)
        );
    }

    /// Проверяет, что public error contract сохраняет текст root cause.
    #[test]
    fn decode_thread_error_exposes_message_for_player_layer() {
        let error = DecodeThreadError::new("P010 DMA-BUF zero-copy import failed");

        assert_eq!(error.message(), "P010 DMA-BUF zero-copy import failed");
        assert_eq!(error.to_string(), "P010 DMA-BUF zero-copy import failed");
    }

    /// Проверяет accounting helper без underflow при переполненном resource pool.
    #[test]
    fn decoder_resource_snapshot_available_slots_saturates() {
        let snapshot = DecoderResourceSnapshot {
            capacity: 2,
            slots: 2,
            in_use: 3,
            free_surfaces: 0,
            waiting_gpu_completion: 0,
            waiting_decoder_reuse: 0,
            import_failures: 0,
            imports_created: 0,
            imports_reused: 0,
            imports_replaced: 0,
        };

        assert_eq!(snapshot.available_slots(), 0);
    }
}
