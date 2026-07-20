use std::fmt;

use capability_core::SystemCapabilities;
use rustiplayer_config::AppConfig;

use crate::startup_media::StartupMediaController;
use crate::state::AppState;

/// Service-neutral type erasure для уже разобранного URL service request-а.
pub(crate) struct StartupUrlLocator(Box<dyn StartupUrlServiceAdapter>);

/// Typed source для необязательного фонового обогащения playlist metadata.
#[derive(Clone)]
pub(crate) enum PlaylistUrlMetadataSource {
    /// Generic yt-dlp owner повторно использует exact HTTP(S) locator.
    YtDlp(service_ytdlp::YtDlpMediaLocator),
}

impl StartupUrlLocator {
    fn new(adapter: impl StartupUrlServiceAdapter + 'static) -> Self {
        Self(Box::new(adapter))
    }

    /// Безопасный label для tracing/UI status.
    pub(crate) fn safe_label(&self) -> &str {
        self.0.safe_label()
    }

    /// Проверяет service-specific runtime enablement без знания policy в caller-е.
    pub(crate) fn validate_config(&self, app_config: &AppConfig) -> Result<(), String> {
        self.0.validate_config(app_config)
    }

    /// Делегирует существующему startup owner-у только выбор typed job adapter-а.
    pub(crate) fn start(
        self,
        controller: &mut StartupMediaController,
        app_state: &mut AppState,
        app_config: &AppConfig,
        system_capabilities: &SystemCapabilities,
    ) {
        self.0
            .start(controller, app_state, app_config, system_capabilities);
    }

    /// Строит request для общего media-open coordinator-а без второго URL parser-а.
    pub(crate) fn into_media_open_source_request(
        self,
        app_config: &AppConfig,
        system_capabilities: &SystemCapabilities,
    ) -> Result<crate::media_open::MediaOpenSourceRequest, String> {
        self.0
            .into_media_open_source_request(app_config, system_capabilities)
    }

    /// Переносит уже нормализованную service identity в service-neutral playlist domain.
    #[allow(dead_code)] // Session 10C/14 подключит mapping к media-open/persistence lifecycle.
    pub(crate) fn to_playlist_locator(
        &self,
    ) -> Result<playlist_core::SecretUrlLocator, playlist_core::PlaylistLocatorBuildError> {
        playlist_core::SecretUrlLocator::from_reopenable_url(self.0.expose_secret_for_persistence())
    }

    /// D15 требует acknowledgement только для exact persisted direct URL identity.
    pub(crate) fn requires_sensitive_persistence_acknowledgement(&self) -> bool {
        self.0.requires_sensitive_persistence_acknowledgement()
    }

    /// Export применяет более строгую service-owned portable-document policy.
    pub(crate) fn requires_sensitive_export_acknowledgement(&self) -> bool {
        self.0.requires_sensitive_export_acknowledgement()
    }

    /// Возвращает service-owned metadata source, если adapter поддерживает enrichment.
    pub(crate) fn playlist_metadata_source(&self) -> Option<PlaylistUrlMetadataSource> {
        self.0.playlist_metadata_source()
    }
}

impl fmt::Debug for StartupUrlLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StartupUrlLocator")
            .field("safe_label", &self.safe_label())
            .finish_non_exhaustive()
    }
}

trait StartupUrlServiceAdapter: Send {
    fn safe_label(&self) -> &str;

    fn validate_config(&self, _app_config: &AppConfig) -> Result<(), String> {
        Ok(())
    }

    fn start(
        self: Box<Self>,
        controller: &mut StartupMediaController,
        app_state: &mut AppState,
        app_config: &AppConfig,
        system_capabilities: &SystemCapabilities,
    );

    fn into_media_open_source_request(
        self: Box<Self>,
        app_config: &AppConfig,
        system_capabilities: &SystemCapabilities,
    ) -> Result<crate::media_open::MediaOpenSourceRequest, String>;

    #[allow(dead_code)] // Используется только intent-named domain mapping-ом выше.
    fn expose_secret_for_persistence(&self) -> &str;

