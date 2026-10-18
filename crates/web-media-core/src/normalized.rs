use std::fmt;

/// S00 bound для неизвестных protocol/ext/container/codec identities.
pub const MAX_RAW_IDENTITY_UTF8_BYTES: usize = 256;

/// Поле исходного extractor descriptor-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawIdentityField {
    /// `protocol`.
    Transport,
    /// `ext`.
    Extension,
    /// `container`.
    Container,
    /// `vcodec` либо `acodec`.
    Codec,
}

/// Ошибка bounded raw identity без echo исходной строки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawIdentityBuildError {
    /// Raw identity отсутствует там, где caller заявил её наличие.
    Empty {
        /// Безопасное имя поля.
        field: RawIdentityField,
    },
    /// UTF-8 строка превышает S00 contract.
    TooLong {
        /// Безопасное имя поля.
        field: RawIdentityField,
        /// Фактическая длина в UTF-8 bytes.
        provided_bytes: usize,
        /// S00 maximum.
        maximum_bytes: usize,
    },
}

impl fmt::Display for RawIdentityBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { field } => write!(formatter, "{field:?} identity can't be empty"),
            Self::TooLong {
                field,
                provided_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "{field:?} identity occupies {provided_bytes} bytes at limit {maximum_bytes}"
            ),
        }
    }
}

impl std::error::Error for RawIdentityBuildError {}

/// Генерирует field-specific raw newtype с exact preservation и safe diagnostics.
macro_rules! raw_identity {
    (
        $(#[$metadata:meta])*
        $name:ident,
        field = $field:expr
    ) => {
        $(#[$metadata])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Проверяет S00 UTF-8 byte bound без normalization.
            pub fn new(exact_value: impl Into<String>) -> Result<Self, RawIdentityBuildError> {
                let exact_value = exact_value.into();
                validate_raw_identity(&exact_value, $field)?;
                Ok(Self(exact_value))
            }

            /// Возвращает exact raw identity для mapping/diagnostics owner-а.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($name))
                    .field("utf8_bytes", &self.0.len())
                    .finish_non_exhaustive()
            }
        }
    };
}

raw_identity!(
    /// Exact bounded `protocol`.
    RawTransportIdentity,
    field = RawIdentityField::Transport
);
raw_identity!(
    /// Exact bounded `ext`.
    RawExtensionIdentity,
    field = RawIdentityField::Extension
);
raw_identity!(
    /// Exact bounded `container`.
    RawContainerIdentity,
    field = RawIdentityField::Container
);
raw_identity!(
    /// Exact bounded `vcodec` или `acodec`.
    RawCodecIdentity,
    field = RawIdentityField::Codec
);

/// Progressive HTTP scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HttpScheme {
    /// Незашифрованный HTTP.
    Http,
    /// TLS-protected HTTPS.
    Https,
}

/// Progressive FTP scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FtpScheme {
    /// Незашифрованный FTP.
    Ftp,
    /// TLS-protected FTPS.
    Ftps,
}

/// RTMP alias из утверждённого S00 inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RtmpVariant {
    /// Обычный RTMP.
    Rtmp,
    /// Encrypted RTMPE identity.
    Rtmpe,
    /// Upstream identity `rtmp_ffmpeg`; это имя не разрешает FFmpeg transport.
    RtmpFfmpegIdentity,
}

/// Известный protocol, который S00 явно исключил.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KnownExcludedTransport {
    /// Generator требует несериализуемое live state.
    DashGenerator,
    /// Extractor-private live transport.
    PrivateLiveState,
    /// Не основной audio/video media.
    NonMedia,
}

/// Нормализованная transport family при сохранённом raw identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TransportFamily {
    /// Progressive HTTP(S).
    ProgressiveHttp(HttpScheme),
    /// Progressive FTP(S).
    ProgressiveFtp(FtpScheme),
    /// HLS aliases.
    Hls,
    /// Serializable DASH aliases.
    Dash,
    /// ISO Smooth Streaming.
    SmoothStreaming,
    /// Adobe HDS/F4M.
    Hds,
    /// RTMP family.
    Rtmp(RtmpVariant),
    /// Известный S00 exclusion.
    KnownExcluded(KnownExcludedTransport),
    /// Будущая либо неизвестная identity.
    Unknown,
}

