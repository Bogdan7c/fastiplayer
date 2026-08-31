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
    /// Generic yt-dlp owner повторно использует exact service-owned locator.
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
        audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    ) -> Result<crate::media_open::MediaOpenSourceRequest, String> {
        self.0
            .into_media_open_source_request(app_config, system_capabilities, audio_capabilities)
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

    /// Возвращает borrowed typed locator только для topology-first Add URL boundary.
    pub(crate) fn yt_dlp_topology_locator(&self) -> Option<&service_ytdlp::YtDlpMediaLocator> {
        self.0.yt_dlp_topology_locator()
    }

    /// Возвращает typed yt-dlp scheme только для app-owned capability admission.
    fn yt_dlp_input_scheme(&self) -> Option<service_ytdlp::YtDlpInputScheme> {
        self.0.yt_dlp_input_scheme()
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
        audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
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

    fn yt_dlp_topology_locator(&self) -> Option<&service_ytdlp::YtDlpMediaLocator> {
        None
    }

    fn yt_dlp_input_scheme(&self) -> Option<service_ytdlp::YtDlpInputScheme> {
        None
    }
}

struct YtDlpStartupAdapter {
    locator: service_ytdlp::YtDlpMediaLocator,
}

/// `.m3u8` остаётся только admission hint-ом: фактический VOD profile доказывает HLS owner.
struct NativeHlsStartupAdapter {
    source: crate::media_open::NativeHlsUrl,
    fallback_locator: service_ytdlp::YtDlpMediaLocator,
}

impl StartupUrlServiceAdapter for NativeHlsStartupAdapter {
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
        controller.start_native_hls_startup_job(
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
        Ok(crate::media_open::MediaOpenSourceRequest::NativeHls {
            source: self.source,
            intent: crate::media_open::NativeHlsOpenIntent::InitialWithYtDlpFallback {
                fallback_locator: self.fallback_locator,
            },
            network_config: app_config.network.clone(),
            web_media_config: app_config.web_media.clone(),
            yt_dlp_config: app_config.yt_dlp.clone(),
            demux_config: app_config.player.demux,
            preferred_video_codec_order: app_config.player.preferred_video_codec_order.clone(),
            system_capabilities: Box::new(system_capabilities.clone()),
            audio_capabilities,
        })
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
        audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    ) -> Result<crate::media_open::MediaOpenSourceRequest, String> {
        self.validate_config(app_config)?;
        Ok(crate::media_open::MediaOpenSourceRequest::YtDlp {
            locator: self.locator,
            selection_intent: crate::web_media_open::YtDlpCandidateOpenIntent::BestPlayable,
            network_config: app_config.network.clone(),
            web_media_config: app_config.web_media.clone(),
            yt_dlp_config: app_config.yt_dlp.clone(),
            demux_config: app_config.player.demux,
            preferred_video_codec_order: app_config.player.preferred_video_codec_order.clone(),
            system_capabilities: Box::new(system_capabilities.clone()),
            audio_capabilities,
        })
    }

    fn expose_secret_for_persistence(&self) -> &str {
        self.locator.expose_secret_for_persistence()
    }

    fn requires_sensitive_persistence_acknowledgement(&self) -> bool {
        self.locator
            .requires_sensitive_persistence_acknowledgement()
    }

    fn requires_sensitive_export_acknowledgement(&self) -> bool {
        self.locator.requires_sensitive_export_acknowledgement()
    }

    fn playlist_metadata_source(&self) -> Option<PlaylistUrlMetadataSource> {
        Some(PlaylistUrlMetadataSource::YtDlp(self.locator.clone()))
    }

    fn yt_dlp_topology_locator(&self) -> Option<&service_ytdlp::YtDlpMediaLocator> {
        Some(&self.locator)
    }

    fn yt_dlp_input_scheme(&self) -> Option<service_ytdlp::YtDlpInputScheme> {
        Some(self.locator.input_scheme())
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
        _audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
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
    UnclaimedUrl { reason: StartupUrlUnsupportedReason },

    /// Service принял URL и вернул typed adapter с нормализованным locator-ом.
    Supported(StartupUrlLocator),
}

/// Pure classifier, который один URL service регистрирует в app composition root.
type StartupUrlServiceClassifier = fn(&str) -> ServiceClassifierResult;

/// Typed capability, которую composition регистрирует только для готового provider-а.
///
/// Само наличие значения означает статус `Implemented`: `Planned` и
/// `ProfileExcluded` rows нельзя представить этим типом и случайно включить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImplementedYtDlpInputProviderCapability {
    input_scheme: service_ytdlp::YtDlpInputScheme,
}

impl ImplementedYtDlpInputProviderCapability {
    /// Создаёт registration row для одного exact scheme без alias expansion.
    const fn exact(input_scheme: service_ytdlp::YtDlpInputScheme) -> Self {
        Self { input_scheme }
    }
}

/// App-owned registry отделяет pure service parsing от runtime availability.
struct StartupUrlServiceRegistry<'capabilities> {
    implemented_yt_dlp_input_providers: &'capabilities [ImplementedYtDlpInputProviderCapability],
}

