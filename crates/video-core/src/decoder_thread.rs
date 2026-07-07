use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use codec_core::{
    BitDepth, ChromaSubsampling, H264Packetization, H265Packetization, VideoCodec,
    VideoColorMetadata, VideoDecodeRequirement, VideoDisplayOrientation, VideoProfile,
};
use crossbeam_channel::{Receiver, RecvError, RecvTimeoutError, Sender, TrySendError, bounded};
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

/// Software host-upload resource counters без смешивания с VA-API surface/import accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostUploadResourceSnapshot {
    /// Сколько decoded host frames уже готово к upload/presentation path-у.
    pub host_frames_ready: usize,

    /// Сколько host frames уже заняло upload slot и ждёт release.
    pub host_frames_in_flight: usize,

    /// Сколько upload slots может одновременно держать software path.
    pub upload_slots_capacity: usize,

    /// Сколько upload slots сейчас свободно для новых host frames.
    pub upload_slots_free: usize,

    /// Сколько upload попыток завершилось ошибкой.
    pub upload_failures: u64,
}

impl HostUploadResourceSnapshot {
    /// Возвращает typed причину software backpressure без обращения к VA-API counters.
    #[must_use]
    pub const fn backpressure_reason(
        self,
        ready_queue_capacity: usize,
    ) -> Option<HostUploadBackpressureReason> {
        if ready_queue_capacity > 0 && self.host_frames_ready >= ready_queue_capacity {
            return Some(HostUploadBackpressureReason::ReadyQueueFull {
                host_frames_ready: self.host_frames_ready,
                capacity: ready_queue_capacity,
            });
        }

        if self.upload_slots_free == 0 {
            return Some(HostUploadBackpressureReason::UploadSlotsExhausted {
                host_frames_in_flight: self.host_frames_in_flight,
                upload_slots_capacity: self.upload_slots_capacity,
            });
        }

        None
    }
}

/// Typed причина software host-upload backpressure для будущей scheduler integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostUploadBackpressureReason {
    /// Очередь decoded host frames заполнена, decoder не должен публиковать ещё один frame.
    ReadyQueueFull {
        /// Сколько host frames уже ждёт upload/presentation path-а.
        host_frames_ready: usize,

        /// Bounded capacity очереди ready host frames.
        capacity: usize,
    },

    /// Upload slots закончились, поэтому renderer/provider ещё не освободил ресурсы.
    UploadSlotsExhausted {
        /// Сколько host frames уже находится в upload/release lifecycle.
        host_frames_in_flight: usize,

        /// Общая capacity upload slots.
        upload_slots_capacity: usize,
    },
}

/// Typed результат чтения software host-upload snapshot-а через decoder boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostUploadResourceSnapshotStatus {
    /// В `player-core` ещё не установлен video decoder.
    AbsentDecoder,

    /// Decoder установлен, но software host-upload resource boundary ещё отсутствует.
    AbsentResource,

    /// Backend не является software host-upload provider-ом.
    UnsupportedBackend,

    /// Software host-upload resource counters доступны.
    Available(HostUploadResourceSnapshot),
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

/// Monotonic номер последней активности decoder thread-а.
///
/// Epoch отделён от pulse channel-а: bounded pulse может coalesce-иться, но
/// caller всё равно видит последнюю активность через атомарный счётчик.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VideoDecoderActivityEpoch(u64);

impl VideoDecoderActivityEpoch {
    /// Начальный epoch до первой активности decoder thread-а.
    pub const INITIAL: Self = Self(0);

    /// Создаёт epoch из сохранённого числового значения.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает числовое значение для diagnostics/runtime state.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Проверяет, что этот epoch новее ранее observed epoch-а.
    #[must_use]
    pub const fn is_after(self, observed_epoch: Self) -> bool {
        self.0 > observed_epoch.0
    }
}

/// Typed причина, почему decoder activity notifier сейчас недоступен.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoDecoderActivityUnavailableReason {
    /// Backend/handle ещё не реализует activity notifier contract.
    UnsupportedNotifier,

    /// Pulse sender отключён; caller должен выключить этот source до замены backend-а.
    DisconnectedNotifier,

    /// Backend сообщил fatal состояние notifier-а без раскрытия backend-specific канала.
    FatalNotifier(DecodeThreadError),
}

