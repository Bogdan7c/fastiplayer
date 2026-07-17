//! Pure admission policy для metadata, полученной от generic `yt-dlp`.
//!
//! Модуль не запускает процесс, не выбирает decoder/backend и не открывает I/O.
//! Его единственная ответственность — доказать, что metadata описывает ровно
//! одну поддерживаемую v1 topology: direct HTTP(S) WebM video-only VP9 плюс
//! отдельный direct HTTP(S) WebM audio-only Opus.

use std::fmt;

use url::Url;

use crate::dto::{YtDlpFormat, YtDlpMetadata};
use crate::error::YtDlpServiceError;

/// Typed причина, почему extractor metadata находится вне v1 envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpCompatibilityRejection {
    /// Extractor не вернул ни одного format entry.
    MissingFormats,

    /// Media или хотя бы один его format явно помечен DRM.
    DrmProtected,

    /// Manifest содержит только audio streams.
    AudioOnly,

    /// Доступен только muxed video+audio format.
    MuxedOnly,

    /// Нет отдельного video-only stream-а.
    MissingSeparateVideo,

    /// Нет отдельного audio-only stream-а.
    MissingSeparateAudio,

    /// Direct URL отсутствует или не является absolute HTTP(S).
    MissingDirectHttpUrl,

    /// Stream доступен только через HLS/DASH/fragment protocol.
    FragmentedProtocol,

    /// Direct stream container не WebM.
    UnsupportedContainer,

    /// Separate WebM streams не образуют VP9+Opus пару.
    UnsupportedCodec,
}

impl fmt::Display for YtDlpCompatibilityRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MissingFormats => "metadata не содержит formats",
            Self::DrmProtected => "DRM media не поддерживается",
            Self::AudioOnly => "audio-only media не поддерживается",
            Self::MuxedOnly => "muxed-only media не поддерживается",
            Self::MissingSeparateVideo => "отсутствует отдельный video stream",
            Self::MissingSeparateAudio => "отсутствует отдельный audio stream",
            Self::MissingDirectHttpUrl => "отсутствует direct HTTP(S) URL",
            Self::FragmentedProtocol => "fragment/HLS/DASH protocol не поддерживается",
            Self::UnsupportedContainer => "container должен быть WebM",
            Self::UnsupportedCodec => "нужна отдельная VP9+Opus пара",
        })
    }
}

/// Доказанный набор format references для дальнейшей нормализации resolver-ом.
#[derive(Debug)]
pub(crate) struct AdmittedFormats<'metadata> {
    /// Все совместимые video candidates; capability selection выполняется позже.
    pub(crate) video_formats: Vec<&'metadata YtDlpFormat>,

    /// Совместимые audio companions; resolver выберет лучший deterministic score.
    pub(crate) audio_formats: Vec<&'metadata YtDlpFormat>,
}

/// Проверяет collection/DRM/topology/transport/container/codec invariants.
pub(crate) fn admit_formats<'metadata>(
    metadata: &YtDlpMetadata,
    formats: &'metadata [YtDlpFormat],
) -> Result<AdmittedFormats<'metadata>, YtDlpServiceError> {
    ensure_single_item(metadata)?;
    if metadata.has_drm == Some(true) || formats.iter().any(format_declares_drm) {
        return Err(no_compatible(YtDlpCompatibilityRejection::DrmProtected));
    }
    if formats.is_empty() {
        return Err(no_compatible(YtDlpCompatibilityRejection::MissingFormats));
    }

    let video_formats = formats
        .iter()
        .filter(|format| is_compatible_video(format))
        .collect::<Vec<_>>();
    let audio_formats = formats
        .iter()
        .filter(|format| is_compatible_audio(format))
        .collect::<Vec<_>>();

    if !video_formats.is_empty() && !audio_formats.is_empty() {
        return Ok(AdmittedFormats {
            video_formats,
            audio_formats,
        });
    }

    Err(no_compatible(classify_topology_rejection(
        formats,
        video_formats.is_empty(),
        audio_formats.is_empty(),
    )))
}

