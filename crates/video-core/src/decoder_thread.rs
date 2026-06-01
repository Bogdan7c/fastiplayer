use std::time::Duration;

use bytes::Bytes;
use codec_core::{
    BitDepth, ChromaSubsampling, H264Packetization, VideoCodec, VideoColorMetadata,
    VideoDecodeRequirement, VideoMemoryContract, VideoProfile, VideoSurfaceFormat,
};
use media_core::{TrackId, TrackTimestamp};

/// Codec-specific framing, которое decoder backend не должен угадывать из bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoStreamPacketization {
    /// H.264/AVC packetization, подтверждённая codec adapter-ом.
    H264(H264Packetization),
}

/// Нейтральная конфигурация выбранного video stream-а для decoder backend-а.
///
/// `player-core` строит этот объект после выбора track-а, а concrete backend
/// решает, может ли он переиспользовать текущий codec instance или нужна
/// reconfiguration. Seek/flush/generation lifecycle сюда намеренно не входит.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoStreamDecodeConfig {
    /// Track ID выбранного video stream-а внутри текущего media source.
    pub track_id: TrackId,

    /// Codec stream-а без container-specific строк.
    pub codec: VideoCodec,

    /// Codec profile, если container/adapter уже подтвердил его до decode.
    pub profile: Option<VideoProfile>,

    /// Bit depth decoded surface-а, если он известен до decode.
    pub bit_depth: Option<BitDepth>,

    /// Chroma subsampling decoded surface-а, если он известен до decode.
    pub chroma: Option<ChromaSubsampling>,

    /// Coded width stream-а, если metadata уже надёжна.
    pub coded_width: Option<u32>,

    /// Coded height stream-а, если metadata уже надёжна.
    pub coded_height: Option<u32>,

    /// Expected decoded surface format на renderer/backend boundary.
    pub surface_format: Option<VideoSurfaceFormat>,

    /// Required decoded memory path; production policy остаётся hardware zero-copy.
    pub memory_contract: VideoMemoryContract,

    /// Container codec-private bytes, например MP4/MKV `avcC` для H.264.
    pub codec_private: Option<Bytes>,

    /// Явная packetization/framing metadata, если codec adapter уже подтвердил её.
    pub packetization: Option<VideoStreamPacketization>,
}

impl VideoStreamDecodeConfig {
    /// Создаёт stream config из уже принятого capability requirement.
    #[must_use]
    pub fn from_requirement(track_id: TrackId, requirement: &VideoDecodeRequirement) -> Self {
        Self {
            track_id,
            codec: requirement.codec,
            profile: requirement.profile,
            bit_depth: requirement.bit_depth,
            chroma: requirement.chroma,
            coded_width: requirement.width,
            coded_height: requirement.height,
            surface_format: requirement.surface_format,
            memory_contract: requirement.memory_contract,
            codec_private: None,
            packetization: None,
        }
    }

    /// Добавляет container codec-private bytes без копирования payload-а.
    #[must_use]
    pub fn with_codec_private(mut self, codec_private: Option<Bytes>) -> Self {
        self.codec_private = codec_private;
        self
    }

    /// Добавляет подтверждённое codec adapter-ом framing metadata.
    #[must_use]
    pub const fn with_packetization(
        mut self,
        packetization: Option<VideoStreamPacketization>,
    ) -> Self {
        self.packetization = packetization;
        self
    }
}

/// Typed причина, по которой backend отказался от stream configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoStreamConfigRejection {
    /// Backend не реализует adapter для этого codec-а.
    UnsupportedCodec { codec: VideoCodec },

    /// Backend не поддерживает указанный codec profile.
    UnsupportedProfile {
        /// Profile, который требовал stream config.
        profile: VideoProfile,
    },

    /// Backend не поддерживает указанную bit depth.
    UnsupportedBitDepth {
        /// Bit depth, которую требовал stream config.
        bit_depth: BitDepth,
    },

    /// Backend не поддерживает указанную chroma subsampling.
    UnsupportedChroma {
        /// Chroma subsampling, которую требовал stream config.
        chroma: ChromaSubsampling,
    },

    /// Backend не может выдать нужный decoded surface format.
    UnsupportedSurfaceFormat {
        /// Surface format, который требовал stream config.
        surface_format: VideoSurfaceFormat,
    },

    /// Backend не может выполнить требуемый decoded memory contract.
    UnsupportedMemoryContract {
        /// Memory contract, который требовал stream config.
        memory_contract: VideoMemoryContract,
    },

    /// Для codec-а нужна packetization metadata, но она ещё не подтверждена.
    MissingPacketization { codec: VideoCodec },

    /// Codec-private bytes есть, но adapter отверг их как непригодные для config.
    InvalidCodecPrivate {
        /// Codec, для которого parsing codec-private завершился ошибкой.
        codec: VideoCodec,

        /// Текст typed adapter error-а без concrete backend-типа.
        reason: String,
    },

    /// Backend-specific отказ, который пока не имеет отдельной neutral категории.
    BackendUnsupported { reason: String },
}

impl std::fmt::Display for VideoStreamConfigRejection {
    /// Печатает отказ так, чтобы player/UI видел actionable причину.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedCodec { codec } => {
                write!(formatter, "decoder backend does not support {codec}")
            }
            Self::UnsupportedProfile { profile } => {
                write!(
                    formatter,
                    "decoder backend does not support profile {profile}"
                )
            }
            Self::UnsupportedBitDepth { bit_depth } => {
                write!(formatter, "decoder backend does not support {bit_depth}")
            }
            Self::UnsupportedChroma { chroma } => {
                write!(
                    formatter,
                    "decoder backend does not support chroma subsampling {chroma}"
                )
            }
            Self::UnsupportedSurfaceFormat { surface_format } => {
                write!(
                    formatter,
                    "decoder backend does not support surface format {surface_format}"
                )
            }
            Self::UnsupportedMemoryContract { memory_contract } => {
                write!(
                    formatter,
                    "decoder backend does not support memory contract {memory_contract:?}"
                )
            }
            Self::MissingPacketization { codec } => {
                write!(formatter, "{codec} stream packetization is not confirmed")
            }
            Self::InvalidCodecPrivate { codec, reason } => {
                write!(formatter, "{codec} codec-private is invalid: {reason}")
            }
            Self::BackendUnsupported { reason } => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for VideoStreamConfigRejection {}