impl std::fmt::Display for VideoDecoderActivityUnavailableReason {
    /// Печатает причину так, чтобы worker diagnostics могли отличать fallback paths.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedNotifier => {
                formatter.write_str("decoder activity notifier is unsupported")
            }
            Self::DisconnectedNotifier => {
                formatter.write_str("decoder activity notifier is disconnected")
            }
            Self::FatalNotifier(error) => {
                write!(formatter, "decoder activity notifier failed: {error}")
            }
        }
    }
}

/// Result ожидания decoder activity без схлопывания timeout/disconnect/stale pulse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoDecoderActivityWaitOutcome {
    /// Epoch продвинулся относительно observed epoch-а.
    ActivityReceived {
        /// Последний известный epoch после activity.
        epoch: VideoDecoderActivityEpoch,
    },

    /// Pulse был получен, но epoch уже был known caller-у.
    NoNewActivityAfterEpoch {
        /// Epoch, относительно которого caller ждал новую activity.
        observed_epoch: VideoDecoderActivityEpoch,

        /// Текущий epoch после drain-а stale/coalesced pulse.
        current_epoch: VideoDecoderActivityEpoch,
    },

    /// Новая activity не появилась до fallback deadline-а.
    Timeout {
        /// Epoch, относительно которого caller ждал новую activity.
        observed_epoch: VideoDecoderActivityEpoch,

        /// Текущий epoch на момент timeout-а.
        current_epoch: VideoDecoderActivityEpoch,
    },

    /// Activity source недоступен; caller должен использовать bounded fallback poll.
    Unavailable {
        /// Typed причина недоступности notifier-а.
        reason: VideoDecoderActivityUnavailableReason,
    },
}

/// Неблокирующая сторона decoder thread-а, которая публикует coalesced activity pulses.
#[derive(Clone)]
pub struct VideoDecoderActivityNotifier {
    /// Общий monotonic epoch, который не теряется при coalescing pulse channel-а.
    shared_epoch: Arc<AtomicU64>,

    /// Bounded pulse channel capacity=1, чтобы decoder thread никогда не копил очередь.
    pulse_tx: Sender<()>,
}

impl VideoDecoderActivityNotifier {
    /// Создаёт связанную пару notifier/subscription для одного decoder activity source-а.
    #[must_use]
    pub fn new() -> (Self, VideoDecoderActivitySubscription) {
        let shared_epoch = Arc::new(AtomicU64::new(VideoDecoderActivityEpoch::INITIAL.get()));
        let (pulse_tx, pulse_rx) = bounded(1);
        (
            Self {
                shared_epoch: Arc::clone(&shared_epoch),
                pulse_tx,
            },
            VideoDecoderActivitySubscription {
                shared_epoch,
                pulse_rx,
            },
        )
    }

    /// Публикует activity без блокировки decoder thread-а.
    ///
    /// Epoch всегда продвигается до попытки отправить pulse. Если bounded pulse
    /// channel уже полон, новый pulse coalesce-ится: caller всё равно увидит
    /// последний epoch через snapshot/check contract.
    #[must_use]
    pub fn notify_activity(&self) -> VideoDecoderActivityEpoch {
        let activity_epoch = self.advance_epoch();
        match self.pulse_tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
        }
        activity_epoch
    }

    /// Атомарно продвигает epoch без wraparound-а.
    fn advance_epoch(&self) -> VideoDecoderActivityEpoch {
        loop {
            let current_epoch = self.shared_epoch.load(Ordering::Acquire);
            let next_epoch = current_epoch.saturating_add(1);
            if next_epoch == current_epoch {
                return VideoDecoderActivityEpoch::new(current_epoch);
            }
            match self.shared_epoch.compare_exchange(
                current_epoch,
                next_epoch,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return VideoDecoderActivityEpoch::new(next_epoch),
                Err(_) => continue,
            }
        }
    }
}