/// Проверяет только single-item topology для metadata-only enrichment boundary.
pub(crate) fn ensure_single_item(metadata: &YtDlpMetadata) -> Result<(), YtDlpServiceError> {
    if metadata_describes_collection(metadata) {
        return Err(YtDlpServiceError::CollectionUrl);
    }

    Ok(())
}

/// Collection topology определяется только extractor-owned структурными полями.
fn metadata_describes_collection(metadata: &YtDlpMetadata) -> bool {
    let collection_type = metadata
        .entry_type
        .as_deref()
        .is_some_and(|entry_type| matches!(entry_type, "playlist" | "multi_video"));
    collection_type || metadata.entries.is_some()
}

/// Явный `has_drm=true` запрещает format до transport open.
fn format_declares_drm(format: &YtDlpFormat) -> bool {
    format.has_drm == Some(true)
}

/// Проверяет полный video-only WebM/VP9/direct HTTP contract.
fn is_compatible_video(format: &YtDlpFormat) -> bool {
    is_video_only(format) && has_direct_http_transport(format) && is_webm(format) && is_vp9(format)
}

/// Проверяет полный audio-only WebM/Opus/direct HTTP contract.
fn is_compatible_audio(format: &YtDlpFormat) -> bool {
    is_audio_only(format) && has_direct_http_transport(format) && is_webm(format) && is_opus(format)
}

/// Video-only означает настоящий vcodec и отсутствие audio codec.
fn is_video_only(format: &YtDlpFormat) -> bool {
    codec_is_present(format.vcodec.as_deref()) && codec_is_absent(format.acodec.as_deref())
}

/// Audio-only означает настоящий acodec и отсутствие video codec.
fn is_audio_only(format: &YtDlpFormat) -> bool {
    codec_is_present(format.acodec.as_deref()) && codec_is_absent(format.vcodec.as_deref())
}

/// Muxed format одновременно содержит настоящий video и audio codec.
fn is_muxed(format: &YtDlpFormat) -> bool {
    codec_is_present(format.vcodec.as_deref()) && codec_is_present(format.acodec.as_deref())
}

/// Codec считается присутствующим только для непустого значения, отличного от `none`.
fn codec_is_present(codec: Option<&str>) -> bool {
    codec
        .map(str::trim)
        .is_some_and(|codec| !codec.is_empty() && !codec.eq_ignore_ascii_case("none"))
}

/// Отсутствующее, пустое и `none` значения одинаково означают отсутствие codec lane.
fn codec_is_absent(codec: Option<&str>) -> bool {
    !codec_is_present(codec)
}

/// Проверяет exact WebM container hint.
fn is_webm(format: &YtDlpFormat) -> bool {
    format
        .ext
        .as_deref()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("webm"))
}

/// VP9 допускает короткий yt-dlp tag `vp9` и подробный `vp09.*`.
fn is_vp9(format: &YtDlpFormat) -> bool {
    format
        .vcodec
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .is_some_and(|codec| codec.starts_with("vp9") || codec.starts_with("vp09"))
}

/// Opus сравнивается без учёта регистра, но без fuzzy aliases.
fn is_opus(format: &YtDlpFormat) -> bool {
    format
        .acodec
        .as_deref()
        .map(str::trim)
        .is_some_and(|codec| codec.eq_ignore_ascii_case("opus"))
}

/// Transport допускает только настоящий direct HTTP(S) URL и non-fragment protocol.
fn has_direct_http_transport(format: &YtDlpFormat) -> bool {
    let Some(protocol) = format.protocol.as_deref() else {
        return false;
    };
    if !matches!(protocol.to_ascii_lowercase().as_str(), "http" | "https") {
        return false;
    }

    let Some(raw_url) = format.url.as_deref().filter(|url| !url.trim().is_empty()) else {
        return false;
    };
    let Ok(parsed_url) = Url::parse(raw_url) else {
        return false;
    };

    matches!(parsed_url.scheme(), "http" | "https") && parsed_url.host().is_some()
}