    fn requires_sensitive_persistence_acknowledgement(&self) -> bool {
        false
    }

    fn requires_sensitive_export_acknowledgement(&self) -> bool {
        self.requires_sensitive_persistence_acknowledgement()
    }

    fn playlist_metadata_source(&self) -> Option<PlaylistUrlMetadataSource> {
        None
    }
}

struct YtDlpStartupAdapter {
    locator: service_ytdlp::YtDlpMediaLocator,
}

impl StartupUrlServiceAdapter for YtDlpStartupAdapter {
    fn safe_label(&self) -> &str {
        self.locator.safe_label()
    }

    fn validate_config(&self, app_config: &AppConfig) -> Result<(), String> {
        if app_config.yt_dlp.enabled {
            Ok(())
        } else {
            Err("NetworkError: URL service adapter отключён в config".to_string())
        }
    }

    fn start(
        self: Box<Self>,
        controller: &mut StartupMediaController,
        app_state: &mut AppState,
        app_config: &AppConfig,
        system_capabilities: &SystemCapabilities,
    ) {
        controller.start_yt_dlp_startup_job(
            self.locator,
            app_state,
            app_config,
            system_capabilities,
        );
    }

    fn into_media_open_source_request(
        self: Box<Self>,
        app_config: &AppConfig,
        system_capabilities: &SystemCapabilities,
    ) -> Result<crate::media_open::MediaOpenSourceRequest, String> {
        self.validate_config(app_config)?;
        Ok(crate::media_open::MediaOpenSourceRequest::YtDlp {
            locator: self.locator,
            required_stream_identity: None,
            network_config: app_config.network.clone(),
            yt_dlp_config: app_config.yt_dlp.clone(),
            demux_config: app_config.player.demux,
            preferred_video_codec_order: app_config.player.preferred_video_codec_order.clone(),
            system_capabilities: system_capabilities.clone(),
        })
    }

    fn expose_secret_for_persistence(&self) -> &str {
        self.locator.expose_secret_for_persistence()
    }

    fn requires_sensitive_export_acknowledgement(&self) -> bool {
        self.locator.requires_sensitive_export_acknowledgement()
    }

    fn playlist_metadata_source(&self) -> Option<PlaylistUrlMetadataSource> {
        Some(PlaylistUrlMetadataSource::YtDlp(self.locator.clone()))
    }
}

struct DirectMediaStartupAdapter {
    locator: service_direct_media::DirectMediaUrl,
}

impl StartupUrlServiceAdapter for DirectMediaStartupAdapter {
    fn safe_label(&self) -> &str {
        self.locator.safe_label()
    }

    fn start(
        self: Box<Self>,
        controller: &mut StartupMediaController,
        app_state: &mut AppState,
        app_config: &AppConfig,
        _system_capabilities: &SystemCapabilities,
    ) {
        controller.start_direct_media_startup_job(self.locator, app_state, app_config);
    }

    fn into_media_open_source_request(
        self: Box<Self>,
        app_config: &AppConfig,
        _system_capabilities: &SystemCapabilities,
    ) -> Result<crate::media_open::MediaOpenSourceRequest, String> {
        Ok(crate::media_open::MediaOpenSourceRequest::Direct {
            locator: self.locator,
            network_config: app_config.network.clone(),
            demux_config: app_config.player.demux,
        })
    }

    fn expose_secret_for_persistence(&self) -> &str {
        self.locator.expose_secret_for_persistence()
    }

    fn requires_sensitive_persistence_acknowledgement(&self) -> bool {
        self.locator
            .requires_sensitive_persistence_acknowledgement()
    }
}

/// Результат одного service-owned classifier-а для общего registry traversal.
enum ServiceClassifierResult {
    /// Аргумент не выглядит URL для этого classifier-а.
    NotUrl,

    /// Аргумент является URL, но этот service его не принял.
    UnclaimedUrl { safe_error: Option<String> },

    /// Service принял URL и вернул typed adapter с нормализованным locator-ом.
    Supported(StartupUrlLocator),
}

/// Pure classifier, который один URL service регистрирует в app composition root.
type StartupUrlServiceClassifier = fn(&str) -> ServiceClassifierResult;