impl std::fmt::Debug for VideoDecoderActivityNotifier {
    /// Печатает только stable diagnostics, не раскрывая внутренние channel handles.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VideoDecoderActivityNotifier")
            .field("current_epoch", &self.shared_epoch.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

/// Подписка worker/caller-а на нейтральные decoder activity pulses.
#[derive(Clone)]
pub struct VideoDecoderActivitySubscription {
    /// Общий monotonic epoch, читаемый перед select и после pulse wakeup-а.
    shared_epoch: Arc<AtomicU64>,

    /// Receiver coalesced pulse channel-а; clone используется только для neutral activity wait.
    pulse_rx: Receiver<()>,
}

impl VideoDecoderActivitySubscription {
    /// Создаёт snapshot, который caller может использовать во время одного planning/wait цикла.
    #[must_use]
    pub fn snapshot(&self) -> VideoDecoderActivitySnapshot {
        VideoDecoderActivitySnapshot::Available {
            captured_epoch: self.current_epoch(),
            subscription: self.clone(),
        }
    }

    /// Возвращает текущий live epoch из shared activity state.
    #[must_use]
    pub fn current_epoch(&self) -> VideoDecoderActivityEpoch {
        VideoDecoderActivityEpoch::new(self.shared_epoch.load(Ordering::Acquire))
    }

    /// Возвращает receiver clone для `select!`, не отдавая backend-specific channels.
    #[must_use]
    pub fn pulse_receiver(&self) -> Receiver<()> {
        self.pulse_rx.clone()
    }

    /// Проверяет activity после observed epoch-а без блокировки.
    #[must_use]
    pub fn activity_since(
        &self,
        observed_epoch: VideoDecoderActivityEpoch,
    ) -> VideoDecoderActivityWaitOutcome {
        let current_epoch = self.current_epoch();
        if current_epoch.is_after(observed_epoch) {
            VideoDecoderActivityWaitOutcome::ActivityReceived {
                epoch: current_epoch,
            }
        } else {
            VideoDecoderActivityWaitOutcome::NoNewActivityAfterEpoch {
                observed_epoch,
                current_epoch,
            }
        }
    }

    /// Классифицирует результат recv branch-а из внешнего `select!`.
    #[must_use]
    pub fn activity_after_recv(
        &self,
        observed_epoch: VideoDecoderActivityEpoch,
        recv_result: Result<(), RecvError>,
    ) -> VideoDecoderActivityWaitOutcome {
        match recv_result {
            Ok(()) => self.activity_since(observed_epoch),
            Err(_) => VideoDecoderActivityWaitOutcome::Unavailable {
                reason: VideoDecoderActivityUnavailableReason::DisconnectedNotifier,
            },
        }
    }

    /// Ждёт activity до fallback timeout-а, предварительно закрывая lost-wakeup окно.
    #[must_use]
    pub fn wait_for_activity_after(
        &self,
        observed_epoch: VideoDecoderActivityEpoch,
        timeout: Duration,
    ) -> VideoDecoderActivityWaitOutcome {
        let immediate_outcome = self.activity_since(observed_epoch);
        if matches!(
            immediate_outcome,
            VideoDecoderActivityWaitOutcome::ActivityReceived { .. }
        ) {
            return immediate_outcome;
        }

        match self.pulse_rx.recv_timeout(timeout) {
            Ok(()) => self.activity_since(observed_epoch),
            Err(RecvTimeoutError::Timeout) => self.timeout_or_late_activity(observed_epoch),
            Err(RecvTimeoutError::Disconnected) => VideoDecoderActivityWaitOutcome::Unavailable {
                reason: VideoDecoderActivityUnavailableReason::DisconnectedNotifier,
            },
        }
    }

    /// После timeout-а повторно читает epoch, чтобы не потерять late activity.
    fn timeout_or_late_activity(
        &self,
        observed_epoch: VideoDecoderActivityEpoch,
    ) -> VideoDecoderActivityWaitOutcome {
        let current_epoch = self.current_epoch();
        if current_epoch.is_after(observed_epoch) {
            VideoDecoderActivityWaitOutcome::ActivityReceived {
                epoch: current_epoch,
            }
        } else {
            VideoDecoderActivityWaitOutcome::Timeout {
                observed_epoch,
                current_epoch,
            }
        }
    }
}

impl std::fmt::Debug for VideoDecoderActivitySubscription {
    /// Печатает только stable diagnostics, не раскрывая внутренний receiver.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VideoDecoderActivitySubscription")
            .field("current_epoch", &self.current_epoch())
            .finish_non_exhaustive()
    }
}

/// Snapshot activity boundary-а, который можно безопасно передать в planning/wait код.
#[derive(Debug, Clone)]
pub enum VideoDecoderActivitySnapshot {
    /// Activity source доступен; captured_epoch отражает момент создания snapshot-а.
    Available {
        /// Epoch, увиденный при создании snapshot-а.
        captured_epoch: VideoDecoderActivityEpoch,

        /// Clone нейтральной subscription, через которую caller ждёт pulse.
        subscription: VideoDecoderActivitySubscription,
    },

