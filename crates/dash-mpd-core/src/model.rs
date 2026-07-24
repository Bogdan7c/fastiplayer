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
}

impl DashBaseUrl {
    /// Создаётся schema parser-ом после cardinality/text checks.
    pub(crate) fn new(reference: DashUrlReference) -> Self {
        Self { reference }
    }

    /// Возвращает следующую ссылку цепочки BaseURL inheritance.
    pub fn reference(&self) -> &DashUrlReference {
        &self.reference
    }
}

/// Доказанный container family для существующего S28 demux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashContainer {
    /// Fragmented ISO BMFF/CMAF.
    IsoBmff,
    /// Finite WebM/Matroska segments.
    WebM,
}

/// Доказанный component layout одной Representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashMediaKind {
    /// Только video component.
    Video,
    /// Только audio component.
    Audio,
    /// Muxed audio + video.
    Muxed,
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

/// Один конечный static Period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashPeriod {
    /// Optional schema identifier.
    pub id: Option<String>,
    /// Absolute start относительно presentation.
    pub start_milliseconds: u64,
    /// Finite duration.
    pub duration_milliseconds: u64,
    /// BaseURL текущего уровня.
    pub base_url: Option<DashBaseUrl>,
    /// Ordered adaptation sets.
    pub adaptation_sets: Box<[DashAdaptationSet]>,
}

/// Полностью проверенный static MPD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashMpd {
    /// Finite presentation duration.
    pub media_presentation_duration_milliseconds: u64,
    /// Root BaseURL.
    pub base_url: Option<DashBaseUrl>,
    /// Ordered contiguous periods.
    pub periods: Box<[DashPeriod]>,
}
