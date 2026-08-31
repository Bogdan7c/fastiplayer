//! Provider-neutral app composition boundary для web-media.
//!
//! Этот модуль намеренно отделяет устойчивый пользовательский intent от
//! временного transport material. В reconstructible source никогда не попадают
//! child endpoints, headers, cookies, keys, manifest bodies или runtime handles.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use media_core::{Demuxer, DynamicMediaTimelinePort, MediaTagMetadata, TrackInfo};
use player_core::{PreparedDemuxSeekPort, PreparedMedia};
use web_media_core::{
    ExtractorInvocationReason, WebMediaIngressKind, WebMediaPresentationKind,
    WebMediaRecoveryStrategy, WebMediaSelection,
};

use super::{NativeHlsOpenIntent, NativeHlsUrl, SafeMediaLabel};

/// Устойчивый app-owned web intent, публикуемый только после exact `Installed`.
///
/// Neutral lifecycle facts лежат рядом с закрытым adapter bridge. Благодаря
/// этому coordinator/recovery могут работать с одним web variant, а N05A/N05B
/// смогут заменить legacy projections без повторной миграции lifecycle envelope.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct WebMediaSourceIntent {
    /// Фактический ingress, которым был открыт установленный runtime.
    ingress: WebMediaIngressKind,
    /// Точный VOD/live lifecycle установленной presentation.
    presentation: WebMediaPresentationKind,
    /// Provider-neutral recovery intent без physical endpoint-а.
    recovery: WebMediaRecoveryStrategy,
    /// Optional product reason допустим только для extractor ingress-а.
    extractor_reason: Option<ExtractorInvocationReason>,
    /// Временный typed bridge к ещё не мигрированным N05A/N05B consumers.
    adapter: Box<WebMediaSourceAdapter>,
}

/// Закрытое adapter-owned содержимое устойчивого source intent.
///
/// Variant не выходит наружу как lifecycle source vocabulary. Его accessors
/// существуют только как временный compile bridge до N05B.
#[derive(Clone, PartialEq, Eq)]
enum WebMediaSourceAdapter {
    /// Direct resource целиком является active semantic selection.
    Direct {
        locator: service_direct_media::DirectMediaUrl,
    },
    /// Native HLS хранит только root locator и semantic selection без child URL.
    NativeHls {
        source: NativeHlsUrl,
        selection: web_media_hls::NativeHlsSemanticSelection,
    },
    /// Extractor сохраняет neutral selection и временные UI/reopen projections.
    Extractor {
        locator: service_ytdlp::YtDlpMediaLocator,
        neutral_selection: WebMediaSelection,
        candidate_selection: Box<service_ytdlp::YtDlpCandidateSelection>,
        composed_selection: Option<Box<service_ytdlp::YtDlpComposedSelection>>,
        stream_configuration: Box<crate::web_media_stream_model::WebMediaStreamConfiguration>,
        catalog_attachment: crate::web_media_catalog::WebMediaCatalogAttachment,
    },
}

/// Owned compatibility dispatch для ещё не мигрированных settings/action consumers.
pub(crate) enum WebMediaSourceAdapterBridge {
    Direct {
        locator: service_direct_media::DirectMediaUrl,
    },
    NativeHls {
        source: NativeHlsUrl,
        selection: web_media_hls::NativeHlsSemanticSelection,
    },
    Extractor {
        locator: service_ytdlp::YtDlpMediaLocator,
        candidate_selection: Box<service_ytdlp::YtDlpCandidateSelection>,
        composed_selection: Option<Box<service_ytdlp::YtDlpComposedSelection>>,
        stream_configuration: Box<crate::web_media_stream_model::WebMediaStreamConfiguration>,
    },
}

impl WebMediaSourceIntent {
    /// Создаёт stable direct-resource intent без extractor fallback/reason.
    pub(crate) fn direct(locator: service_direct_media::DirectMediaUrl) -> Self {
        Self {
            ingress: WebMediaIngressKind::DirectResource,
            presentation: WebMediaPresentationKind::Vod,
            recovery: WebMediaRecoveryStrategy::ReopenStableResource,
            extractor_reason: None,
            adapter: Box::new(WebMediaSourceAdapter::Direct { locator }),
        }
    }

