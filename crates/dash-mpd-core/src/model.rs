use crate::template::DashTemplateString;

/// Единственный namespace поддерживаемого MPEG-DASH MPD schema.
pub const DASH_MPD_NAMESPACE: &str = "urn:mpeg:dash:schema:mpd:2011";

/// Проверенная относительная либо абсолютная ссылка без URL parsing authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashUrlReference(String);

impl DashUrlReference {
    /// Parser создаёт ссылку только из непустого bounded XML text.
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    /// Runtime раскрывает строку только для typed URL resolution.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Один BaseURL конкретного уровня inheritance chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashBaseUrl {
    /// Lexical reference разрешается runtime-ом относительно effective MPD URL.
    reference: DashUrlReference,
    /// Optional signed availability offset в nanoseconds.
    pub availability_time_offset_nanoseconds: Option<i128>,
    /// Optional completeness flag; отсутствие означает default `true`.
    pub availability_time_complete: Option<bool>,
}

impl DashBaseUrl {
    /// Создаётся parser-ом вместе с dynamic availability attributes.
    pub(crate) fn with_availability(
        reference: DashUrlReference,
        availability_time_offset_nanoseconds: Option<i128>,
        availability_time_complete: Option<bool>,
    ) -> Self {
        Self {
            reference,
            availability_time_offset_nanoseconds,
            availability_time_complete,
        }
    }

    /// Возвращает следующую ссылку цепочки BaseURL inheritance.
    pub fn reference(&self) -> &DashUrlReference {
        &self.reference
    }
}

/// Доказанный container family для существующего S28 demux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DashContainer {
    /// Fragmented ISO BMFF/CMAF.
    IsoBmff,
    /// Finite WebM/Matroska segments.
    WebM,
}

/// Доказанный component layout одной Representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DashMediaKind {
    /// Только video component.
    Video,
    /// Только audio component.
    Audio,
    /// Muxed audio + video.
    Muxed,
}

/// Exact positive `FrameRateType` value из MPD без floating-point округления.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DashFrameRate {
    /// Числитель frames/time.
    pub numerator: u32,
    /// Знаменатель; для целого lexical value равен единице.
    pub denominator: u32,
}

/// Стандартизованная схема `AudioChannelConfiguration`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DashAudioChannelConfiguration {
    /// ISO/IEC 23091-3 ChannelConfiguration code point.
    MpegCicp(u16),
    /// ISO/IEC 23003-3 channelConfigurationIndex.
    Mpeg23003_3(u16),
    /// Descriptor использует неизвестную текущему profile схему.
    Unsupported,
}

/// Exact standardized CICP color evidence; каждое отсутствующее поле остаётся `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DashColorMetadata {
    /// ISO/IEC 23091-2 colour_primaries code point.
    pub colour_primaries: Option<u8>,
    /// ISO/IEC 23091-2 transfer_characteristics code point.
    pub transfer_characteristics: Option<u8>,
    /// ISO/IEC 23091-2 matrix_coefficients code point.
    pub matrix_coefficients: Option<u8>,
    /// Exact VideoFullRangeFlag.
    pub video_full_range: Option<bool>,
}

/// Стандартизованная HDR transfer function, доказанная CICP metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DashHdrTransfer {
    /// SMPTE ST 2084 perceptual quantizer, CICP transfer characteristic 16.
    Pq,
    /// ARIB STD-B67 hybrid log-gamma, CICP transfer characteristic 18.
    Hlg,
}

impl DashColorMetadata {
    /// Возвращает HDR только для двух стандартизованных HDR transfer code points.
    pub const fn hdr_transfer(self) -> Option<DashHdrTransfer> {
        match self.transfer_characteristics {
            Some(16) => Some(DashHdrTransfer::Pq),
            Some(18) => Some(DashHdrTransfer::Hlg),
            _ => None,
        }
    }
}

/// Inclusive byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexRange {
    /// Первый byte.
    start: u64,
    /// Последний byte включительно.
    end: u64,
}

impl IndexRange {
    /// Проверяет непустой inclusive range.
    pub(crate) fn new(start: u64, end: u64) -> Option<Self> {
        (start <= end).then_some(Self { start, end })
    }

    /// Первый byte.
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Последний byte включительно.
    pub const fn end(self) -> u64 {
        self.end
    }
}

/// Initialization resource descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashInitialization {
    /// Optional separate init URL.
    pub source_url: Option<DashUrlReference>,
    /// Optional Range внутри effective resource.
    pub byte_range: Option<IndexRange>,
}

/// Одна SegmentList media resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashSegmentListEntry {
    /// Media reference обязательна.
    pub media: DashUrlReference,
    /// Optional exact media range.
    pub media_range: Option<IndexRange>,
    /// Optional external index reference.
    pub index: Option<DashUrlReference>,
    /// Optional exact index range.
    pub index_range: Option<IndexRange>,
}

/// Explicit finite SegmentList.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashSegmentList {
    /// Segment timescale.
    pub timescale: u64,
    /// Uniform duration, если timeline отсутствует.
    pub duration: Option<u64>,
    /// Optional initialization descriptor.
    pub initialization: Option<DashInitialization>,
    /// Ordered media resources.
    pub segments: Box<[DashSegmentListEntry]>,
}