impl TransportFamily {
    /// Могут ли video- и audio-component с этими family открыться одним runtime-маршрутом.
    ///
    /// Separate-кандидат (отдельные video-only + audio-only ресурсы) runtime собирает
    /// только внутри одного маршрута:
    /// - progressive HTTP(S)/FTP(S) — каждый component открывается своим byte-transport-ом
    ///   и склеивается packet-level composite demuxer-ом, поэтому HTTP и FTP совместимы;
    /// - adaptive/streaming family (HLS, DASH, Smooth, HDS, RTMP) — оба component обязаны
    ///   принадлежать одной presentation той же family.
    ///
    /// Смешанная пара (например HLS video + progressive HTTPS audio, реальный YouTube
    /// `616+251`) не имеет маршрута: progressive composer не умеет HLS, а HLS-opener не
    /// умеет внешний progressive audio. Planner обязан отклонять такую пару заранее,
    /// а не отдавать её runtime-у как playable.
    #[must_use]
    pub fn shares_component_composition_route(self, other: Self) -> bool {
        match (self, other) {
            // Progressive HTTP/FTP открываются по отдельности и композируются одинаково.
            (
                Self::ProgressiveHttp(_) | Self::ProgressiveFtp(_),
                Self::ProgressiveHttp(_) | Self::ProgressiveFtp(_),
            ) => true,
            (Self::Hls, Self::Hls)
            | (Self::Dash, Self::Dash)
            | (Self::SmoothStreaming, Self::SmoothStreaming)
            | (Self::Hds, Self::Hds) => true,
            // RTMP-варианты различаются handshake-ом, поэтому совпадать должен и variant.
            (Self::Rtmp(left), Self::Rtmp(right)) => left == right,
            // Excluded/Unknown и любые смешанные пары маршрута не имеют.
            _ => false,
        }
    }
}

/// Raw+parsed transport value.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NormalizedTransport {
    /// Exact raw identity.
    raw: RawTransportIdentity,
    /// Parsed family.
    family: TransportFamily,
}

impl NormalizedTransport {
    /// Нормализует только известные S00 aliases и никогда не меняет raw строку.
    pub fn parse(raw: RawTransportIdentity) -> Self {
        let family = match raw.as_str().to_ascii_lowercase().as_str() {
            "http" => TransportFamily::ProgressiveHttp(HttpScheme::Http),
            "https" => TransportFamily::ProgressiveHttp(HttpScheme::Https),
            "ftp" => TransportFamily::ProgressiveFtp(FtpScheme::Ftp),
            "ftps" => TransportFamily::ProgressiveFtp(FtpScheme::Ftps),
            "m3u8" | "m3u8_native" | "m3u8_frag_urls" => TransportFamily::Hls,
            "http_dash_segments" | "dash_frag_urls" => TransportFamily::Dash,
            "http_dash_segments_generator" => {
                TransportFamily::KnownExcluded(KnownExcludedTransport::DashGenerator)
            }
            "ism" => TransportFamily::SmoothStreaming,
            "f4m" => TransportFamily::Hds,
            "rtmp" => TransportFamily::Rtmp(RtmpVariant::Rtmp),
            "rtmpe" => TransportFamily::Rtmp(RtmpVariant::Rtmpe),
            "rtmp_ffmpeg" => TransportFamily::Rtmp(RtmpVariant::RtmpFfmpegIdentity),
            "bunnycdn" | "soopvod" | "niconico_live" | "fc2_live" | "websocket_frag" => {
                TransportFamily::KnownExcluded(KnownExcludedTransport::PrivateLiveState)
            }
            "mhtml" | "youtube_live_chat" | "youtube_live_chat_replay" => {
                TransportFamily::KnownExcluded(KnownExcludedTransport::NonMedia)
            }
            _ => TransportFamily::Unknown,
        };

        Self { raw, family }
    }

    /// Возвращает exact raw identity.
    pub const fn raw(&self) -> &RawTransportIdentity {
        &self.raw
    }

    /// Возвращает parsed family.
    pub const fn family(&self) -> TransportFamily {
        self.family
    }
}

impl fmt::Debug for NormalizedTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NormalizedTransport")
            .field("raw", &self.raw)
            .field("family", &self.family)
            .finish()
    }
}

/// Нормализованная container family из S00 target/provisional rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContainerFamily {
    /// ISO BMFF: MP4/M4A/MOV.
    IsoBmff,
    /// Fragmented MP4/CMAF.
    FragmentedIsoBmff,
    /// Matroska без WebM restriction.
    Matroska,
    /// WebM.
    WebM,
    /// Ogg.
    Ogg,
    /// FLAC.
    Flac,
    /// RIFF/WAVE.
    Wav,
    /// AIFF.
    Aiff,
    /// Core Audio Format.
    Caf,
    /// MPEG audio elementary/container family.
    MpegAudio,
    /// Сырой AC-3 / E-AC-3 elementary stream (`.ac3`, `.eac3`, `.ec3`) без контейнера.
    Ac3Elementary,
    /// MPEG transport stream.
    MpegTs,
    /// Flash Video.
    Flv,
    /// Adobe Fragmented F4F.
    F4f,
    /// MPEG program stream: S00 provisional exclusion.
    MpegProgramStream,
    /// AVI: S00 provisional exclusion.
    Avi,
    /// ASF/WMV/WMA: S00 provisional exclusion.
    Asf,
    /// Неизвестная future identity.
    Unknown,
}