    /// Создаёт proven native HLS VOD intent без временных rendition endpoints.
    pub(crate) fn native_hls_vod(
        source: NativeHlsUrl,
        selection: web_media_hls::NativeHlsSemanticSelection,
    ) -> Self {
        Self {
            ingress: WebMediaIngressKind::NativeManifest,
            presentation: WebMediaPresentationKind::Vod,
            recovery: WebMediaRecoveryStrategy::RefreshRootManifestAndRematch,
            extractor_reason: None,
            adapter: Box::new(WebMediaSourceAdapter::NativeHls { source, selection }),
        }
    }

    /// Создаёт extractor-backed intent из canonical neutral selection.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn extractor(
        locator: service_ytdlp::YtDlpMediaLocator,
        presentation: WebMediaPresentationKind,
        neutral_selection: WebMediaSelection,
        candidate_selection: service_ytdlp::YtDlpCandidateSelection,
        composed_selection: Option<Box<service_ytdlp::YtDlpComposedSelection>>,
        stream_configuration: crate::web_media_stream_model::WebMediaStreamConfiguration,
        catalog_attachment: crate::web_media_catalog::WebMediaCatalogAttachment,
        extractor_reason: ExtractorInvocationReason,
    ) -> Self {
        Self {
            ingress: WebMediaIngressKind::ExtractorBacked,
            presentation,
            recovery: WebMediaRecoveryStrategy::FreshExtractionAndRematch,
            extractor_reason: Some(extractor_reason),
            adapter: Box::new(WebMediaSourceAdapter::Extractor {
                locator,
                neutral_selection,
                candidate_selection: Box::new(candidate_selection),
                composed_selection,
                stream_configuration: Box::new(stream_configuration),
                catalog_attachment,
            }),
        }
    }

    /// Возвращает фактический ingress без знания adapter implementation.
    pub(crate) const fn ingress(&self) -> WebMediaIngressKind {
        self.ingress
    }

    /// Возвращает exact VOD/live lifecycle kind.
    pub(crate) const fn presentation(&self) -> WebMediaPresentationKind {
        self.presentation
    }

    /// Возвращает source-owned recovery intent без runtime attachment-а.
    pub(crate) const fn recovery(&self) -> WebMediaRecoveryStrategy {
        self.recovery
    }

    /// Возвращает extractor reason; native/direct intent всегда дают `None`.
    pub(crate) const fn extractor_reason(&self) -> Option<ExtractorInvocationReason> {
        self.extractor_reason
    }

    /// Возвращает canonical neutral selection только catalog-backed ingress-а.
    pub(crate) const fn neutral_selection(&self) -> Option<&WebMediaSelection> {
        match &*self.adapter {
            WebMediaSourceAdapter::Extractor {
                neutral_selection, ..
            } => Some(neutral_selection),
            WebMediaSourceAdapter::Direct { .. } | WebMediaSourceAdapter::NativeHls { .. } => None,
        }
    }

    /// Временный N05B bridge к direct locator-у.
    pub(crate) const fn direct_locator(&self) -> Option<&service_direct_media::DirectMediaUrl> {
        match &*self.adapter {
            WebMediaSourceAdapter::Direct { locator } => Some(locator),
            WebMediaSourceAdapter::NativeHls { .. } | WebMediaSourceAdapter::Extractor { .. } => {
                None
            }
        }
    }

    /// Временный N05B bridge к extractor reopen/UI projections.
    pub(crate) const fn extractor_bridge(&self) -> Option<ExtractorSourceBridge<'_>> {
        match &*self.adapter {
            WebMediaSourceAdapter::Extractor {
                locator,
                stream_configuration,
                ..
            } => Some(ExtractorSourceBridge {
                locator,
                stream_configuration,
            }),
            WebMediaSourceAdapter::Direct { .. } | WebMediaSourceAdapter::NativeHls { .. } => None,
        }
    }

    /// Передаёт catalog coordinator-у только neutral attachment, не locator/request material.
    pub(crate) const fn catalog_attachment(
        &self,
    ) -> Option<&crate::web_media_catalog::WebMediaCatalogAttachment> {
        match &*self.adapter {
            WebMediaSourceAdapter::Extractor {
                catalog_attachment, ..
            } => Some(catalog_attachment),
            WebMediaSourceAdapter::Direct { .. } | WebMediaSourceAdapter::NativeHls { .. } => None,
        }
    }

    /// Возвращает единый secret-safe read-only projection для catalog/sidebar owners.
    pub(crate) fn read_only_projection(&self) -> WebMediaSourceReadProjection<'_> {
        match &*self.adapter {
            WebMediaSourceAdapter::Direct { locator } => WebMediaSourceReadProjection {
                ingress: self.ingress,
                presentation: self.presentation,
                source_label: locator.safe_label(),
                stream_configuration: None,
            },
            WebMediaSourceAdapter::NativeHls { source, .. } => WebMediaSourceReadProjection {
                ingress: self.ingress,
                presentation: self.presentation,
                source_label: source.safe_label().as_str(),
                stream_configuration: None,
            },
            WebMediaSourceAdapter::Extractor {
                locator,
                stream_configuration,
                ..
            } => WebMediaSourceReadProjection {
                ingress: self.ingress,
                presentation: self.presentation,
                source_label: locator.safe_label(),
                stream_configuration: Some(stream_configuration),
            },
        }
    }

    /// Передаёт owned adapter bridge единственному settings rebuild dispatch-у.
    pub(crate) fn into_adapter_bridge(self) -> WebMediaSourceAdapterBridge {
        match *self.adapter {
            WebMediaSourceAdapter::Direct { locator } => {
                WebMediaSourceAdapterBridge::Direct { locator }
            }
            WebMediaSourceAdapter::NativeHls { source, selection } => {
                WebMediaSourceAdapterBridge::NativeHls { source, selection }
            }
            WebMediaSourceAdapter::Extractor {
                locator,
                candidate_selection,
                composed_selection,
                stream_configuration,
                ..
            } => WebMediaSourceAdapterBridge::Extractor {
                locator,
                candidate_selection,
                composed_selection,
                stream_configuration,
            },
        }
    }

    pub(crate) fn controlled_reopen_request(
        &self,
        network_config: rustiplayer_config::NetworkConfig,
        demux_config: rustiplayer_config::PlayerDemuxConfig,
        adaptive_settings: Option<WebMediaOpenSettings>,
    ) -> Option<WebMediaOpenRequest> {
        let adapter = match &*self.adapter {
            WebMediaSourceAdapter::Direct { locator } => WebMediaOpenAdapter::Direct {
                locator: locator.clone(),
                network_config,
                demux_config,
            },
            WebMediaSourceAdapter::NativeHls { source, selection } => {
                let settings = adaptive_settings?;
                WebMediaOpenAdapter::NativeHls {
                    source: source.clone(),
                    intent: NativeHlsOpenIntent::ExactSelection(selection.clone()),
                    settings,
                }
            }
            WebMediaSourceAdapter::Extractor {
                locator,
                candidate_selection,
                composed_selection,
                stream_configuration,
                ..
            } => {
                let settings = adaptive_settings?;
                let selection_intent = match composed_selection {
                    Some(composed_selection) => {
                        crate::web_media_open::YtDlpCandidateOpenIntent::composed(
                            composed_selection.clone(),
                            candidate_selection.clone(),
                            stream_configuration.preference(),
                        )
                    }
                    None => crate::web_media_open::YtDlpCandidateOpenIntent::
                        exact_preserving_installed_stream_configuration(
                            candidate_selection.clone(),
                            stream_configuration,
                        ),
                };
                WebMediaOpenAdapter::Extractor {
                    locator: locator.clone(),
                    selection_intent,
                    settings,
                }
            }
        };
        Some(WebMediaOpenRequest {
            adapter: Box::new(adapter),
        })
    }
}