    /// Activity source недоступен; caller должен выбрать bounded fallback poll.
    Unavailable {
        /// Typed причина, почему wait через activity невозможен.
        reason: VideoDecoderActivityUnavailableReason,
    },
}

impl VideoDecoderActivitySnapshot {
    /// Создаёт snapshot недоступного notifier-а с typed reason.
    #[must_use]
    pub const fn unavailable(reason: VideoDecoderActivityUnavailableReason) -> Self {
        Self::Unavailable { reason }
    }

    /// Создаёт default unsupported snapshot для backend-ов без activity contract-а.
    #[must_use]
    pub const fn unsupported() -> Self {
        Self::Unavailable {
            reason: VideoDecoderActivityUnavailableReason::UnsupportedNotifier,
        }
    }

    /// Возвращает epoch, захваченный в момент snapshot-а.
    #[must_use]
    pub const fn captured_epoch(&self) -> Option<VideoDecoderActivityEpoch> {
        match self {
            Self::Available { captured_epoch, .. } => Some(*captured_epoch),
            Self::Unavailable { .. } => None,
        }
    }

    /// Возвращает receiver clone для внешнего `select!`, если notifier доступен.
    #[must_use]
    pub fn pulse_receiver(&self) -> Option<Receiver<()>> {
        match self {
            Self::Available { subscription, .. } => Some(subscription.pulse_receiver()),
            Self::Unavailable { .. } => None,
        }
    }

    /// Проверяет lost-wakeup окно после planning и перед входом в `select!`.
    #[must_use]
    pub fn activity_since(
        &self,
        observed_epoch: VideoDecoderActivityEpoch,
    ) -> VideoDecoderActivityWaitOutcome {
        match self {
            Self::Available { subscription, .. } => subscription.activity_since(observed_epoch),
            Self::Unavailable { reason } => VideoDecoderActivityWaitOutcome::Unavailable {
                reason: reason.clone(),
            },
        }
    }

    /// Классифицирует recv branch внешнего `select!` через typed neutral outcome.
    #[must_use]
    pub fn activity_after_recv(
        &self,
        observed_epoch: VideoDecoderActivityEpoch,
        recv_result: Result<(), RecvError>,
    ) -> VideoDecoderActivityWaitOutcome {
        match self {
            Self::Available { subscription, .. } => {
                subscription.activity_after_recv(observed_epoch, recv_result)
            }
            Self::Unavailable { reason } => VideoDecoderActivityWaitOutcome::Unavailable {
                reason: reason.clone(),
            },
        }
    }

    /// Ждёт activity через subscription или возвращает typed unavailable state.
    #[must_use]
    pub fn wait_for_activity_after(
        &self,
        observed_epoch: VideoDecoderActivityEpoch,
        timeout: Duration,
    ) -> VideoDecoderActivityWaitOutcome {
        match self {
            Self::Available { subscription, .. } => {
                subscription.wait_for_activity_after(observed_epoch, timeout)
            }
            Self::Unavailable { reason } => VideoDecoderActivityWaitOutcome::Unavailable {
                reason: reason.clone(),
            },
        }
    }
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

/// Production default output frame pool size для software (host-frame) decode.
///
/// Намеренно меньше hardware surface pool: каждый software-кадр — это полный
/// host-буфер в RAM (для 4K YUV420 ~12 МБ), и удержание десятков таких кадров
/// создаёт давление на пропускную способность общей памяти iGPU, из-за чего
/// растёт стоимость host→GPU upload и проседает playback FPS. 8 даёт декодеру
/// достаточный запас впереди playback, не раздувая резидентный footprint.
const DEFAULT_SOFTWARE_FRAME_POOL_FRAMES: usize = 8;

/// Production default zero-copy external import slot capacity.
const DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS: usize = 24;

/// Env-переменная для настройки flush timeout-а без перекомпиляции приложения.
const DECODER_FLUSH_TIMEOUT_ENV_VAR: &str = "VIDEOPLAYER_DECODER_FLUSH_TIMEOUT_MS";

/// Production default flush timeout decoder thread-а в миллисекундах.
const DEFAULT_DECODER_FLUSH_TIMEOUT_MS: u64 = 2_000;

/// Software decoder thread budget в нейтральной форме.
///
/// `Auto` хранит намерение "подобрать безопасное число потоков автоматически".
/// Его нужно резолвить через `resolved_thread_count()`, чтобы оставить CPU
/// headroom под render/upload/worker пути. `Fixed` используется только там, где
/// caller уже разрешил budget policy и доказал, что значение положительное и
/// меньше matching playback budget-а. Это намеренно не `usize` в config-структуре,
/// чтобы callsite явно показывал смысл числа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftwareDecodeThreadBudget {
    /// Concrete software backend выбирает число потоков сам.
    Auto,