/// Backpressure control-команд decoder thread-а, отдельно от packet queue pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoDecoderControlBackpressureReason {
    /// Bounded control channel заполнен: release/flush/config/drain команды ждут обработки.
    ControlChannelFull {
        /// Текущая глубина control channel.
        queued_messages: usize,

        /// Bounded capacity control channel-а.
        capacity: usize,
    },
}

impl std::fmt::Display for VideoDecoderControlBackpressureReason {
    /// Печатает control-channel pressure без потери чисел очереди.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ControlChannelFull {
                queued_messages,
                capacity,
            } => write!(
                formatter,
                "decoder control channel is full: queued={queued_messages}, capacity={capacity}"
            ),
        }
    }
}

/// Результат configure/clear stream boundary без схлопывания важных состояний.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoStreamConfigResult {
    /// Decoder thread отсутствует; caller может сохранить selection и настроить новый backend позже.
    AbsentDecoder,

    /// Backend принял новую stream configuration.
    Configured,

    /// Backend уже был настроен на эквивалентный stream.
    Unchanged,

    /// Backend очистил текущую stream configuration.
    Cleared,

    /// Backend жив, но не поддерживает запрошенный stream config.
    Unsupported(VideoStreamConfigRejection),

    /// Bounded control channel временно не принял command.
    Backpressure(VideoDecoderControlBackpressureReason),

    /// Backend fail-closed или configuration command завершилась fatal ошибкой.
    Fatal(DecodeThreadError),
}

impl VideoStreamConfigResult {
    /// Возвращает true для результатов, после которых caller может продолжать selection.
    #[must_use]
    pub const fn is_accepted(&self) -> bool {
        matches!(
            self,
            Self::AbsentDecoder | Self::Configured | Self::Unchanged | Self::Cleared
        )
    }
}

/// Нейтральное состояние explicit decoder EOF/DPB drain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoDecoderEndOfStreamDrainState {
    /// Decoder не дожимает tail frames.
    Idle,

    /// Decoder принял EOF drain и может ещё публиковать tail frames обычным frame path-ом.
    Draining {
        /// Seek generation, к которому относится drain request.
        generation: u64,
    },

    /// Decoder подтвердил, что tail frames для generation полностью отданы/отсутствуют.
    Drained {
        /// Seek generation, для которого drain завершён.
        generation: u64,
    },

    /// Decoder drain завершился fatal ошибкой.
    Fatal {
        /// Generation request-а, если backend успел его зафиксировать.
        generation: Option<u64>,

        /// Root cause, которую player layer должен показать как runtime failure.
        error: DecodeThreadError,
    },
}

/// Результат запуска explicit decoder EOF/DPB drain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoDecoderEndOfStreamDrainResult {
    /// Decoder thread отсутствует; caller не должен считать это seek flush-ом.
    AbsentDecoder,

    /// Backend принял request; новое state возвращено явно.
    Started(VideoDecoderEndOfStreamDrainState),

    /// Эквивалентный drain уже был активен или завершён.
    Unchanged(VideoDecoderEndOfStreamDrainState),

    /// Bounded control channel временно не принял drain command.
    Backpressure(VideoDecoderControlBackpressureReason),

    /// Backend fail-closed или drain command завершилась fatal ошибкой.
    Fatal(DecodeThreadError),
}

/// Encoded video packet на границе player/session -> decoder backend.
#[derive(Debug, Clone)]
pub struct DecodePacket {
    /// Track ID выбранного video stream.
    pub track_id: TrackId,

    /// Presentation timestamp packet-а на media timeline.
    pub pts: Duration,

    /// Decode timestamp на media timeline, если container сообщил DTS отдельно от PTS.
    pub dts: Option<Duration>,

    /// Исходный signed DTS в track time base для codec backends, которым нужен decode order.
    pub track_dts: Option<TrackTimestamp>,

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

    /// Настраивает codec-specific stream state без изменения seek generation или pending queues.
    fn configure_stream(&self, _config: VideoStreamDecodeConfig) -> VideoStreamConfigResult {
        VideoStreamConfigResult::Unsupported(VideoStreamConfigRejection::BackendUnsupported {
            reason: format!(
                "{} decoder handle does not implement stream configuration",
                self.backend_name()
            ),
        })
    }

    /// Очищает codec-specific stream state при media switch/backend lifecycle reset.
    fn clear_stream(&self) -> VideoStreamConfigResult {
        VideoStreamConfigResult::Unchanged
    }

    /// Запускает explicit EOF/DPB drain отдельно от seek `flush`.
    fn begin_end_of_stream_drain(&self, generation: u64) -> VideoDecoderEndOfStreamDrainResult {
        VideoDecoderEndOfStreamDrainResult::Started(VideoDecoderEndOfStreamDrainState::Drained {
            generation,
        })
    }

    /// Возвращает текущее состояние explicit EOF/DPB drain.
    fn end_of_stream_drain_state(&self) -> VideoDecoderEndOfStreamDrainState {
        VideoDecoderEndOfStreamDrainState::Idle
    }

    /// Освобождает texture/surface handle после presentation/drop.
    fn release_frame(&self, handle: crate::FrameResourceHandle);

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
