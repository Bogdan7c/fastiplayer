use std::{fmt, num::NonZeroU64};

/// Номер исходной строки с единицы, используемый только в безопасной диагностике.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HlsLineNumber(NonZeroU64);

impl HlsLineNumber {
    pub(crate) fn from_index(index: usize) -> Self {
        let one_based = u64::try_from(index.saturating_add(1)).unwrap_or(u64::MAX);
        Self(NonZeroU64::new(one_based).expect("saturating_add keeps line non-zero"))
    }

    /// Возвращает номер строки с единицы.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Exact-сериализация URI-reference; `Debug` не раскрывает signed/query material.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ExactReference(Box<str>);

impl ExactReference {
    pub(crate) fn new(reference: &str) -> Self {
        Self(reference.into())
    }

    /// Открывает exact reference только будущей resolution/request-границе.
    pub fn expose_for_resolution(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ExactReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExactReference")
            .field("utf8_bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// RFC decimal-duration сохраняется точно, без преждевременной политики округления.
#[derive(Clone, PartialEq, Eq)]
pub struct HlsDuration(Box<str>);

impl HlsDuration {
    pub(crate) fn new(raw: &str) -> Self {
        Self(raw.into())
    }

    /// Exact validated decimal-сериализация.
    pub fn as_decimal_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for HlsDuration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("HlsDuration").field(&self.0).finish()
    }
}

/// Точная рациональная максимальная частота из `EXT-X-STREAM-INF:FRAME-RATE`.
///
/// Playlist использует decimal-синтаксис, поэтому parser хранит сокращённую
/// дробь и не добавляет binary floating-point rounding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HlsFrameRate {
    numerator: u64,
    denominator: NonZeroU64,
}

impl HlsFrameRate {
    pub(crate) fn new(numerator: u64, denominator: NonZeroU64) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    /// Возвращает сокращённый numerator.
    pub const fn numerator(self) -> u64 {
        self.numerator
    }

    /// Возвращает сокращённый положительный denominator.
    pub const fn denominator(self) -> u64 {
        self.denominator.get()
    }
}

/// Стандартизованное evidence `EXT-X-STREAM-INF:VIDEO-RANGE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HlsVideoRange {
    /// Standard dynamic range.
    Sdr,
    /// Hybrid Log-Gamma HDR.
    Hlg,
    /// Perceptual Quantizer HDR.
    Pq,
}

/// Декларация byte range; пропущенный offset остаётся явным для resolution в S32B.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    /// Обязательная положительная длина в bytes.
    pub length: u64,
    /// Explicit absolute offset либо RFC implicit continuation.
    pub offset: Option<u64>,
}

/// Поддерживаемый/неподдерживаемый key method сохраняется до profile validation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HlsKeyMethod {
    None,
    Aes128,
    SampleAes,
    Other(Box<str>),
}

/// Отсутствующий `KEYFORMAT` семантически равен `identity`, но остаётся различимым.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HlsKeyFormat {
    ImplicitIdentity,
    Identity,
    Other(Box<str>),
}

/// Разобранная декларация `EXT-X-KEY`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HlsKeyDeclaration {
    pub method: HlsKeyMethod,
    pub key_format: HlsKeyFormat,
    pub key_format_versions: Option<Box<str>>,
    pub uri: Option<ExactReference>,
    pub explicit_iv: Option<[u8; 16]>,
    /// Playlist-local identity конкретной `EXT-X-KEY` declaration.
    pub(crate) declaration_sequence: u64,
}

impl HlsKeyDeclaration {
    /// Возвращает identity declaration, сохраняемый всеми segment/MAP snapshot-ами.
    #[must_use]
    pub const fn declaration_sequence(&self) -> u64 {
        self.declaration_sequence
    }
}

/// Активная декларация `EXT-X-MAP` на границе сегмента.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitializationMap {
    pub uri: ExactReference,
    pub byte_range: Option<ByteRange>,
    pub key: Option<HlsKeyDeclaration>,
}