impl fmt::Debug for WebMediaSourceIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebMediaSourceIntent")
            .field("ingress", &self.ingress())
            .field("presentation", &self.presentation())
            .field("recovery", &self.recovery())
            .field("extractor_reason", &self.extractor_reason())
            .field("stable_locator", &"<redacted-typed-locator>")
            .field(
                "selection",
                &self.neutral_selection().map(|_| "<semantic-selection>"),
            )
            .finish()
    }
}

/// Borrowed compatibility projection для N05A/N05B migration.
#[derive(Clone, Copy)]
pub(crate) struct ExtractorSourceBridge<'a> {
    pub(crate) locator: &'a service_ytdlp::YtDlpMediaLocator,
    pub(crate) stream_configuration: &'a crate::web_media_stream_model::WebMediaStreamConfiguration,
}

/// Borrowed read-only N04 projection без locator/request/exact identity material.
#[derive(Clone, Copy)]
pub(crate) struct WebMediaSourceReadProjection<'a> {
    pub(crate) ingress: WebMediaIngressKind,
    pub(crate) presentation: WebMediaPresentationKind,
    pub(crate) source_label: &'a str,
    pub(crate) stream_configuration:
        Option<&'a crate::web_media_stream_model::WebMediaStreamConfiguration>,
}