impl StartupUrlServiceRegistry<'_> {
    /// Выполняет direct-first traversal и фиксирует первый выбранный adapter.
    fn classify(&self, argument: &str) -> StartupUrlClassification {
        let mut recognized_url = false;
        let mut last_rejection = None;

        for classifier in STARTUP_URL_SERVICE_CLASSIFIERS {
            match classifier(argument) {
                ServiceClassifierResult::NotUrl => {}
                ServiceClassifierResult::UnclaimedUrl { reason } => {
                    recognized_url = true;
                    last_rejection = Some(reason);
                }
                ServiceClassifierResult::Supported(locator) => {
                    if let Some(input_scheme) = profile_excluded_input_scheme(&locator) {
                        recognized_url = true;
                        last_rejection =
                            Some(StartupUrlUnsupportedReason::ProfileExcludedInputScheme {
                                input_scheme,
                            });
                    } else if let Some(input_scheme) =
                        self.missing_implemented_provider_for_locator(&locator)
                    {
                        recognized_url = true;
                        last_rejection = Some(
                            StartupUrlUnsupportedReason::ImplementedProviderUnavailable {
                                input_scheme,
                            },
                        );
                    } else {
                        return StartupUrlClassification::Supported(locator);
                    }
                }
            }
        }

        if recognized_url {
            let reason = last_rejection.unwrap_or(StartupUrlUnsupportedReason::NoRegisteredService);
            StartupUrlClassification::Unsupported { reason }
        } else {
            StartupUrlClassification::NotUrl
        }
    }

    /// Возвращает только extended scheme, которой не хватает exact provider row.
    fn missing_implemented_provider_for_locator(
        &self,
        locator: &StartupUrlLocator,
    ) -> Option<service_ytdlp::YtDlpInputScheme> {
        let Some(input_scheme) = locator.yt_dlp_input_scheme() else {
            // Direct-media admission полностью принадлежит его classifier-у.
            return None;
        };
        if input_scheme.is_http_fallback()
            || self
                .implemented_yt_dlp_input_providers
                .iter()
                .any(|capability| capability.input_scheme == input_scheme)
        {
            None
        } else {
            Some(input_scheme)
        }
    }
}

/// S37 зарегистрировал exact FTP(S); исключённые schemes сюда не добавляются.
const PRODUCTION_YT_DLP_INPUT_PROVIDERS: &[ImplementedYtDlpInputProviderCapability] = &[
    ImplementedYtDlpInputProviderCapability::exact(service_ytdlp::YtDlpInputScheme::Ftp),
    ImplementedYtDlpInputProviderCapability::exact(service_ytdlp::YtDlpInputScheme::Ftps),
];

/// Exact top-level schemes, которые parser сохраняет как typed identity, но
/// утверждённый serializable profile намеренно не допускает к wire playback.
const PROFILE_EXCLUDED_YT_DLP_INPUT_SCHEMES: &[service_ytdlp::YtDlpInputScheme] = &[
    service_ytdlp::YtDlpInputScheme::Rtmp,
    service_ytdlp::YtDlpInputScheme::Rtmpe,
];

/// Единственный production registry используется CLI, toolbar, import и reopen.
const PRODUCTION_STARTUP_URL_SERVICE_REGISTRY: StartupUrlServiceRegistry<'static> =
    StartupUrlServiceRegistry {
        implemented_yt_dlp_input_providers: PRODUCTION_YT_DLP_INPUT_PROVIDERS,
    };

/// Возвращает exact scheme только для намеренного profile exclusion.
///
/// Проверка живёт в composition root: pure locator parser отвечает за typed
/// identity, а app registry — за фактический release disposition.
fn profile_excluded_input_scheme(
    locator: &StartupUrlLocator,
) -> Option<service_ytdlp::YtDlpInputScheme> {
    locator
        .yt_dlp_input_scheme()
        .filter(|input_scheme| PROFILE_EXCLUDED_YT_DLP_INPUT_SCHEMES.contains(input_scheme))
}