/// Raw ext/container hints вместе с независимо parsed families.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContainerIdentity {
    /// Exact `ext`, если extractor его предоставил.
    raw_extension: Option<RawExtensionIdentity>,
    /// Family, parsed из `ext`.
    extension_family: Option<ContainerFamily>,
    /// Exact `container`, если extractor его предоставил.
    raw_container: Option<RawContainerIdentity>,
    /// Family, parsed из `container`.
    container_family: Option<ContainerFamily>,
}

impl ContainerIdentity {
    /// Парсит оба hint-а независимо, не скрывая конфликт или отсутствие.
    pub fn parse(
        raw_extension: Option<RawExtensionIdentity>,
        raw_container: Option<RawContainerIdentity>,
    ) -> Self {
        let extension_family = raw_extension
            .as_ref()
            .map(|identity| parse_container_family(identity.as_str()));
        let container_family = raw_container
            .as_ref()
            .map(|identity| parse_container_family(identity.as_str()));

        Self {
            raw_extension,
            extension_family,
            raw_container,
            container_family,
        }
    }

    /// Возвращает exact `ext`.
    pub const fn raw_extension(&self) -> Option<&RawExtensionIdentity> {
        self.raw_extension.as_ref()
    }

    /// Возвращает parsed `ext`.
    pub const fn extension_family(&self) -> Option<ContainerFamily> {
        self.extension_family
    }

    /// Возвращает exact `container`.
    pub const fn raw_container(&self) -> Option<&RawContainerIdentity> {
        self.raw_container.as_ref()
    }

    /// Возвращает parsed `container`.
    pub const fn container_family(&self) -> Option<ContainerFamily> {
        self.container_family
    }

    /// Возвращает единственную непротиворечивую family.
    ///
    /// Unknown hint не подменяет известный hint. Две разные известные family
    /// возвращают typed conflict вместо угадывания по приоритету полей.
    pub fn consistent_family(&self) -> Result<Option<ContainerFamily>, ContainerHintConflict> {
        let extension = self
            .extension_family
            .filter(|family| *family != ContainerFamily::Unknown);
        let container = self
            .container_family
            .filter(|family| *family != ContainerFamily::Unknown);

        match (extension, container) {
            (Some(left), Some(right)) => resolve_compatible_container_family(left, right)
                .map(Some)
                .ok_or(ContainerHintConflict {
                    extension: left,
                    container: right,
                }),
            (Some(family), None) | (None, Some(family)) => Ok(Some(family)),
            (None, None) => Ok(None),
        }
    }
}

/// Разрешает только доказанные refinement-отношения между container hints.
///
/// `ext` обычно описывает внешний тип файла, а `container` может уточнять его
/// внутренний профиль. Поэтому fMP4 не конфликтует с MP4, WebM — с Matroska,
/// а F4F — с FLV. Любая иная пара остаётся настоящим typed conflict.
fn resolve_compatible_container_family(
    extension: ContainerFamily,
    container: ContainerFamily,
) -> Option<ContainerFamily> {
    if extension == container {
        return Some(extension);
    }

    match (extension, container) {
        (ContainerFamily::IsoBmff, ContainerFamily::FragmentedIsoBmff)
        | (ContainerFamily::FragmentedIsoBmff, ContainerFamily::IsoBmff) => {
            Some(ContainerFamily::FragmentedIsoBmff)
        }
        (ContainerFamily::Matroska, ContainerFamily::WebM)
        | (ContainerFamily::WebM, ContainerFamily::Matroska) => Some(ContainerFamily::WebM),
        (ContainerFamily::Flv, ContainerFamily::F4f)
        | (ContainerFamily::F4f, ContainerFamily::Flv) => Some(ContainerFamily::F4f),
        _ => None,
    }
}

impl fmt::Debug for ContainerIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContainerIdentity")
            .field("raw_extension", &self.raw_extension)
            .field("extension_family", &self.extension_family)
            .field("raw_container", &self.raw_container)
            .field("container_family", &self.container_family)
            .finish()
    }
}