    /// Caller передал уже разрешённый положительный лимит потоков.
    Fixed(NonZeroUsize),
}

impl SoftwareDecodeThreadBudget {
    /// Создаёт автоматический budget без backend-specific деталей на callsite-е.
    #[must_use]
    pub const fn auto() -> Self {
        Self::Auto
    }

    /// Создаёт fixed budget из уже проверенного positive value.
    #[must_use]
    pub const fn fixed(thread_count: NonZeroUsize) -> Self {
        Self::Fixed(thread_count)
    }

    /// Возвращает concrete positive limit, если caller уже разрешил budget.
    #[must_use]
    pub const fn fixed_thread_count(self) -> Option<NonZeroUsize> {
        match self {
            Self::Auto => None,
            Self::Fixed(thread_count) => Some(thread_count),
        }
    }

    /// Резолвит budget в конкретное число потоков software-декода.
    ///
    /// `Auto` = `max(2, доступный параллелизм − 2)`: два hardware thread-а
    /// остаются render/upload/worker путям, иначе полный набор decode worker-ов
    /// вытесняет render-поток и UI-кадры регулярно вылетают за бюджет
    /// (замерено на 4K60 AV1 software: 7.8% → 0.7% кадров сверх 16.7мс).
    #[must_use]
    pub fn resolved_thread_count(self) -> NonZeroUsize {
        match self {
            Self::Auto => {
                let host_parallelism = std::thread::available_parallelism()
                    .map(NonZeroUsize::get)
                    .unwrap_or(1);
                let reserved_for_render_and_worker = 2;
                let auto_threads = host_parallelism
                    .saturating_sub(reserved_for_render_and_worker)
                    .max(2)
                    .min(host_parallelism);
                NonZeroUsize::new(auto_threads)
                    .unwrap_or_else(|| NonZeroUsize::new(1).expect("1 is non-zero"))
            }
            Self::Fixed(thread_count) => thread_count,
        }
    }
}

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

    /// Output frame pool size для software (host-frame) decode backends.
    ///
    /// Применяется только software-путём (`video-ffmpeg`): задаёт и decoded-frame
    /// channel, и host resource table, т.е. сколько полных host-кадров может жить
    /// одновременно (channel + present queue + render leases). Hardware backend
    /// (`video-vaapi`) этот лимит игнорирует и использует
    /// `decoder_surface_pool_frames`, потому что VA surface — это дешёвый GPU
    /// descriptor, а не RAM-буфер. Разделение позволяет держать software-пул
    /// маленьким (меньше memory-bandwidth pressure на iGPU) независимо от
    /// hardware surface pool.
    pub software_frame_pool_frames: usize,

    /// Thread budget только для software decoder backends.
    ///
    /// Playback default остаётся `Auto`, чтобы FFmpeg мог выбирать число потоков
    /// сам (`thread_count = 0`). Независимые decode-потребители могут передать
    /// сюда `Fixed`, если их budget уже проверен на уровне владельца runtime.
    pub software_decode_thread_budget: SoftwareDecodeThreadBudget,

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
            software_frame_pool_frames: self.software_frame_pool_frames.max(1),
            software_decode_thread_budget: self.software_decode_thread_budget,
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
            software_frame_pool_frames: DEFAULT_SOFTWARE_FRAME_POOL_FRAMES,
            software_decode_thread_budget: SoftwareDecodeThreadBudget::Auto,
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

    /// Устанавливает decoder-side output floor для accurate seek preroll.
    fn set_preroll_output_floor(
        &self,
        _floor: VideoPrerollOutputFloor,
    ) -> VideoPrerollOutputFloorResult {
        VideoPrerollOutputFloorResult::Unsupported
    }

