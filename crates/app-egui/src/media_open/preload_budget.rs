//! Resource projection для bounded next-item source/demux preparation.

use super::MediaOpenSourceRequest;

/// Валидированный общий RAM/read-ahead budget одного speculative queue item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueuePreloadResourceBudget {
    total_mebibytes: u64,
}

impl QueuePreloadResourceBudget {
    /// Config validation гарантирует место минимум для двух component windows.
    pub(crate) fn from_validated_config(total_mebibytes: u64) -> Self {
        debug_assert!(total_mebibytes >= 2);
        Self { total_mebibytes }
    }

    /// Один direct source может использовать весь общий budget.
    const fn direct_component_mebibytes(self) -> u64 {
        self.total_mebibytes
    }

    /// YtDlp может открыть separate A/V, поэтому каждый component получает половину.
    const fn ytdlp_component_mebibytes(self) -> u64 {
        self.total_mebibytes / 2
    }
}

impl MediaOpenSourceRequest {
    /// Проецирует active-playback request в bounded speculative resource policy.
    pub(crate) fn with_queue_preload_budget(self, budget: QueuePreloadResourceBudget) -> Self {
        match self {
            request @ Self::Local { .. } => request,
            Self::Web(request) => {
                let request = match request.into_adapter() {
                    super::web::WebMediaOpenAdapterView::Direct {
                        locator,
                        mut network_config,
                        demux_config,
                    } => {
                        limit_network_resources(
                            &mut network_config,
                            budget.direct_component_mebibytes(),
                        );
                        super::WebMediaOpenRequest::direct(locator, network_config, demux_config)
                    }
                    super::web::WebMediaOpenAdapterView::NativeHls {
                        source,
                        intent,
                        mut settings,
                    } => {
                        limit_network_resources(
                            &mut settings.network_config,
                            budget.ytdlp_component_mebibytes(),
                        );
                        super::WebMediaOpenRequest::native_hls(source, intent, settings)
                    }
                    super::web::WebMediaOpenAdapterView::Extractor {
                        locator,
                        selection_intent,
                        mut settings,
                    } => {
                        limit_network_resources(
                            &mut settings.network_config,
                            budget.ytdlp_component_mebibytes(),
                        );
                        super::WebMediaOpenRequest::extractor(locator, selection_intent, settings)
                    }
                };
                Self::Web(request)
            }
            Self::PlaybackWindow {
                source,
                semantic_identity,
            } => Self::PlaybackWindow {
                source: Box::new((*source).with_queue_preload_budget(budget)),
                semantic_identity,
            },
        }
    }
}

/// Сохраняет invariant `initial <= chunk <= window` после уменьшения window.
fn limit_network_resources(
    network_config: &mut rustiplayer_config::NetworkConfig,
    component_budget_mebibytes: u64,
) {
    let component_budget_mebibytes = component_budget_mebibytes.max(1);
    network_config.memory_cache_mb = network_config
        .memory_cache_mb
        .min(component_budget_mebibytes);
    network_config.read_ahead_mb = network_config.read_ahead_mb.min(component_budget_mebibytes);
    network_config.prefetch_chunk_mb = network_config
        .prefetch_chunk_mb
        .min(network_config.read_ahead_mb)
        .max(1);
    network_config.prefetch_initial_chunk_kb = network_config
        .prefetch_initial_chunk_kb
        .min(network_config.prefetch_chunk_mb.saturating_mul(1_024))
        .max(1);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::media_open::WebMediaOpenRequest;
    use crate::media_open::web::WebMediaOpenAdapterView;

    #[test]
    fn caps_nested_network_request_without_changing_identity() {
        let direct_locator = crate::direct_progressive_open::classify_direct_media_url(
            "https://example.com/video.mp4?token=budget-secret",
        )
        .expect("direct locator parsed");
        let semantic_identity = player_core::MediaPlaybackWindow::new(
            media_core::MediaTime::from_secs(5),
            Some(media_core::MediaTime::from_secs(20)),
        )
        .expect("valid playback window");
        let network_config = rustiplayer_config::NetworkConfig {
            read_ahead_mb: 256,
            prefetch_chunk_mb: 128,
            prefetch_initial_chunk_kb: 96 * 1_024,
            ..rustiplayer_config::NetworkConfig::default()
        };
        let request = MediaOpenSourceRequest::PlaybackWindow {
            source: Box::new(MediaOpenSourceRequest::Web(WebMediaOpenRequest::direct(
                direct_locator.clone(),
                network_config,
                rustiplayer_config::PlayerDemuxConfig::default(),
            ))),
            semantic_identity,
        };

        let projected = request
            .with_queue_preload_budget(QueuePreloadResourceBudget::from_validated_config(64));

        let MediaOpenSourceRequest::PlaybackWindow {
            source,
            semantic_identity: projected_identity,
        } = projected
        else {
            panic!("projection must preserve playback-window boundary");
        };
        let MediaOpenSourceRequest::Web(web_request) = *source else {
            panic!("projection must preserve neutral web boundary");
        };
        let WebMediaOpenAdapterView::Direct {
            locator,
            network_config,
            ..
        } = web_request.into_adapter()
        else {
            panic!("projection must preserve direct source kind");
        };
        assert_eq!(projected_identity, semantic_identity);
        assert_eq!(locator.safe_label(), direct_locator.safe_label());
        assert_eq!(network_config.memory_cache_mb, 64);
        assert_eq!(network_config.read_ahead_mb, 64);
        assert_eq!(network_config.prefetch_chunk_mb, 64);
        assert_eq!(network_config.prefetch_initial_chunk_kb, 64 * 1_024);
    }

    #[test]
    fn does_not_inflate_small_or_local_requests() {
        let small_network_config = rustiplayer_config::NetworkConfig {
            read_ahead_mb: 24,
            prefetch_chunk_mb: 8,
            prefetch_initial_chunk_kb: 512,
            ..rustiplayer_config::NetworkConfig::default()
        };
        let direct_locator = crate::direct_progressive_open::classify_direct_media_url(
            "https://example.com/already-small.mp4",
        )
        .expect("direct locator parsed");
        let projected_direct = MediaOpenSourceRequest::Web(WebMediaOpenRequest::direct(
            direct_locator,
            small_network_config,
            rustiplayer_config::PlayerDemuxConfig::default(),
        ))
        .with_queue_preload_budget(QueuePreloadResourceBudget::from_validated_config(64));
        let projected_local = MediaOpenSourceRequest::Local {
            path: PathBuf::from("fixture.flac"),
            expected_fingerprint: None,
            demux_config: rustiplayer_config::PlayerDemuxConfig::default(),
        }
        .with_queue_preload_budget(QueuePreloadResourceBudget::from_validated_config(64));

        let MediaOpenSourceRequest::Web(web_request) = projected_direct else {
            panic!("neutral web request boundary is stable");
        };
        let WebMediaOpenAdapterView::Direct { network_config, .. } = web_request.into_adapter()
        else {
            panic!("direct adapter kind is stable inside neutral web request");
        };
        assert_eq!(network_config.memory_cache_mb, 64);
        assert_eq!(network_config.read_ahead_mb, 24);
        assert_eq!(network_config.prefetch_chunk_mb, 8);
        assert_eq!(network_config.prefetch_initial_chunk_kb, 512);
        assert!(matches!(
            projected_local,
            MediaOpenSourceRequest::Local { path, .. } if path == *"fixture.flac"
        ));
    }
}