/// Единственное место регистрации URL services; общий traversal не знает их семантику.
const STARTUP_URL_SERVICE_CLASSIFIERS: &[StartupUrlServiceClassifier] = &[
    classify_direct_media_startup_url,
    classify_yt_dlp_startup_url,
];

/// Safe classification result одного CLI argument-а.
pub(crate) enum StartupUrlClassification {
    /// Аргумент не похож на web URL и остаётся local-path candidate-ом.
    NotUrl,

    /// Один service adapter принял URL и вернул typed locator.
    Supported(StartupUrlLocator),

    /// URL выглядит сетевым, но ни один зарегистрированный adapter не принял его.
    Unsupported { safe_error: String },
}

/// Последовательно спрашивает зарегистрированные service owners без app parser-а.
pub(crate) fn classify_startup_url(argument: &str) -> StartupUrlClassification {
    let mut recognized_url = false;
    let mut last_safe_error = None;

    for classifier in STARTUP_URL_SERVICE_CLASSIFIERS {
        match classifier(argument) {
            ServiceClassifierResult::NotUrl => {}
            ServiceClassifierResult::UnclaimedUrl { safe_error } => {
                recognized_url = true;
                if safe_error.is_some() {
                    last_safe_error = safe_error;
                }
            }
            ServiceClassifierResult::Supported(locator) => {
                return StartupUrlClassification::Supported(locator);
            }
        }
    }

    if recognized_url {
        StartupUrlClassification::Unsupported {
            safe_error: last_safe_error.unwrap_or_else(|| {
                "NetworkError: URL не поддерживается media services".to_string()
            }),
        }
    } else {
        StartupUrlClassification::NotUrl
    }
}

/// Повторно открывает persisted domain locator через тот же service registry, без app parser-а.
#[allow(dead_code)] // Session 10C/14 вызовет boundary после state-load/controller wiring.
pub(crate) fn classify_playlist_url(
    locator: &playlist_core::SecretUrlLocator,
) -> StartupUrlClassification {
    classify_startup_url(locator.expose_secret_for_open())
}

/// Generic fallback adapter принимает любой оставшийся absolute HTTP(S) URL.
fn classify_yt_dlp_startup_url(argument: &str) -> ServiceClassifierResult {
    if !service_ytdlp::is_probably_url(argument) {
        return ServiceClassifierResult::NotUrl;
    }

    match service_ytdlp::parse_yt_dlp_media_locator(argument) {
        Ok(locator) => {
            ServiceClassifierResult::Supported(StartupUrlLocator::new(YtDlpStartupAdapter {
                locator,
            }))
        }
        Err(service_ytdlp::YtDlpLocatorParseError::InvalidSyntax) => {
            ServiceClassifierResult::UnclaimedUrl {
                safe_error: Some("NetworkError: некорректный URL".to_string()),
            }
        }
        Err(service_ytdlp::YtDlpLocatorParseError::UnsupportedScheme) => {
            ServiceClassifierResult::UnclaimedUrl {
                safe_error: Some(
                    "NetworkError: yt-dlp поддерживает только HTTP(S) URL".to_string(),
                ),
            }
        }
    }
}