    /// Очищает decoder-side output floor без изменения seek generation.
    fn clear_preroll_output_floor(
        &self,
        _clear: VideoPrerollOutputFloorClear,
    ) -> VideoPrerollOutputFloorResult {
        VideoPrerollOutputFloorResult::Unchanged
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

    /// Возвращает typed snapshot software host-upload ресурсов.
    fn host_upload_resource_snapshot(&self) -> HostUploadResourceSnapshotStatus {
        HostUploadResourceSnapshotStatus::UnsupportedBackend
    }

    /// Возвращает snapshot bounded control channel-а для diagnostics.
    fn decoder_control_channel_pressure(
        &self,
    ) -> Option<VideoDecoderControlChannelPressureSnapshot> {
        None
    }

    /// Возвращает snapshot нейтрального activity notifier-а для event-driven waits.
    fn decoder_activity_snapshot(&self) -> VideoDecoderActivitySnapshot {
        VideoDecoderActivitySnapshot::unsupported()
    }

    /// Возвращает глубину packet channel-а внутри decoder thread.
    fn packet_queue_depth(&self) -> usize;

    /// Забирает количество packets, обработанных decoder thread-ом.
    fn drain_completed_packet_count(&self) -> usize;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct TestResourceProvider;

    struct DefaultPrerollFloorDecoderThread;

    impl VideoDecoderThreadHandle for DefaultPrerollFloorDecoderThread {
        type ResourceProvider = TestResourceProvider;

        fn backend_name(&self) -> &'static str {
            "default-preroll-floor-test"
        }

        fn send_packet(&self, _packet: DecodePacket) -> Result<(), DecodeSendError> {
            Ok(())
        }

        fn release_frame(&self, _handle: crate::FrameResourceHandle) {}

        fn try_recv_frame(&self) -> Option<crate::DecodedFrame> {
            None
        }

        fn try_recv_diagnostic_event(&self) -> Option<crate::VideoDecoderDiagnosticEvent> {
            None
        }

        fn try_recv_error(&self) -> Option<DecodeThreadError> {
            None
        }

        fn flush(&self) -> anyhow::Result<()> {
            Ok(())
        }

        fn resource_provider(&self) -> Self::ResourceProvider {
            TestResourceProvider
        }

        fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
            None
        }

        fn packet_queue_depth(&self) -> usize {
            0
        }