impl fmt::Debug for WebMediaSourceReadProjection<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebMediaSourceReadProjection")
            .field("ingress", &self.ingress)
            .field("presentation", &self.presentation)
            .field("source_label", &"<safe-label>")
            .field(
                "has_stream_configuration",
                &self.stream_configuration.is_some(),
            )
            .finish()
    }
}

/// Общий immutable settings snapshot одного web open/reopen.
#[derive(Clone)]
pub(crate) struct WebMediaOpenSettings {
    pub(crate) network_config: rustiplayer_config::NetworkConfig,
    pub(crate) web_media_config: rustiplayer_config::WebMediaConfig,
    pub(crate) yt_dlp_config: rustiplayer_config::YtDlpConfig,
    pub(crate) demux_config: rustiplayer_config::PlayerDemuxConfig,
    pub(crate) preferred_video_codec_order: Vec<rustiplayer_config::VideoCodec>,
    pub(crate) system_capabilities: Box<capability_core::SystemCapabilities>,
    pub(crate) audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
}

impl WebMediaOpenSettings {
    /// Снимает один immutable settings/capability snapshot до background open.
    pub(crate) fn from_app_config(
        app_config: &rustiplayer_config::AppConfig,
        system_capabilities: &capability_core::SystemCapabilities,
        audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    ) -> Self {
        Self {
            network_config: app_config.network.clone(),
            web_media_config: app_config.web_media.clone(),
            yt_dlp_config: app_config.yt_dlp.clone(),
            demux_config: app_config.player.demux,
            preferred_video_codec_order: app_config.player.preferred_video_codec_order.clone(),
            system_capabilities: Box::new(system_capabilities.clone()),
            audio_capabilities,
        }
    }
}

/// Единый web request, который внешний lifecycle видит одним variant-ом.
#[derive(Clone)]
pub(crate) struct WebMediaOpenRequest {
    adapter: Box<WebMediaOpenAdapter>,
}

/// Закрытый adapter dispatch внутри neutral request envelope.
#[derive(Clone)]
enum WebMediaOpenAdapter {
    Direct {
        locator: service_direct_media::DirectMediaUrl,
        network_config: rustiplayer_config::NetworkConfig,
        demux_config: rustiplayer_config::PlayerDemuxConfig,
    },
    NativeHls {
        source: NativeHlsUrl,
        intent: NativeHlsOpenIntent,
        settings: WebMediaOpenSettings,
    },
    Extractor {
        locator: service_ytdlp::YtDlpMediaLocator,
        selection_intent: crate::web_media_open::YtDlpCandidateOpenIntent,
        settings: WebMediaOpenSettings,
    },
}

impl WebMediaOpenRequest {
    /// Создаёт direct request без extractor-capable settings.
    pub(crate) fn direct(
        locator: service_direct_media::DirectMediaUrl,
        network_config: rustiplayer_config::NetworkConfig,
        demux_config: rustiplayer_config::PlayerDemuxConfig,
    ) -> Self {
        Self {
            adapter: Box::new(WebMediaOpenAdapter::Direct {
                locator,
                network_config,
                demux_config,
            }),
        }
    }

    /// Создаёт native HLS request с typed pre-Installed fallback intent.
    pub(crate) fn native_hls(
        source: NativeHlsUrl,
        intent: NativeHlsOpenIntent,
        settings: WebMediaOpenSettings,
    ) -> Self {
        Self {
            adapter: Box::new(WebMediaOpenAdapter::NativeHls {
                source,
                intent,
                settings,
            }),
        }
    }