/// Выбирает максимально полезную aggregate-причину, не отражая raw metadata.
fn classify_topology_rejection(
    formats: &[YtDlpFormat],
    missing_compatible_video: bool,
    missing_compatible_audio: bool,
) -> YtDlpCompatibilityRejection {
    let has_video_only = formats.iter().any(is_video_only);
    let has_audio_only = formats.iter().any(is_audio_only);
    let has_muxed = formats.iter().any(is_muxed);

    if !has_video_only && has_audio_only && !has_muxed {
        return YtDlpCompatibilityRejection::AudioOnly;
    }
    if !has_video_only && !has_audio_only && has_muxed {
        return YtDlpCompatibilityRejection::MuxedOnly;
    }
    if formats.iter().any(|format| {
        format.protocol.as_deref().is_some_and(|protocol| {
            !matches!(protocol.to_ascii_lowercase().as_str(), "http" | "https")
        })
    }) {
        return YtDlpCompatibilityRejection::FragmentedProtocol;
    }
    if formats
        .iter()
        .any(|format| format.url.as_deref().is_none_or(str::is_empty))
        || !formats.iter().any(has_direct_http_transport)
    {
        return YtDlpCompatibilityRejection::MissingDirectHttpUrl;
    }
    if formats.iter().all(|format| !is_webm(format)) {
        return YtDlpCompatibilityRejection::UnsupportedContainer;
    }
    if has_video_only && has_audio_only && (missing_compatible_video || missing_compatible_audio) {
        return YtDlpCompatibilityRejection::UnsupportedCodec;
    }
    if missing_compatible_video {
        return YtDlpCompatibilityRejection::MissingSeparateVideo;
    }

    YtDlpCompatibilityRejection::MissingSeparateAudio
}