        fn drain_completed_packet_count(&self) -> usize {
            0
        }
    }

    /// Проверяет default set behavior: unsupported backend не маскируется под no-op.
    #[test]
    fn default_preroll_output_floor_set_returns_unsupported() {
        let decoder_thread = DefaultPrerollFloorDecoderThread;
        let floor = VideoPrerollOutputFloor {
            generation: 7,
            floor_pts: Duration::from_millis(1_500),
            retain_latest_before_floor: true,
        };

        assert_eq!(
            decoder_thread.set_preroll_output_floor(floor),
            VideoPrerollOutputFloorResult::Unsupported
        );
    }

    /// Проверяет default clear behavior: отсутствие support остаётся harmless no-op.
    #[test]
    fn default_preroll_output_floor_clear_returns_unchanged() {
        let decoder_thread = DefaultPrerollFloorDecoderThread;

        assert_eq!(
            decoder_thread
                .clear_preroll_output_floor(VideoPrerollOutputFloorClear::MatchingGeneration(7)),
            VideoPrerollOutputFloorResult::Unchanged
        );
    }

    /// Проверяет default activity contract: старый backend получает typed unsupported fallback.
    #[test]
    fn default_decoder_activity_snapshot_returns_unsupported() {
        let decoder_thread = DefaultPrerollFloorDecoderThread;

        match decoder_thread.decoder_activity_snapshot() {
            VideoDecoderActivitySnapshot::Unavailable { reason } => {
                assert_eq!(
                    reason,
                    VideoDecoderActivityUnavailableReason::UnsupportedNotifier
                );
            }
            VideoDecoderActivitySnapshot::Available { .. } => {
                panic!("default decoder handle must not expose activity subscription");
            }
        }
    }

    /// Проверяет default software-resource contract: hardware/old backend не маскируется под нули.
    #[test]
    fn default_host_upload_resource_snapshot_returns_unsupported() {
        let decoder_thread = DefaultPrerollFloorDecoderThread;

        assert_eq!(
            decoder_thread.host_upload_resource_snapshot(),
            HostUploadResourceSnapshotStatus::UnsupportedBackend
        );
    }

    /// Проверяет happy path: caller видит activity после ранее observed epoch-а.
    #[test]
    fn decoder_activity_wait_receives_activity_after_observed_epoch() {
        let (notifier, subscription) = VideoDecoderActivityNotifier::new();
        let observed_epoch = subscription.current_epoch();

        let notified_epoch = notifier.notify_activity();
        let outcome =
            subscription.wait_for_activity_after(observed_epoch, Duration::from_millis(1));

        assert_eq!(
            outcome,
            VideoDecoderActivityWaitOutcome::ActivityReceived {
                epoch: notified_epoch
            }
        );
    }

    /// Проверяет fallback timeout: без нового epoch-а outcome не маскируется под activity.
    #[test]
    fn decoder_activity_wait_times_out_without_new_epoch() {
        let (_notifier, subscription) = VideoDecoderActivityNotifier::new();
        let observed_epoch = subscription.current_epoch();

        let outcome =
            subscription.wait_for_activity_after(observed_epoch, Duration::from_millis(1));

        assert_eq!(
            outcome,
            VideoDecoderActivityWaitOutcome::Timeout {
                observed_epoch,
                current_epoch: observed_epoch
            }
        );
    }

    /// Проверяет stale pulse: wakeup есть, но activity уже была учтена caller-ом.
    #[test]
    fn decoder_activity_wait_reports_no_new_activity_after_epoch() {
        let (notifier, subscription) = VideoDecoderActivityNotifier::new();
        let observed_epoch = notifier.notify_activity();

        let outcome =
            subscription.wait_for_activity_after(observed_epoch, Duration::from_millis(1));

        assert_eq!(
            outcome,
            VideoDecoderActivityWaitOutcome::NoNewActivityAfterEpoch {
                observed_epoch,
                current_epoch: observed_epoch
            }
        );
    }

    /// Проверяет disconnect как typed state, чтобы wait side не входил в tight ready-loop.
    #[test]
    fn decoder_activity_wait_reports_disconnected_notifier() {
        let (notifier, subscription) = VideoDecoderActivityNotifier::new();
        let observed_epoch = subscription.current_epoch();
        drop(notifier);

        let outcome =
            subscription.wait_for_activity_after(observed_epoch, Duration::from_millis(1));

        assert_eq!(
            outcome,
            VideoDecoderActivityWaitOutcome::Unavailable {
                reason: VideoDecoderActivityUnavailableReason::DisconnectedNotifier
            }
        );
    }

    /// Проверяет coalescing: bounded pulse не копит очередь, но epoch сохраняет все активности.
    #[test]
    fn decoder_activity_coalescing_keeps_latest_epoch() {
        let (notifier, subscription) = VideoDecoderActivityNotifier::new();
        let observed_epoch = subscription.current_epoch();

        let first_epoch = notifier.notify_activity();
        let second_epoch = notifier.notify_activity();
        let third_epoch = notifier.notify_activity();

        assert_eq!(first_epoch.get(), observed_epoch.get() + 1);
        assert_eq!(second_epoch.get(), observed_epoch.get() + 2);
        assert_eq!(third_epoch.get(), observed_epoch.get() + 3);
        assert_eq!(
            subscription.snapshot().activity_since(observed_epoch),
            VideoDecoderActivityWaitOutcome::ActivityReceived { epoch: third_epoch }
        );
        assert_eq!(
            subscription.wait_for_activity_after(third_epoch, Duration::from_millis(1)),
            VideoDecoderActivityWaitOutcome::NoNewActivityAfterEpoch {
                observed_epoch: third_epoch,
                current_epoch: third_epoch
            }
        );
        assert_eq!(
            subscription.wait_for_activity_after(third_epoch, Duration::from_millis(1)),
            VideoDecoderActivityWaitOutcome::Timeout {
                observed_epoch: third_epoch,
                current_epoch: third_epoch
            }
        );
    }

    /// Фиксирует, что result enum сохраняет typed backpressure и fatal payloads.
    #[test]
    fn preroll_output_floor_result_preserves_backpressure_and_fatal_payloads() {
        let backpressure_reason = VideoDecoderControlBackpressureReason::ControlChannelFull {
            queued_messages: 3,
            capacity: 4,
        };
        let fatal_error = DecodeThreadError::new("floor command failed");

        assert_eq!(
            VideoPrerollOutputFloorResult::Backpressure(backpressure_reason),
            VideoPrerollOutputFloorResult::Backpressure(
                VideoDecoderControlBackpressureReason::ControlChannelFull {
                    queued_messages: 3,
                    capacity: 4,
                }
            )
        );
        assert_eq!(
            VideoPrerollOutputFloorResult::Fatal(fatal_error.clone()),
            VideoPrerollOutputFloorResult::Fatal(DecodeThreadError::new(fatal_error.message()))
        );
    }

    /// Проверяет, что direct API caller не может создать zero-capacity очереди.
    #[test]
    fn decoder_thread_config_normalizes_zero_limits() {
        let config = VideoDecoderThreadConfig {
            packet_channel_frames: 0,
            frame_channel_frames: 0,
            control_channel_frames: 0,
            decoder_ready_queue_frames: 0,
            decoder_surface_pool_frames: 0,
            software_frame_pool_frames: 0,
            software_decode_thread_budget: SoftwareDecodeThreadBudget::auto(),
            zero_copy_surface_pool_slots: 0,
            flush_timeout: Duration::ZERO,
        }
        .normalized();

        assert_eq!(config.packet_channel_frames, 1);
        assert_eq!(config.frame_channel_frames, 1);
        assert_eq!(config.software_frame_pool_frames, 1);
        assert_eq!(
            config.software_decode_thread_budget,
            SoftwareDecodeThreadBudget::auto()
        );
        assert_eq!(config.control_channel_frames, 1);
        assert_eq!(config.decoder_ready_queue_frames, 1);
        assert_eq!(config.decoder_surface_pool_frames, 1);
        assert_eq!(config.zero_copy_surface_pool_slots, 1);
        assert_eq!(config.flush_timeout, Duration::from_millis(1));
    }

    /// Fixed software thread budget хранит только positive value на уровне типа.
    #[test]
    fn software_decode_thread_budget_preserves_fixed_positive_value() {
        let thread_count = NonZeroUsize::new(3).expect("test value is positive");
        let budget = SoftwareDecodeThreadBudget::fixed(thread_count);

        assert_eq!(budget.fixed_thread_count(), Some(thread_count));
        assert_eq!(
            SoftwareDecodeThreadBudget::auto().fixed_thread_count(),
            None
        );
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

    /// Проверяет, что software snapshot не использует VA-API surface slots.
    #[test]
    fn host_upload_resource_snapshot_stores_software_counters() {
        let snapshot = HostUploadResourceSnapshot {
            host_frames_ready: 2,
            host_frames_in_flight: 3,
            upload_slots_capacity: 4,
            upload_slots_free: 1,
            upload_failures: 5,
        };

        assert_eq!(snapshot.host_frames_ready, 2);
        assert_eq!(snapshot.host_frames_in_flight, 3);
        assert_eq!(snapshot.upload_slots_capacity, 4);
        assert_eq!(snapshot.upload_slots_free, 1);
        assert_eq!(snapshot.upload_failures, 5);
    }

    /// Проверяет, что ready-queue pressure и upload-slot pressure остаются разными типами.
    #[test]
    fn host_upload_backpressure_distinguishes_ready_queue_and_upload_slots() {
        let ready_queue_full = HostUploadResourceSnapshot {
            host_frames_ready: 4,
            host_frames_in_flight: 1,
            upload_slots_capacity: 2,
            upload_slots_free: 1,
            upload_failures: 0,
        };
        let upload_slots_exhausted = HostUploadResourceSnapshot {
            host_frames_ready: 1,
            host_frames_in_flight: 2,
            upload_slots_capacity: 2,
            upload_slots_free: 0,
            upload_failures: 0,
        };

        assert_eq!(
            ready_queue_full.backpressure_reason(4),
            Some(HostUploadBackpressureReason::ReadyQueueFull {
                host_frames_ready: 4,
                capacity: 4,
            })
        );
        assert_eq!(
            upload_slots_exhausted.backpressure_reason(4),
            Some(HostUploadBackpressureReason::UploadSlotsExhausted {
                host_frames_in_flight: 2,
                upload_slots_capacity: 2,
            })
        );
    }
}