/// Владеющий descriptor media segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaSegment {
    pub uri: ExactReference,
    pub duration: HlsDuration,
    pub title: Box<str>,
    pub byte_range: Option<ByteRange>,
    pub discontinuity: bool,
    pub media_sequence: u64,
    /// RFC discontinuity sequence этого сегмента.
    ///
    /// В отличие от media sequence это значение можно сопоставлять между
    /// выбранными renditions вместе с relative timeline.
    pub discontinuity_sequence: u64,
    pub initialization_map: Option<InitializationMap>,
    pub key: Option<HlsKeyDeclaration>,
}

/// Тип master rendition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaRenditionType {
    Audio,
    Video,
    Subtitles,
    ClosedCaptions,
}

/// Владеющий alternate-media/subtitle descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaRendition {
    pub rendition_type: MediaRenditionType,
    pub group_id: Box<str>,
    pub name: Box<str>,
    pub uri: Option<ExactReference>,
    pub language: Option<Box<str>>,
    pub associated_language: Option<Box<str>>,
    pub characteristics: Option<Box<str>>,
    /// Первый положительный RFC `CHANNELS` parameter.
    pub channel_count: Option<NonZeroU64>,
    pub channels: Option<Box<str>>,
    pub is_default: bool,
    pub autoselect: bool,
    pub forced: bool,
}

/// Владеющий descriptor variant stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariantStream {
    pub uri: ExactReference,
    pub bandwidth: u64,
    pub average_bandwidth: Option<u64>,
    pub codecs: Option<Box<str>>,
    pub resolution: Option<(u32, u32)>,
    pub frame_rate: Option<HlsFrameRate>,
    pub video_range: Option<HlsVideoRange>,
    pub audio_group: Option<Box<str>>,
    pub video_group: Option<Box<str>>,
    pub subtitle_group: Option<Box<str>>,
    pub closed_captions: Option<ClosedCaptionsReference>,
    pub requires_output_protection: bool,
}

/// Ссылка variant stream на closed-caption group либо явное отсутствие captions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClosedCaptionsReference {
    None,
    Group(Box<str>),
}

/// Владеющая модель master playlist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MasterPlaylist {
    pub variants: Box<[VariantStream]>,
    pub renditions: Box<[MediaRendition]>,
    pub has_session_key: bool,
    pub session_keys: Box<[HlsKeyDeclaration]>,
    pub has_low_latency_semantics: bool,
    pub(crate) protocol_version: Option<u64>,
    pub(crate) has_i_frame_variant: bool,
    pub(crate) has_start_offset: bool,
    pub(crate) has_variable_substitution: bool,
    pub(crate) has_content_steering: bool,
}

/// Declared mutability mode media playlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsPlaylistType {
    Event,
    Vod,
}

/// Владеющая модель media playlist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaPlaylist {
    pub target_duration_seconds: u64,
    pub media_sequence: u64,
    /// Значение `EXT-X-DISCONTINUITY-SEQUENCE`, либо RFC default `0`.
    pub discontinuity_sequence: u64,
    pub segments: Box<[MediaSegment]>,
    pub key_declarations: Box<[HlsKeyDeclaration]>,
    pub end_list: bool,
    pub has_low_latency_semantics: bool,
    pub i_frames_only: bool,
    pub playlist_type: Option<HlsPlaylistType>,
    pub(crate) protocol_version: Option<u64>,
    pub(crate) has_start_offset: bool,
    pub(crate) has_variable_substitution: bool,
    pub(crate) has_content_steering: bool,
}

/// Структурно допустимая HLS topology.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HlsPlaylist {
    Master(MasterPlaylist),
    Media(MediaPlaylist),
}

/// Заданный caller-ом container intent для проверок, невозможных только по URI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaContainerIntent {
    TransportStream,
    FragmentedMp4,
}
