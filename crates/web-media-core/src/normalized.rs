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
            Self::Empty { field } => write!(formatter, "{field:?} identity не может быть пустой"),
            Self::TooLong {
                field,
                provided_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "{field:?} identity занимает {provided_bytes} bytes при лимите {maximum_bytes}"
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
            | Self::Pcm => CodecMediaKind::Audio,
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
mod tests {
    use super::*;

    /// Unknown identity остаётся exact и не протекает в Debug.
    #[test]
    fn raw_identity_is_preserved_with_bounds_and_redaction() {
        let raw_text = "Future_Protocol.V2";
        let raw = RawTransportIdentity::new(raw_text).expect("raw identity валидна");
        let normalized = NormalizedTransport::parse(raw);

        assert_eq!(normalized.raw().as_str(), raw_text);
        assert_eq!(normalized.family(), TransportFamily::Unknown);
        assert!(!format!("{normalized:?}").contains(raw_text));

        let error = RawCodecIdentity::new("x".repeat(MAX_RAW_IDENTITY_UTF8_BYTES + 1))
            .expect_err("overflow обязан быть rejected");
        assert_eq!(
            error,
            RawIdentityBuildError::TooLong {
                field: RawIdentityField::Codec,
                provided_bytes: MAX_RAW_IDENTITY_UTF8_BYTES + 1,
                maximum_bytes: MAX_RAW_IDENTITY_UTF8_BYTES,
            }
        );
    }

    /// Protocol aliases нормализуются по S00 manifest.
    #[test]
    fn transport_aliases_map_to_manifest_families() {
        // HLS alias обязан остаться в concrete adaptive family.
        let hls = NormalizedTransport::parse(
            RawTransportIdentity::new("m3u8_native").expect("identity валидна"),
        );
        // DASH alias обязан остаться в concrete adaptive family.
        let dash = NormalizedTransport::parse(
            RawTransportIdentity::new("dash_frag_urls").expect("identity валидна"),
        );
        // Generator identity хранится отдельно от serializable DASH fragments.
        let generator = NormalizedTransport::parse(
            RawTransportIdentity::new("http_dash_segments_generator").expect("identity валидна"),
        );

        // Known HLS alias не теряет family semantics.
        assert_eq!(hls.family(), TransportFamily::Hls);
        // Known DASH alias не теряет family semantics.
        assert_eq!(dash.family(), TransportFamily::Dash);
        // Generator остаётся typed exclusion без generic provider fallback.
        assert_eq!(
            generator.family(),
            TransportFamily::KnownExcluded(KnownExcludedTransport::DashGenerator)
        );

        // Каждая special identity обязана оставаться exact member-ом одной excluded family.
        for special_identity in [
            "bunnycdn",
            "soopvod",
            "niconico_live",
            "fc2_live",
            "websocket_frag",
        ] {
            // Парсим exact raw identity без alias normalization между providers.
            let special_transport = NormalizedTransport::parse(
                RawTransportIdentity::new(special_identity).expect("identity валидна"),
            );
            // Serializable protocol string не является admission воспроизводимого provider-а.
            assert_eq!(
                special_transport.family(),
                TransportFamily::KnownExcluded(KnownExcludedTransport::PrivateLiveState),
                "special identity `{special_identity}` неожиданно получила provider family"
            );
        }
    }

    /// Codec parser отделяет family от exact parameters.
    #[test]
    fn codec_family_and_parameters_are_parsed_without_raw_loss() {
        let avc = NormalizedCodec::parse(
            RawCodecIdentity::new("avc1.640028").expect("codec identity валидна"),
        );
        let aac = NormalizedCodec::parse(
            RawCodecIdentity::new("mp4a.40.2").expect("codec identity валидна"),
        );
        let future = NormalizedCodec::parse(
            RawCodecIdentity::new("futureCodec.Profile-X").expect("codec identity валидна"),
        );

        assert_eq!(avc.kind(), CodecKind::Known(CodecFamily::H264));
        assert_eq!(avc.parameters().collect::<Vec<_>>(), vec!["640028"]);
        assert_eq!(aac.kind(), CodecKind::Known(CodecFamily::Aac));
        assert_eq!(aac.parameters().collect::<Vec<_>>(), vec!["40", "2"]);
        assert_eq!(future.kind(), CodecKind::Unknown);
        assert_eq!(future.raw().as_str(), "futureCodec.Profile-X");
        assert_eq!(future.parameters().collect::<Vec<_>>(), vec!["Profile-X"]);
    }

    /// Runtime container ids нормализуются тем же owner-ом, что extractor codec names.
    #[test]
    fn container_codec_ids_map_to_public_codec_families_without_raw_loss() {
        let cases = [
            ("V_VP9", CodecFamily::Vp9),
            ("V_MPEG4/ISO/AVC", CodecFamily::H264),
            ("A_OPUS", CodecFamily::Opus),
            ("A_VORBIS", CodecFamily::Vorbis),
            ("A_AAC", CodecFamily::Aac),
            ("A_AAC/MPEG4/LC", CodecFamily::Aac),
            ("AACL", CodecFamily::Aac),
            ("A_FLAC", CodecFamily::Flac),
            ("A_PCM_S16LE", CodecFamily::Pcm),
        ];

        for (raw_codec, expected_family) in cases {
            let codec = NormalizedCodec::parse(
                RawCodecIdentity::new(raw_codec).expect("container codec identity валидна"),
            );
            assert_eq!(codec.kind(), CodecKind::Known(expected_family));
            assert_eq!(codec.raw().as_str(), raw_codec);
        }
    }

    /// Конфликт известных ext/container hints не разрешается угадыванием.
    #[test]
    fn container_hint_conflict_is_typed() {
        let identity = ContainerIdentity::parse(
            Some(RawExtensionIdentity::new("webm").expect("ext валиден")),
            Some(RawContainerIdentity::new("mp4").expect("container валиден")),
        );

        assert_eq!(
            identity.consistent_family(),
            Err(ContainerHintConflict {
                extension: ContainerFamily::WebM,
                container: ContainerFamily::IsoBmff,
            })
        );
    }

    /// Более точный container hint не конфликтует с совместимым file extension.
    #[test]
    fn container_refinement_pairs_resolve_to_the_more_specific_family() {
        let cases = [
            ("mp4", "mp4_dash", ContainerFamily::FragmentedIsoBmff),
            ("mkv", "webm", ContainerFamily::WebM),
            ("flv", "f4f", ContainerFamily::F4f),
        ];

        for (extension, container, expected) in cases {
            let identity = ContainerIdentity::parse(
                Some(RawExtensionIdentity::new(extension).expect("ext валиден")),
                Some(RawContainerIdentity::new(container).expect("container валиден")),
            );

            assert_eq!(identity.consistent_family(), Ok(Some(expected)));
        }
    }

    /// F4V — ISO-BMFF identity, тогда как F4F остаётся отдельным Adobe fragment path.
    #[test]
    fn f4v_routes_to_iso_bmff_without_collapsing_f4f_into_flv() {
        let f4v = ContainerIdentity::parse(
            Some(RawExtensionIdentity::new("f4v").expect("ext валиден")),
            None,
        );
        let f4f = ContainerIdentity::parse(
            Some(RawExtensionIdentity::new("f4f").expect("ext валиден")),
            None,
        );

        assert_eq!(f4v.extension_family(), Some(ContainerFamily::IsoBmff));
        assert_eq!(f4v.consistent_family(), Ok(Some(ContainerFamily::IsoBmff)));
        assert_eq!(f4f.extension_family(), Some(ContainerFamily::F4f));
        assert_eq!(f4f.consistent_family(), Ok(Some(ContainerFamily::F4f)));
    }

    /// yt-dlp Smooth Streaming extensions обозначают fragmented ISO-BMFF tracks.
    #[test]
    fn isma_and_ismv_route_to_fragmented_iso_bmff() {
        for extension in ["isma", "ismv"] {
            let identity = ContainerIdentity::parse(
                Some(RawExtensionIdentity::new(extension).expect("ext валиден")),
                None,
            );
            assert_eq!(
                identity.consistent_family(),
                Ok(Some(ContainerFamily::FragmentedIsoBmff))
            );
        }
    }
}
