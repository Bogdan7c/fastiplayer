use bytes::Bytes;
use codec_core::{
    BitDepth, ChromaSubsampling, H264Packetization, H265Packetization, VideoCodec,
    VideoColorMetadata, VideoDecodeRequirement, VideoDisplayOrientation, VideoProfile,
};
use media_core::{TrackId, TrackTimestamp};
use video_frame_contract::{VideoFrameContract, VideoFramePixelLayout};

/// Codec-specific framing, которое decoder backend не должен угадывать из bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoStreamPacketization {
    /// H.264/AVC packetization, подтверждённая codec adapter-ом.
    H264(H264Packetization),

    /// H.265/HEVC packetization, подтверждённая codec adapter-ом.
    H265(H265Packetization),
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

    /// Display orientation из container track transform.
    pub display_orientation: VideoDisplayOrientation,

    /// Expected decoded frame/runtime resource contract на decoder->renderer boundary.
    pub frame_contract: VideoFrameContract,

    /// Container codec-private bytes, например MP4/MKV `avcC`/`hvcC`.
    pub codec_private: Option<Bytes>,

    /// Явная packetization/framing metadata, если codec adapter уже подтвердил её.
    pub packetization: Option<VideoStreamPacketization>,
}

impl VideoStreamDecodeConfig {
    /// Создаёт stream config из requirement и выбранного capability output contract-а.
    #[must_use]
    pub fn from_requirement(
        track_id: TrackId,
        requirement: &VideoDecodeRequirement,
        frame_contract: VideoFrameContract,
    ) -> Self {
        Self {
            track_id,
            codec: requirement.codec,
            profile: requirement.profile,
            bit_depth: requirement.bit_depth,
            chroma: requirement.chroma,
            coded_width: requirement.width,
            coded_height: requirement.height,
            display_orientation: VideoDisplayOrientation::Identity,
            frame_contract,
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

    /// Добавляет display orientation, которую renderer применит к decoded frames.
    #[must_use]
    pub const fn with_display_orientation(
        mut self,
        display_orientation: VideoDisplayOrientation,
    ) -> Self {
        self.display_orientation = display_orientation;
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
        surface_format: VideoFramePixelLayout,
    },

    /// Backend не может выполнить требуемый decoded frame transfer/layout contract.
    UnsupportedFrameContract {
        /// Frame contract, который требовал stream config.
        frame_contract: VideoFrameContract,
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
            Self::UnsupportedFrameContract { frame_contract } => {
                write!(
                    formatter,
                    "decoder backend does not support frame contract {}",
                    frame_contract.diagnostic_label()
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

/// Нейтральный decoder-side floor для accurate seek preroll output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoPrerollOutputFloor {
    /// Seek generation, для которой действует этот preroll floor.
    pub generation: u64,

    /// Минимальный presentation timestamp, который decoder должен публиковать наружу.
    pub floor_pts: Duration,

    /// Разрешает backend-у сохранить последний кадр перед floor как preroll reference.
    pub retain_latest_before_floor: bool,
}

/// Нейтральный запрос очистки accurate seek preroll floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoPrerollOutputFloorClear {
    /// Очищает floor только если active generation совпадает с указанной.
    MatchingGeneration(u64),

    /// Очищает любой active floor независимо от generation.
    Any,
}

/// Результат preroll output-floor boundary без схлопывания control states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoPrerollOutputFloorResult {
    /// Decoder thread отсутствует; caller может продолжить seek без decoder-side floor.
    AbsentDecoder,

    /// Backend применил новый preroll output floor.
    Applied,

    /// Backend уже находился в требуемом состоянии или clear не нашёл matching floor.
    Unchanged,

    /// Backend очистил active preroll output floor.
    Cleared,

    /// Backend жив, но не поддерживает decoder-side preroll output floor.
    Unsupported,

    /// Bounded control channel временно не принял command.
    Backpressure(VideoDecoderControlBackpressureReason),

    /// Backend fail-closed или floor command завершилась fatal ошибкой.
    Fatal(DecodeThreadError),
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

    /// Исходный signed PTS в track time base для точной FFmpeg packet time-base проекции.
    pub track_pts: Option<TrackTimestamp>,

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
use std::time::Duration;