/// Range-backed SegmentBase descriptor для будущего S34B.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashSegmentBase {
    /// Segment index range в effective Representation resource.
    pub index_range: Option<IndexRange>,
    /// Optional initialization range/resource.
    pub initialization: Option<DashInitialization>,
    /// Presentation time offset в timescale units.
    pub presentation_time_offset: u64,
    /// Timescale для offset/index interpretation.
    pub timescale: u64,
}

/// Один `S` элемент SegmentTimeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DashTimelineEntry {
    /// Explicit start либо continuation предыдущего entry.
    pub start_time: Option<u64>,
    /// Positive duration.
    pub duration: u64,
    /// Non-negative repeat либо `-1` до следующего start/Period end.
    pub repeat: i64,
}

/// SegmentTemplate descriptor с validated placeholder syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashSegmentTemplate {
    /// Timescale, всегда non-zero.
    pub timescale: u64,
    /// First `$Number$`.
    pub start_number: u64,
    /// Presentation time offset.
    pub presentation_time_offset: u64,
    /// Media template.
    pub media: DashTemplateString,
    /// Optional init template.
    pub initialization: Option<DashTemplateString>,
    /// Uniform duration alternative timeline-у.
    pub duration: Option<u64>,
    /// Explicit timeline alternative duration-у.
    pub timeline: Box<[DashTimelineEntry]>,
    /// Optional signed availability offset в nanoseconds.
    pub availability_time_offset_nanoseconds: Option<i128>,
    /// Optional completeness flag; отсутствие означает default `true`.
    pub availability_time_complete: Option<bool>,
}

/// Ровно один addressing mode после inheritance resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DashAddressing {
    /// URL template.
    Template(DashSegmentTemplate),
    /// Explicit URL list.
    List(DashSegmentList),
    /// Single Range-backed Representation resource.
    Base(DashSegmentBase),
    /// Один обычный effective Representation resource.
    SingleResource,
}

/// Проверенная Representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashRepresentation {
    /// Stable Representation identifier для template expansion.
    pub id: String,
    /// Optional bandwidth для `$Bandwidth$`.
    pub bandwidth: Option<u64>,
    /// Optional exact coded width.
    pub width: Option<u32>,
    /// Optional exact coded height.
    pub height: Option<u32>,
    /// Optional exact inherited frame rate.
    pub frame_rate: Option<DashFrameRate>,
    /// Optional exact inherited audio sampling rate.
    pub audio_sampling_rate: Option<u32>,
    /// Optional inherited standardized channel configuration.
    pub audio_channel_configuration: Option<DashAudioChannelConfiguration>,
    /// Optional inherited BCP 47 language metadata.
    pub language: Option<String>,
    /// Exact inherited standardized color metadata.
    pub color: DashColorMetadata,
    /// Доказанный container.
    pub container: DashContainer,
    /// Доказанный component kind.
    pub media_kind: DashMediaKind,
    /// Exact bounded codecs attribute.
    pub codecs: String,
    /// BaseURL текущего уровня.
    pub base_url: Option<DashBaseUrl>,
    /// Effective addressing после AdaptationSet inheritance.
    pub addressing: DashAddressing,
}

/// AdaptationSet с однородным media/container назначением.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashAdaptationSet {
    /// Optional schema identifier.
    pub id: Option<String>,
    /// BaseURL текущего уровня.
    pub base_url: Option<DashBaseUrl>,
    /// Ordered Representation rows.
    pub representations: Box<[DashRepresentation]>,
}

/// Явно различает конечную presentation-длительность и открытый live tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashPresentationDuration {
    /// Конечная длительность в миллисекундах.
    FiniteMilliseconds(u64),
    /// Последний dynamic Period продолжается за пределами текущего MPD snapshot-а.
    OpenEnded,
}

impl DashPresentationDuration {
    /// Возвращает конечную длительность только для bounded presentation-а.
    #[must_use]
    pub const fn finite_milliseconds(self) -> Option<u64> {
        match self {
            Self::FiniteMilliseconds(milliseconds) => Some(milliseconds),
            Self::OpenEnded => None,
        }
    }

    /// Проверяет, что live tail не имеет объявленной конечной границы.
    #[must_use]
    pub const fn is_open_ended(self) -> bool {
        matches!(self, Self::OpenEnded)
    }
}

/// Один static либо dynamic Period с явной lifecycle-семантикой конца.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashPeriod {
    /// Optional schema identifier.
    pub id: Option<String>,
    /// Absolute start относительно presentation.
    pub start_milliseconds: u64,
    /// Конечная длительность либо открытый dynamic tail.
    pub duration: DashPresentationDuration,
    /// BaseURL текущего уровня.
    pub base_url: Option<DashBaseUrl>,
    /// Ordered adaptation sets.
    pub adaptation_sets: Box<[DashAdaptationSet]>,
}

/// Полностью проверенный static либо dynamic MPD presentation graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashMpd {
    /// Конечная длительность либо открытый dynamic presentation tail.
    pub media_presentation_duration: DashPresentationDuration,
    /// Root BaseURL.
    pub base_url: Option<DashBaseUrl>,
    /// Ordered contiguous periods.
    pub periods: Box<[DashPeriod]>,
}