/// Оборачивает typed admission reason в единый service boundary error.
fn no_compatible(reason: YtDlpCompatibilityRejection) -> YtDlpServiceError {
    YtDlpServiceError::NoCompatibleStreams { reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Строит минимальный hermetic format без HTTP/network I/O.
    fn media_format(
        url: Option<&str>,
        protocol: Option<&str>,
        extension: &str,
        video_codec: &str,
        audio_codec: &str,
    ) -> YtDlpFormat {
        YtDlpFormat {
            url: url.map(str::to_string),
            protocol: protocol.map(str::to_string),
            has_drm: Some(false),
            format_id: None,
            ext: Some(extension.to_string()),
            vcodec: Some(video_codec.to_string()),
            acodec: Some(audio_codec.to_string()),
            width: None,
            height: None,
            fps: None,
            tbr: None,
            vbr: None,
            abr: None,
            dynamic_range: None,
            filesize: None,
            filesize_approx: None,
            duration: None,
            http_headers: None,
        }
    }

    /// Строит single-item metadata; конкретные topology flags тест меняет явно.
    fn single_item_metadata() -> YtDlpMetadata {
        YtDlpMetadata {
            entry_type: Some("video".to_string()),
            entries: None,
            has_drm: Some(false),
            title: None,
            id: None,
            format_id: None,
            height: None,
            fps: None,
            vcodec: None,
            acodec: None,
            duration: None,
            is_live: None,
            live_status: None,
            requested_downloads: None,
            requested_formats: None,
            formats: None,
        }
    }

    /// Извлекает typed compatibility reason без сравнения текстов.
    fn rejection_reason(
        metadata: &YtDlpMetadata,
        formats: &[YtDlpFormat],
    ) -> YtDlpCompatibilityRejection {
        match admit_formats(metadata, formats).expect_err("fixture должна быть отклонена")
        {
            YtDlpServiceError::NoCompatibleStreams { reason } => reason,
            other_error => panic!("ожидалась compatibility ошибка, получена {other_error:?}"),
        }
    }

    #[test]
    fn direct_http_webm_vp9_and_opus_pair_is_admitted() {
        let formats = vec![
            media_format(
                Some("https://cdn.example.test/video.webm"),
                Some("https"),
                "webm",
                "vp9",
                "none",
            ),
            media_format(
                Some("https://cdn.example.test/audio.webm"),
                Some("https"),
                "webm",
                "none",
                "opus",
            ),
        ];

        let admitted =
            admit_formats(&single_item_metadata(), &formats).expect("compatible pair admitted");

        assert_eq!(admitted.video_formats.len(), 1);
        assert_eq!(admitted.audio_formats.len(), 1);
    }

    #[test]
    fn missing_url_and_fragment_protocols_are_typed_rejections() {
        let missing_url = vec![
            media_format(None, Some("https"), "webm", "vp9", "none"),
            media_format(
                Some("https://cdn.example.test/audio.webm"),
                Some("https"),
                "webm",
                "none",
                "opus",
            ),
        ];
        assert_eq!(
            rejection_reason(&single_item_metadata(), &missing_url),
            YtDlpCompatibilityRejection::MissingDirectHttpUrl
        );

        let missing_protocol = vec![
            media_format(
                Some("https://cdn.example.test/video.webm"),
                None,
                "webm",
                "vp9",
                "none",
            ),
            media_format(
                Some("https://cdn.example.test/audio.webm"),
                None,
                "webm",
                "none",
                "opus",
            ),
        ];
        assert_eq!(
            rejection_reason(&single_item_metadata(), &missing_protocol),
            YtDlpCompatibilityRejection::MissingDirectHttpUrl
        );

        for fragmented_protocol in ["m3u8_native", "http_dash_segments"] {
            let fragmented = vec![
                media_format(
                    Some("https://cdn.example.test/video.webm"),
                    Some(fragmented_protocol),
                    "webm",
                    "vp9",
                    "none",
                ),
                media_format(
                    Some("https://cdn.example.test/audio.webm"),
                    Some(fragmented_protocol),
                    "webm",
                    "none",
                    "opus",
                ),
            ];
            assert_eq!(
                rejection_reason(&single_item_metadata(), &fragmented),
                YtDlpCompatibilityRejection::FragmentedProtocol
            );
        }
    }

    #[test]
    fn muxed_audio_only_and_unsupported_codecs_are_rejected() {
        let muxed = vec![media_format(
            Some("https://cdn.example.test/muxed.webm"),
            Some("https"),
            "webm",
            "vp9",
            "opus",
        )];
        assert_eq!(
            rejection_reason(&single_item_metadata(), &muxed),
            YtDlpCompatibilityRejection::MuxedOnly
        );

        let audio_only = vec![media_format(
            Some("https://cdn.example.test/audio.webm"),
            Some("https"),
            "webm",
            "none",
            "opus",
        )];
        assert_eq!(
            rejection_reason(&single_item_metadata(), &audio_only),
            YtDlpCompatibilityRejection::AudioOnly
        );

        let unsupported_codecs = vec![
            media_format(
                Some("https://cdn.example.test/video.webm"),
                Some("https"),
                "webm",
                "av01.0.08M.08",
                "none",
            ),
            media_format(
                Some("https://cdn.example.test/audio.webm"),
                Some("https"),
                "webm",
                "none",
                "vorbis",
            ),
        ];
        assert_eq!(
            rejection_reason(&single_item_metadata(), &unsupported_codecs),
            YtDlpCompatibilityRejection::UnsupportedCodec
        );
    }

    #[test]
    fn drm_and_collection_topologies_are_rejected_before_stream_selection() {
        let mut drm_video = media_format(
            Some("https://cdn.example.test/video.webm"),
            Some("https"),
            "webm",
            "vp9",
            "none",
        );
        drm_video.has_drm = Some(true);
        assert_eq!(
            rejection_reason(&single_item_metadata(), &[drm_video]),
            YtDlpCompatibilityRejection::DrmProtected
        );

        let mut collection = single_item_metadata();
        collection.entry_type = Some("playlist".to_string());
        collection.entries = Some(Vec::new());
        assert!(matches!(
            admit_formats(&collection, &[]),
            Err(YtDlpServiceError::CollectionUrl)
        ));
    }
}