    /// Создаёт extractor request с exact typed selection intent.
    pub(crate) fn extractor(
        locator: service_ytdlp::YtDlpMediaLocator,
        selection_intent: crate::web_media_open::YtDlpCandidateOpenIntent,
        settings: WebMediaOpenSettings,
    ) -> Self {
        Self {
            adapter: Box::new(WebMediaOpenAdapter::Extractor {
                locator,
                selection_intent,
                settings,
            }),
        }
    }

    /// Возвращает redacted safe label до любого I/O.
    pub(crate) fn safe_label(&self) -> SafeMediaLabel {
        match &*self.adapter {
            WebMediaOpenAdapter::Direct { locator, .. } => {
                SafeMediaLabel::from_service_safe_label(locator.safe_label())
            }
            WebMediaOpenAdapter::NativeHls { source, .. } => source.safe_label().clone(),
            WebMediaOpenAdapter::Extractor { locator, .. } => {
                SafeMediaLabel::from_service_safe_label(locator.safe_label())
            }
        }
    }

    /// Передаёт закрытый adapter payload neutral composition owner-у.
    pub(super) fn into_adapter(self) -> WebMediaOpenAdapterView {
        (*self.adapter).into()
    }
}

/// Общая descriptor envelope для всех web ingress-ов.
#[derive(Clone)]
pub(crate) struct PreparedWebMediaEnvelope {
    tracks: Vec<TrackInfo>,
    duration: Option<Duration>,
    metadata: MediaTagMetadata,
    source: WebMediaSourceIntent,
    safe_label: SafeMediaLabel,
    playback_window: Option<player_core::MediaPlaybackWindow>,
    vod_endpoint_recovery: Option<crate::web_media_vod_recovery::VodEndpointRecoveryAttachment>,
}

impl fmt::Debug for PreparedWebMediaEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedWebMediaEnvelope")
            .field("tracks", &self.tracks().len())
            .field("duration", &self.duration())
            .field("metadata", self.metadata())
            .field("source", self.source())
            .field("safe_label", self.safe_label())
            .field("playback_window", &self.playback_window)
            .field(
                "has_vod_endpoint_recovery",
                &self.vod_endpoint_recovery.is_some(),
            )
            .finish()
    }
}

impl PreparedWebMediaEnvelope {
    /// Собирает immutable Installed descriptor без demux/runtime ownership.
    pub(crate) fn new(
        tracks: Vec<TrackInfo>,
        duration: Option<Duration>,
        metadata: MediaTagMetadata,
        source: WebMediaSourceIntent,
        safe_label: SafeMediaLabel,
        playback_window: Option<player_core::MediaPlaybackWindow>,
        vod_endpoint_recovery: Option<crate::web_media_vod_recovery::VodEndpointRecoveryAttachment>,
    ) -> Self {
        Self {
            tracks,
            duration,
            metadata,
            source,
            safe_label,
            playback_window,
            vod_endpoint_recovery,
        }
    }

    pub(crate) fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    pub(crate) const fn duration(&self) -> Option<Duration> {
        self.duration
    }

    pub(crate) const fn metadata(&self) -> &MediaTagMetadata {
        &self.metadata
    }

    pub(crate) const fn source(&self) -> &WebMediaSourceIntent {
        &self.source
    }

    /// Возвращает reconstructible active source вместе с neutral window identity.
    pub(crate) fn active_source(&self) -> super::ActiveMediaSource {
        let source = super::ActiveMediaSource::Web(self.source.clone());
        match self.playback_window {
            Some(window) => source.with_playback_window(window),
            None => source,
        }
    }

    /// Добавляет либо заменяет playback window без изменения stable web intent.
    pub(crate) fn with_playback_window(
        mut self,
        playback_window: player_core::MediaPlaybackWindow,
    ) -> Self {
        self.playback_window = Some(playback_window);
        self
    }

    pub(crate) const fn safe_label(&self) -> &SafeMediaLabel {
        &self.safe_label
    }