/// Direct-media registration adapter: service сам владеет extension/open policy.
fn classify_direct_media_startup_url(argument: &str) -> ServiceClassifierResult {
    if !service_direct_media::looks_like_url(argument) {
        return ServiceClassifierResult::NotUrl;
    }

    match service_direct_media::parse_direct_media_url(argument) {
        Ok(locator) => {
            ServiceClassifierResult::Supported(StartupUrlLocator::new(DirectMediaStartupAdapter {
                locator,
            }))
        }
        Err(service_direct_media::DirectMediaOpenError::UnsupportedUrl {
            reason: service_direct_media::DirectMediaUrlUnsupportedReason::UnsupportedProtocol,
        }) => {
            let protocol = argument
                .split_once("://")
                .map_or("unknown", |(protocol, _)| protocol);
            ServiceClassifierResult::UnclaimedUrl {
                safe_error: Some(format!(
                    "NetworkError: protocol `{protocol}` не поддерживается; direct media v1 принимает только http(s)"
                )),
            }
        }
        Err(error) => ServiceClassifierResult::UnclaimedUrl {
            safe_error: Some(format!(
                "NetworkError: Direct media URL unsupported: {error}"
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use capability_core::SystemCapabilities;
    use playlist_core::SecretUrlLocator;
    use rustiplayer_config::AppConfig;

    use super::{StartupUrlClassification, classify_playlist_url, classify_startup_url};

    fn persistence_identity(locator: &super::StartupUrlLocator) -> String {
        locator
            .to_playlist_locator()
            .expect("service locator должен создавать непустую domain identity")
            .expose_secret_for_persistence()
            .to_string()
    }

    #[test]
    fn registry_keeps_exact_generic_identity_in_service_owner() {
        let exact_url = "https://youtu.be/video-id?v=keep&si=preserve&unknown=preserve";
        let classified = classify_startup_url(exact_url);
        let StartupUrlClassification::Supported(locator) = classified else {
            panic!("generic yt-dlp service должен принять HTTP(S) URL");
        };

        assert_eq!(persistence_identity(&locator), exact_url);
    }

    #[test]
    fn direct_signed_url_keeps_exact_identity() {
        let secret = "https://cdn.example.test/video.mp4?signature=a%2Bb&part=1+2";
        let classified = classify_startup_url(secret);
        let StartupUrlClassification::Supported(locator) = classified else {
            panic!("generic direct service должен принять signed media URL");
        };

        assert_eq!(persistence_identity(&locator), secret);
    }

    #[test]
    fn playlist_locator_reopens_through_same_service_registry() {
        let domain_locator = SecretUrlLocator::from_reopenable_url(
            "https://youtu.be/video-id?si=drop&future=preserve",
        )
        .expect("domain URL identity должна быть непустой");
        let StartupUrlClassification::Supported(service_locator) =
            classify_playlist_url(&domain_locator)
        else {
            panic!("persisted locator должен быть принят service registry");
        };

        assert_eq!(
            persistence_identity(&service_locator),
            "https://youtu.be/video-id?si=drop&future=preserve"
        );
    }

    #[test]
    fn unsupported_error_does_not_reflect_secret_input() {
        let classified = classify_startup_url("https://user:password@[invalid-host]?token=secret");
        let StartupUrlClassification::Unsupported { safe_error } = classified else {
            panic!("синтаксически невалидный URL должен быть rejected");
        };

        assert!(!safe_error.contains("password"));
        assert!(!safe_error.contains("secret"));
        assert!(!safe_error.contains("private"));
    }

    #[test]
    fn registry_prioritizes_direct_media_and_uses_yt_dlp_as_generic_fallback() {
        let StartupUrlClassification::Supported(direct_locator) =
            classify_startup_url("https://cdn.example.test/video.mp4?token=direct")
        else {
            panic!("direct media URL должен быть принят");
        };
        let direct_request = direct_locator
            .into_media_open_source_request(&AppConfig::default(), &SystemCapabilities::empty(1))
            .expect("direct request");
        assert!(matches!(
            direct_request,
            crate::media_open::MediaOpenSourceRequest::Direct { .. }
        ));

        let exact_generic_url =
            "https://user:password@media.example.test/article?token=generic#chapter";
        let StartupUrlClassification::Supported(generic_locator) =
            classify_startup_url(exact_generic_url)
        else {
            panic!("оставшийся HTTP(S) URL должен попасть в yt-dlp fallback");
        };
        assert_eq!(persistence_identity(&generic_locator), exact_generic_url);
        assert!(
            !generic_locator.requires_sensitive_persistence_acknowledgement(),
            "утверждённая generic policy сохраняет exact yt-dlp URL без отдельного prompt"
        );
        let generic_request = generic_locator
            .into_media_open_source_request(&AppConfig::default(), &SystemCapabilities::empty(1))
            .expect("yt-dlp request");
        assert!(matches!(
            generic_request,
            crate::media_open::MediaOpenSourceRequest::YtDlp { .. }
        ));
    }
}
