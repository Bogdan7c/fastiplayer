//! Syntactic direct Smooth `/Manifest` classifier и startup adapter.

use super::*;

/// Stable root target и initial-only extractor fallback locator.
struct NativeSmoothStartupAdapter {
    /// Reconstructible root без fragment/template material.
    source: crate::media_open::NativeSmoothUrl,
    /// Fallback locator живёт только до authoritative initial admission.
    fallback_locator: service_ytdlp::YtDlpMediaLocator,
}

impl StartupUrlServiceAdapter for NativeSmoothStartupAdapter {
    fn safe_label(&self) -> &str {
        self.source.safe_label().as_str()
    }

    fn start(
        self: Box<Self>,
        controller: &mut StartupMediaController,
        app_state: &mut AppState,
        app_config: &AppConfig,
        system_capabilities: &SystemCapabilities,
    ) {
        controller.start_native_smooth_startup_job(
            self.source,
            self.fallback_locator,
            app_state,
            app_config,
            system_capabilities,
        );
    }

    fn into_media_open_source_request(
        self: Box<Self>,
        app_config: &AppConfig,
        system_capabilities: &SystemCapabilities,
        audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    ) -> Result<crate::media_open::MediaOpenSourceRequest, String> {
        Ok(crate::media_open::MediaOpenSourceRequest::Web(
            crate::media_open::WebMediaOpenRequest::native_smooth(
                self.source,
                crate::media_open::NativeSmoothOpenIntent::InitialWithYtDlpFallback {
                    fallback_locator: self.fallback_locator,
                },
                crate::media_open::WebMediaOpenSettings::from_app_config(
                    app_config,
                    system_capabilities,
                    audio_capabilities,
                ),
            ),
        ))
    }

    fn expose_secret_for_persistence(&self) -> &str {
        self.source.target().expose_secret_for_request()
    }

    fn requires_sensitive_persistence_acknowledgement(&self) -> bool {
        self.fallback_locator
            .requires_sensitive_persistence_acknowledgement()
    }

    fn requires_sensitive_export_acknowledgement(&self) -> bool {
        self.fallback_locator
            .requires_sensitive_export_acknowledgement()
    }
}

/// Регистрирует только HTTP(S) path, оканчивающийся exact `/Manifest` segment-ом.
pub(super) fn classify_native_smooth_startup_url(argument: &str) -> ServiceClassifierResult {
    let Ok(parsed) = url::Url::parse(argument) else {
        return ServiceClassifierResult::NotUrl;
    };
    let is_manifest_path = parsed
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .is_some_and(|segment| segment.eq_ignore_ascii_case("Manifest"));
    if !matches!(parsed.scheme(), "http" | "https") || !is_manifest_path {
        return ServiceClassifierResult::NotUrl;
    }
    let Ok(target) = source_core::HttpRequestTarget::parse_exact(argument) else {
        return ServiceClassifierResult::UnclaimedUrl {
            reason: StartupUrlUnsupportedReason::InvalidSyntax,
        };
    };
    let Ok(fallback_locator) = service_ytdlp::parse_yt_dlp_media_locator(argument) else {
        return ServiceClassifierResult::UnclaimedUrl {
            reason: StartupUrlUnsupportedReason::UnsupportedScheme,
        };
    };
    let safe_label =
        crate::media_open::SafeMediaLabel::from_service_safe_label(fallback_locator.safe_label());
    ServiceClassifierResult::Supported(StartupUrlLocator::new(NativeSmoothStartupAdapter {
        source: crate::media_open::NativeSmoothUrl::new(target, safe_label),
        fallback_locator,
    }))
}