/// Конфликт двух известных container hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainerHintConflict {
    /// Family из `ext`.
    pub extension: ContainerFamily,
    /// Family из `container`.
    pub container: ContainerFamily,
}

/// Тип media, которому принадлежит codec family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CodecMediaKind {
    /// Video.
    Video,
    /// Audio.
    Audio,
}

/// Нормализованная codec family из S00 target/provisional scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CodecFamily {
    /// VP8.
    Vp8,
    /// VP9.
    Vp9,
    /// AV1.
    Av1,
    /// H.264/AVC.
    H264,
    /// H.265/HEVC.
    H265,
    /// Opus.
    Opus,
    /// Vorbis.
    Vorbis,
    /// AAC.
    Aac,
    /// Generic ISO BMFF audio sample entry с ещё не доказанным object type.
    IsoBmffAudio,
    /// ADPCM family.
    Adpcm,
    /// Apple Lossless.
    Alac,
    /// FLAC.
    Flac,
    /// MPEG-1 Layer I.
    Mp1,
    /// MPEG-1 Layer II.
    Mp2,
    /// MPEG Layer III.
    Mp3,
    /// PCM family.
    Pcm,
    /// Dolby Digital (AC-3, ATSC A/52 bsid ≤ 8 и downgraded 9/10).
    ///
    /// Отдельная family от E-AC-3: decoder capability публикуется для каждой отдельно.
    Ac3,
    /// Dolby Digital Plus (E-AC-3, ATSC A/52 Annex E, bsid 16).
    Eac3,
}

impl CodecFamily {
    /// Возвращает media kind без попытки доказать runtime decode capability.
    pub const fn media_kind(self) -> CodecMediaKind {
        match self {
            Self::Vp8 | Self::Vp9 | Self::Av1 | Self::H264 | Self::H265 => CodecMediaKind::Video,
            Self::Opus
            | Self::Vorbis
            | Self::Aac
            | Self::IsoBmffAudio
            | Self::Adpcm
            | Self::Alac
            | Self::Flac
            | Self::Mp1
            | Self::Mp2
            | Self::Mp3
            | Self::Pcm
            | Self::Ac3
            | Self::Eac3 => CodecMediaKind::Audio,
        }
    }
}

/// Результат parsing-а codec identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CodecKind {
    /// Extractor явно сообщил отсутствие codec-а через `none`.
    Absent,
    /// Известная codec family.
    Known(CodecFamily),
    /// Future/unknown codec, raw identity сохранена.
    Unknown,
}

/// Raw+parsed codec identity с exact parameter tokens.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NormalizedCodec {
    /// Exact `vcodec`/`acodec`.
    raw: RawCodecIdentity,
    /// Parsed family либо absence/unknown.
    kind: CodecKind,
    /// Exact dot-separated suffix tokens без family prefix.
    parameters: Box<[String]>,
}

impl NormalizedCodec {
    /// Парсит family и параметры без потери исходной строки.
    pub fn parse(raw: RawCodecIdentity) -> Self {
        let mut parts = raw.as_str().split('.');
        let family_token = parts.next().unwrap_or_default().to_ascii_lowercase();
        let parameters: Box<[String]> = parts.map(ToOwned::to_owned).collect();
        let kind = parse_codec_kind(&family_token, &parameters);

        Self {
            raw,
            kind,
            parameters,
        }
    }

    /// Возвращает exact raw codec identity.
    pub const fn raw(&self) -> &RawCodecIdentity {
        &self.raw
    }

    /// Возвращает parsed kind.
    pub const fn kind(&self) -> CodecKind {
        self.kind
    }

    /// Возвращает exact parameter tokens в исходном регистре.
    pub fn parameters(&self) -> impl ExactSizeIterator<Item = &str> {
        self.parameters.iter().map(String::as_str)
    }
}

impl fmt::Debug for NormalizedCodec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NormalizedCodec")
            .field("raw", &self.raw)
            .field("kind", &self.kind)
            .field("parameter_count", &self.parameters.len())
            .finish()
    }
}

/// Проверяет S00 bound для exact raw identity.
fn validate_raw_identity(
    exact_value: &str,
    field: RawIdentityField,
) -> Result<(), RawIdentityBuildError> {
    if exact_value.is_empty() {
        return Err(RawIdentityBuildError::Empty { field });
    }

    if exact_value.len() > MAX_RAW_IDENTITY_UTF8_BYTES {
        return Err(RawIdentityBuildError::TooLong {
            field,
            provided_bytes: exact_value.len(),
            maximum_bytes: MAX_RAW_IDENTITY_UTF8_BYTES,
        });
    }

    Ok(())
}

