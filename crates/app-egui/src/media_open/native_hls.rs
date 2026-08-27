//! App-owned identity и reopen intent для доказанного native HLS VOD.

use std::fmt;

use super::types::SafeMediaLabel;

/// Reconstructible native HLS top-level identity без URL в diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NativeHlsUrl {
    target: source_core::HttpRequestTarget,
    safe_label: SafeMediaLabel,
}

impl NativeHlsUrl {
    /// Сохраняет exact request target и отдельно уже redacted UI label.
    #[must_use]
    pub(crate) fn new(target: source_core::HttpRequestTarget, safe_label: SafeMediaLabel) -> Self {
        Self { target, safe_label }
    }

    /// Exact locator раскрывается только app-owned HTTP composition owner-у.
    #[must_use]
    pub(crate) const fn target(&self) -> &source_core::HttpRequestTarget {
        &self.target
    }

    /// Возвращает bounded label без URL/query material.
    #[must_use]
    pub(crate) const fn safe_label(&self) -> &SafeMediaLabel {
        &self.safe_label
    }
}

impl fmt::Debug for NativeHlsUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeHlsUrl")
            .field("target", &"<redacted>")
            .field("safe_label", &self.safe_label)
            .finish()
    }
}

/// Initial admission может ровно один раз перейти в unchanged extractor path;
/// exact reopen хранит только доказанную selection и при расхождении падает закрыто.
#[derive(Clone)]
pub(crate) enum NativeHlsOpenIntent {
    InitialWithYtDlpFallback {
        fallback_locator: service_ytdlp::YtDlpMediaLocator,
    },
    ExactSelection(web_media_hls::NativeHlsSemanticSelection),
}

impl fmt::Debug for NativeHlsOpenIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitialWithYtDlpFallback { .. } => {
                formatter.write_str("InitialWithYtDlpFallback(<redacted>)")
            }
            Self::ExactSelection(selection) => formatter
                .debug_tuple("ExactSelection")
                .field(selection)
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proven_media_selection() -> web_media_hls::NativeHlsSemanticSelection {
        let target =
            source_core::HttpRequestTarget::parse_exact("https://media.example.test/master.m3u8")
                .expect("valid target");
        let policy = web_media_hls::NativeHlsSelectionPolicy::new(
            web_media_core::PreferredHeightPolicy::NoPreference,
            vec![web_media_core::CodecFamily::H264],
        )
        .expect("valid policy");
        web_media_hls::admit_native_hls_vod(
            b"#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nsegment.ts\n#EXT-X-ENDLIST\n",
            &target,
            hls_playlist_core::HlsParserLimits::default(),
            &policy,
            None,
        )
        .expect("static media playlist must be admitted")
    }

    #[test]
    fn debug_never_reveals_exact_target_query() {
        let source = NativeHlsUrl::new(
            source_core::HttpRequestTarget::parse_exact(
                "https://media.example.test/master.m3u8?access_token=top-secret",
            )
            .expect("valid target"),
            SafeMediaLabel::from_service_safe_label("media.example.test/master.m3u8"),
        );

        let debug = format!("{source:?}");
        assert!(!debug.contains("top-secret"));
        assert!(!debug.contains("access_token"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn exact_reopen_intent_cannot_retain_or_invoke_extractor_fallback() {
        let intent = NativeHlsOpenIntent::ExactSelection(proven_media_selection());

        match intent {
            NativeHlsOpenIntent::ExactSelection(_) => {}
            NativeHlsOpenIntent::InitialWithYtDlpFallback { .. } => {
                panic!("exact reopen не должен содержать fallback locator")
            }
        }
    }
}