    pub(crate) fn vod_endpoint_recovery(
        &self,
    ) -> Option<crate::web_media_vod_recovery::VodEndpointRecoveryAttachment> {
        self.vod_endpoint_recovery.clone()
    }
}

/// Named seek attachment сохраняет обычную и authoritative landing semantics.
pub(crate) enum PreparedWebMediaSeekAttachment {
    WorkerReceipted(Arc<dyn PreparedDemuxSeekPort>),
    AuthoritativePostTarget(Arc<dyn PreparedDemuxSeekPort>),
}

/// Runtime-only attachments, которые устанавливаются до strong barrier-а.
#[derive(Default)]
pub(crate) struct PreparedWebMediaAttachments {
    pub(crate) timeline_port: Option<DynamicMediaTimelinePort>,
    pub(crate) demux_seek: Option<PreparedWebMediaSeekAttachment>,
    pub(crate) playback_window: Option<player_core::MediaPlaybackWindow>,
    pub(crate) initial_position: Option<player_core::PreparedInitialPosition>,
}

/// Composition сохраняет различимые ошибки timeline mode и initial position.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PreparedWebMediaCompositionError {
    #[error(transparent)]
    TimelineMode(#[from] player_core::PreparedMediaTimelineModeError),
    #[error(transparent)]
    InitialPosition(#[from] player_core::PreparedInitialPositionError),
}

/// Собирает один player-facing `PreparedMedia` для любого web adapter-а.
pub(crate) fn compose_prepared_web_media(
    safe_label: &str,
    demuxer: Box<dyn Demuxer + Send>,
    attachments: PreparedWebMediaAttachments,
) -> Result<PreparedMedia, PreparedWebMediaCompositionError> {
    let mut prepared_media = PreparedMedia::from_external_label(safe_label, demuxer);
    if let Some(seek_attachment) = attachments.demux_seek {
        prepared_media = match seek_attachment {
            PreparedWebMediaSeekAttachment::WorkerReceipted(port) => {
                prepared_media.with_worker_receipted_demux_seek(port)
            }
            PreparedWebMediaSeekAttachment::AuthoritativePostTarget(port) => prepared_media
                .with_worker_receipted_demux_seek_policy(
                    port,
                    player_core::PreparedDemuxSeekLandingPolicy::AuthoritativePostTarget,
                ),
        };
    }
    if let Some(playback_window) = attachments.playback_window {
        prepared_media = prepared_media.with_playback_window(playback_window)?;
    }
    prepared_media = match attachments.timeline_port {
        Some(timeline_port) => prepared_media.with_dynamic_timeline(timeline_port),
        None => Ok(prepared_media),
    }?;
    match attachments.initial_position {
        Some(initial_position) => {
            Ok(prepared_media.with_prepared_initial_position(initial_position)?)
        }
        None => Ok(prepared_media),
    }
}

/// Внутренний adapter payload нужен только `media_open::preparation`.
pub(super) enum WebMediaOpenAdapterView {
    Direct {
        locator: service_direct_media::DirectMediaUrl,
        network_config: rustiplayer_config::NetworkConfig,
        demux_config: rustiplayer_config::PlayerDemuxConfig,
    },
    NativeHls {
        source: NativeHlsUrl,
        intent: NativeHlsOpenIntent,
        settings: WebMediaOpenSettings,
    },
    Extractor {
        locator: service_ytdlp::YtDlpMediaLocator,
        selection_intent: crate::web_media_open::YtDlpCandidateOpenIntent,
        settings: WebMediaOpenSettings,
    },
}

impl From<WebMediaOpenAdapter> for WebMediaOpenAdapterView {
    fn from(adapter: WebMediaOpenAdapter) -> Self {
        match adapter {
            WebMediaOpenAdapter::Direct {
                locator,
                network_config,
                demux_config,
            } => Self::Direct {
                locator,
                network_config,
                demux_config,
            },
            WebMediaOpenAdapter::NativeHls {
                source,
                intent,
                settings,
            } => Self::NativeHls {
                source,
                intent,
                settings,
            },
            WebMediaOpenAdapter::Extractor {
                locator,
                selection_intent,
                settings,
            } => Self::Extractor {
                locator,
                selection_intent,
                settings,
            },
        }
    }
}

#[cfg(test)]
mod tests;