/// Нормализует ext/container token без изменения raw owner-а.
fn parse_container_family(raw: &str) -> ContainerFamily {
    match raw.to_ascii_lowercase().as_str() {
        "mp4" | "m4a" | "m4v" | "mov" | "isom" | "f4v" => ContainerFamily::IsoBmff,
        "fmp4" | "cmaf" | "m4a_dash" | "mp4_dash" | "isma" | "ismv" => {
            ContainerFamily::FragmentedIsoBmff
        }
        "mkv" | "matroska" => ContainerFamily::Matroska,
        "webm" | "webm_dash" => ContainerFamily::WebM,
        "ogg" | "oga" | "ogv" => ContainerFamily::Ogg,
        "flac" => ContainerFamily::Flac,
        "wav" | "wave" => ContainerFamily::Wav,
        "aif" | "aiff" => ContainerFamily::Aiff,
        "caf" => ContainerFamily::Caf,
        "mp1" | "mp2" | "mp3" | "mpeg_audio" => ContainerFamily::MpegAudio,
        "ac3" | "eac3" | "ec3" => ContainerFamily::Ac3Elementary,
        "ts" | "m2ts" | "mpegts" => ContainerFamily::MpegTs,
        "flv" => ContainerFamily::Flv,
        "f4f" => ContainerFamily::F4f,
        "mpg" | "mpeg" | "mpegps" => ContainerFamily::MpegProgramStream,
        "avi" => ContainerFamily::Avi,
        "asf" | "wmv" | "wma" => ContainerFamily::Asf,
        _ => ContainerFamily::Unknown,
    }
}

/// Нормализует codec prefix и использует mp4a object type только там, где он однозначен.
fn parse_codec_kind(family_token: &str, parameters: &[String]) -> CodecKind {
    let known = match family_token {
        "none" => return CodecKind::Absent,
        "vp8" | "vp08" | "v_vp8" => CodecFamily::Vp8,
        "vp9" | "vp09" | "v_vp9" => CodecFamily::Vp9,
        "av1" | "av01" | "v_av1" => CodecFamily::Av1,
        "h264" | "avc1" | "avc3" | "v_mpeg4/iso/avc" => CodecFamily::H264,
        "h265" | "hevc" | "hev1" | "hvc1" | "v_mpegh/iso/hevc" => CodecFamily::H265,
        "opus" | "a_opus" => CodecFamily::Opus,
        "vorbis" | "a_vorbis" => CodecFamily::Vorbis,
        "aac" | "a_aac" | "aacl" => CodecFamily::Aac,
        token if token.starts_with("a_aac/") => CodecFamily::Aac,
        "mp4a" => mp4a_codec_family(parameters),
        token if token.starts_with("adpcm") || token.starts_with("a_adpcm") => CodecFamily::Adpcm,
        "alac" | "a_alac" => CodecFamily::Alac,
        "flac" | "a_flac" => CodecFamily::Flac,
        "mp1" | "a_mp1" => CodecFamily::Mp1,
        "mp2" | "a_mp2" => CodecFamily::Mp2,
        "mp3" | "a_mp3" => CodecFamily::Mp3,
        token if token.starts_with("pcm") || token.starts_with("a_pcm") => CodecFamily::Pcm,
        // Точные написания: RFC 6381/HLS/DASH (`ac-3`, `ec-3`), yt-dlp (`ac3`, `eac3`),
        // Matroska/demux codec id (`a_ac3`, `a_eac3`). `ec+3` — E-AC-3 с JOC (Atmos):
        // основной поток остаётся обычным E-AC-3 и декодируется как канальная основа.
        // Префиксные wildcard-ы намеренно не используются: `ac-4`, `a_ac3/bsid9` и
        // подобные строки остаются `Unknown`.
        "ac-3" | "ac3" | "a_ac3" => CodecFamily::Ac3,
        "ec-3" | "ec+3" | "eac3" | "a_eac3" => CodecFamily::Eac3,
        _ => return CodecKind::Unknown,
    };

    CodecKind::Known(known)
}

/// Интерпретирует только доказанные MPEG-4 object type indication значения.
fn mp4a_codec_family(parameters: &[String]) -> CodecFamily {
    match parameters.first().map(|value| value.to_ascii_lowercase()) {
        Some(object_type) if object_type == "40" => CodecFamily::Aac,
        Some(object_type) if object_type == "69" || object_type == "6b" => CodecFamily::Mp3,
        _ => CodecFamily::IsoBmffAudio,
    }
}

#[cfg(test)]
mod tests;