/// Единственное место регистрации URL services; общий traversal не знает их семантику.
const STARTUP_URL_SERVICE_CLASSIFIERS: &[StartupUrlServiceClassifier] = &[
    classify_native_hls_startup_url,
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
    Unsupported {
        /// Стабильная typed причина без raw input.
        reason: StartupUrlUnsupportedReason,
    },
}

/// Typed причины, по которым общий registry не выбрал URL adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupUrlUnsupportedReason {
    /// Absolute URL синтаксически некорректен.
    InvalidSyntax,

    /// Scheme не входит в exact approved service vocabulary.
    UnsupportedScheme,

    /// Scheme approved parser-ом, но transport provider ещё не `Implemented`.
    ImplementedProviderUnavailable {
        /// Exact approved scheme, для которого отсутствует registration row.
        input_scheme: service_ytdlp::YtDlpInputScheme,
    },

    /// Scheme распознана как typed identity, но исключена текущим profile.
    ProfileExcludedInputScheme {
        /// Exact scheme без raw locator и credential material.
        input_scheme: service_ytdlp::YtDlpInputScheme,
    },

    /// Ни один service не заявил URL после успешного URL recognition.
    NoRegisteredService,
}

impl StartupUrlUnsupportedReason {
    /// Формирует bounded message только из typed/static данных.
    pub(crate) fn safe_error(self) -> String {
        match self {
            Self::InvalidSyntax => "NetworkError: некорректный URL".to_string(),
            Self::UnsupportedScheme => {
                "NetworkError: URL scheme не поддерживается media services".to_string()
            }
            Self::ImplementedProviderUnavailable { input_scheme } => format!(
                "NetworkError: provider для `{}` ещё не реализован",
                input_scheme.as_str()
            ),
            Self::ProfileExcludedInputScheme { input_scheme } => format!(
                "NetworkError: `{}` исключён утверждённым compatibility profile",
                input_scheme.as_str()
            ),
            Self::NoRegisteredService => {
                "NetworkError: URL не поддерживается media services".to_string()
            }
        }
    }
}

/// Последовательно спрашивает единый production registry без app parser-а.
pub(crate) fn classify_startup_url(argument: &str) -> StartupUrlClassification {
    PRODUCTION_STARTUP_URL_SERVICE_REGISTRY.classify(argument)
}

/// Повторно открывает persisted domain locator через тот же service registry, без app parser-а.
#[allow(dead_code)] // Session 10C/14 вызовет boundary после state-load/controller wiring.
pub(crate) fn classify_playlist_url(
    locator: &playlist_core::SecretUrlLocator,
) -> StartupUrlClassification {
    classify_startup_url(locator.expose_secret_for_open())
}

/// Регистрирует только syntactic `.m3u8` hint; bytes/profile остаются authoritative.
fn classify_native_hls_startup_url(argument: &str) -> ServiceClassifierResult {
    let Ok(parsed) = url::Url::parse(argument) else {
        return ServiceClassifierResult::NotUrl;
    };
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.path().to_ascii_lowercase().ends_with(".m3u8")
    {
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
    ServiceClassifierResult::Supported(StartupUrlLocator::new(NativeHlsStartupAdapter {
        source: crate::media_open::NativeHlsUrl::new(target, safe_label),
        fallback_locator,
    }))
}

/// Generic adapter pure-парсит exact approved schemes; registry решает availability.
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
                reason: StartupUrlUnsupportedReason::InvalidSyntax,
            }
        }
        Err(service_ytdlp::YtDlpLocatorParseError::UnsupportedScheme) => {
            ServiceClassifierResult::UnclaimedUrl {
                reason: StartupUrlUnsupportedReason::UnsupportedScheme,
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
        }) => ServiceClassifierResult::UnclaimedUrl {
            reason: StartupUrlUnsupportedReason::UnsupportedScheme,
        },
        Err(service_direct_media::DirectMediaOpenError::InvalidUrl { .. }) => {
            ServiceClassifierResult::UnclaimedUrl {
                reason: StartupUrlUnsupportedReason::InvalidSyntax,
            }
        }
        Err(_) => ServiceClassifierResult::UnclaimedUrl {
            reason: StartupUrlUnsupportedReason::NoRegisteredService,
        },
    }
}

#[cfg(test)]
mod tests;
