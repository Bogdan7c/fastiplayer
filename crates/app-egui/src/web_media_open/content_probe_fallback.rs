//! Bounded retry orchestration для planner-ranked ContentProbe failures.
//!
//! Retry разрешён только BestPlayable intent-у после typed runtime content
//! rejection либо одного typed `NetworkUnavailable` candidate-а.
//! Timeout, authentication, cancellation, parser и остальные provider failures
//! завершают open немедленно и не умножают wall-clock latency.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use rustiplayer_config::{NetworkConfig, YtDlpConfig};
use service_ytdlp::{
    YtDlpCandidateSelection, YtDlpCandidateSnapshot, YtDlpMediaLocator, YtDlpNormalizedCandidate,
};
use source_core::CancellationToken;
use web_media_core::{CandidateIdentity, SemanticIdentity};
use web_media_dash::DashEndpointRefreshPort;
use web_media_hls::HlsEndpointRefreshPort;
use web_media_playback_plan::{
    PlanningCandidateSnapshot, PlaybackCapabilitySnapshot, PlaybackSelectionPolicy,
    rank_playable_opaque_alternatives,
};
use web_media_transport_api::{TransportFailure, TransportOpenError};

use super::content_probe::ContentProbeRejection;
use super::{
    AdaptiveEndpointRefreshPorts, OpenedWebCandidate, WebCandidateOpenContext, WebOpenRuntime,
    catalog_capabilities::AppCatalogCapabilityProbe,
    component_variants::YtDlpComponentSelectionOpenIntent, preparation,
};

/// BestPlayable может обойти ровно один недоступный physical candidate.
///
/// Это не transport retry: следующая попытка использует другую planner identity.
/// Лимит не позволяет большим inventories последовательно умножать network latency.
const MAX_NETWORK_UNAVAILABLE_FALLBACKS: usize = 1;

/// Named зависимости открытия одного concrete candidate-а.
///
/// Контекст живёт только внутри одного immutable extraction snapshot-а. Retry
/// меняет candidate/selection, но не policy, cancellation generation или
/// provider registries.
pub(super) struct CandidateAttemptContext<'context, IsCancelled>
where
    IsCancelled: Fn() -> bool,
{
    pub(super) locator: &'context YtDlpMediaLocator,
    pub(super) network_config: &'context NetworkConfig,
    pub(super) yt_dlp_config: &'context YtDlpConfig,
    pub(super) candidate_snapshot: &'context YtDlpCandidateSnapshot,
    pub(super) runtime: &'context WebOpenRuntime,
    pub(super) component_selection_intent: &'context YtDlpComponentSelectionOpenIntent,
    pub(super) preferred_height: web_media_core::PreferredHeightPolicy,
    pub(super) cancellation: &'context CancellationToken,
    pub(super) is_cancelled: &'context IsCancelled,
    pub(super) playback_policy: &'context PlaybackSelectionPolicy,
    pub(super) catalog_capability_probe: &'context mut AppCatalogCapabilityProbe,
}

/// Успешная concrete попытка вместе с exact active selection.
pub(super) struct OpenedCandidateAttempt {
    candidate_selection: YtDlpCandidateSelection,
    opened_candidate: OpenedWebCandidate,
}

impl OpenedCandidateAttempt {
    /// Передаёт ownership orchestration layer-у после выбора успешной попытки.
    pub(super) fn into_parts(self) -> (YtDlpCandidateSelection, OpenedWebCandidate) {
        (self.candidate_selection, self.opened_candidate)
    }
}

impl<IsCancelled> CandidateAttemptContext<'_, IsCancelled>
where
    IsCancelled: Fn() -> bool,
{
    /// Собирает candidate-specific refresh/catalog state и открывает одну попытку.
    pub(super) fn open(
        &mut self,
        candidate: &YtDlpNormalizedCandidate,
        candidate_selection: YtDlpCandidateSelection,
    ) -> std::result::Result<OpenedCandidateAttempt, CandidateOpenError> {
        let catalog_identity = web_media_core::ComponentVariantCatalogIdentity::new(
            web_media_core::ExactSelectionIdentity::new(
                candidate_selection.exact_identity().clone(),
                candidate_selection.semantic_identity().clone(),
            )
            .context("YtDlp catalog parent identities нарушают source lineage")?,
            preparation::next_component_variant_catalog_generation()?,
        );
        let hls_endpoint_refresh: Option<Arc<dyn HlsEndpointRefreshPort>> =
            (self.candidate_snapshot.live_intent() == service_ytdlp::YtDlpLiveIntent::Live
                && crate::web_media_hls_open::candidate_is_hls(candidate))
            .then(|| {
                Arc::new(
                    crate::web_media_hls_refresh::AppHlsEndpointRefreshPort::new(
                        self.locator.clone(),
                        self.yt_dlp_config.clone(),
                        self.network_config.clone(),
                        self.runtime.source_config.clone(),
                        self.runtime.provider_id.clone(),
                        candidate_selection.clone(),
                        self.cancellation.clone(),
                    ),
                ) as Arc<dyn HlsEndpointRefreshPort>
            });
        let dash_endpoint_refresh: Option<Arc<dyn DashEndpointRefreshPort>> =
            (self.candidate_snapshot.live_intent() == service_ytdlp::YtDlpLiveIntent::Live
                && crate::web_media_dash_open::candidate_is_dash(candidate))
            .then(|| {
                Arc::new(
                    crate::web_media_dash_refresh::AppDashEndpointRefreshPort::new(
                        self.locator.clone(),
                        self.yt_dlp_config.clone(),
                        self.network_config.clone(),
                        self.runtime.source_config.clone(),
                        self.runtime.provider_id.clone(),
                        candidate_selection.clone(),
                        self.cancellation.clone(),
                    ),
                ) as Arc<dyn DashEndpointRefreshPort>
            });
        let opened_candidate = self.runtime.open_candidate(
            candidate,
            WebCandidateOpenContext {
                live_intent: self.candidate_snapshot.live_intent(),
                endpoint_refresh_ports: AdaptiveEndpointRefreshPorts {
                    hls: hls_endpoint_refresh,
                    dash: dash_endpoint_refresh,
                },
                timeline_port_generation: preparation::next_dynamic_timeline_port_generation()?,
                component_selection_intent: self.component_selection_intent.clone(),
                preferred_height: self.preferred_height,
                catalog_identity,
                cancellation: self.cancellation.clone(),
            },
            self.is_cancelled,
            self.catalog_capability_probe,
            self.playback_policy,
        )?;
        Ok(OpenedCandidateAttempt {
            candidate_selection,
            opened_candidate,
        })
    }
}

/// Typed граница между retryable content proof и terminal open failure.
#[derive(Debug)]
pub(super) enum CandidateOpenError {
    /// Candidate physical resource открылся, но actual tracks не прошли proof.
    ContentProbe(ContentProbeRejection),
    /// Concrete candidate не открылся без timeout-а; BestPlayable может попробовать один alternate.
    NetworkUnavailable(anyhow::Error),
    /// Любая ошибка вне content-proof contract-а остаётся terminal.
    Fatal(anyhow::Error),
}

impl CandidateOpenError {
    /// Возвращает исходную typed ошибку для single-attempt Exact/Composed path-а.
    pub(super) fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::ContentProbe(rejection) => anyhow::Error::new(rejection),
            Self::NetworkUnavailable(error) | Self::Fatal(error) => error,
        }
    }
}

impl From<anyhow::Error> for CandidateOpenError {
    fn from(error: anyhow::Error) -> Self {
        if matches!(
            error.downcast_ref::<TransportOpenError>(),
            Some(TransportOpenError::Transport(
                TransportFailure::NetworkUnavailable
            ))
        ) {
            Self::NetworkUnavailable(error)
        } else {
            Self::Fatal(error)
        }
    }
}

impl From<ContentProbeRejection> for CandidateOpenError {
    fn from(rejection: ContentProbeRejection) -> Self {
        Self::ContentProbe(rejection)
    }
}

/// Проецирует accepted service candidates в exact planner-owned best-first rank.
pub(super) fn ranked_best_playable_candidates<'candidate>(
    candidate_snapshot: &'candidate YtDlpCandidateSnapshot,
    planning_snapshot: &PlanningCandidateSnapshot,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    policy: &PlaybackSelectionPolicy,
) -> Result<Vec<&'candidate YtDlpNormalizedCandidate>> {
    candidate_snapshot
        .validate_planning_snapshot_alignment(planning_snapshot)
        .context("Service/planner candidate snapshots не соответствуют друг другу")?;
    let ranking = rank_playable_opaque_alternatives(planning_snapshot, capabilities, policy)
        .context("Не удалось получить planner-owned BestPlayable ranking")?;
    map_ranked_service_candidates(candidate_snapshot, ranking.ranked_candidate_identities())
}

/// Сопоставляет полный planner rank с canonical service candidates по identity.
fn map_ranked_service_candidates<'candidate, 'ranked_identity>(
    candidate_snapshot: &'candidate YtDlpCandidateSnapshot,
    ranked_identities: impl ExactSizeIterator<
        Item = (
            &'ranked_identity CandidateIdentity,
            &'ranked_identity SemanticIdentity,
        ),
    >,
) -> Result<Vec<&'candidate YtDlpNormalizedCandidate>> {
    let mut canonical_candidates = BTreeMap::new();
    for candidate in candidate_snapshot.accepted_candidates() {
        let identity = (
            candidate.descriptor().identity().clone(),
            candidate.descriptor().semantic_identity().clone(),
        );
        if canonical_candidates.insert(identity, candidate).is_some() {
            bail!("Service snapshot содержит duplicate canonical candidate identity");
        }
    }

    let mut ranked_candidates = Vec::with_capacity(ranked_identities.len());
    for (exact_identity, semantic_identity) in ranked_identities {
        let identity = (exact_identity.clone(), semantic_identity.clone());
        let candidate = canonical_candidates.remove(&identity).ok_or_else(|| {
            anyhow::anyhow!("Planner ranking нельзя полностью сопоставить service snapshot-у")
        })?;
        ranked_candidates.push(candidate);
    }
    if ranked_candidates.is_empty() {
        bail!("Planner ranking не содержит BestPlayable candidate");
    }
    Ok(ranked_candidates)
}

/// Открывает BestPlayable candidates по bounded planner rank до первого успеха.
pub(super) fn open_ranked_best<'candidate, Candidate, Opened>(
    candidates: impl IntoIterator<Item = &'candidate Candidate>,
    is_cancelled: &impl Fn() -> bool,
    mut open: impl FnMut(&'candidate Candidate) -> std::result::Result<Opened, CandidateOpenError>,
) -> Result<(&'candidate Candidate, Opened)> {
    let mut rejection_count = 0_usize;
    let mut network_unavailable_count = 0_usize;
    let mut last_retryable_error = None;
    for candidate in candidates {
        if is_cancelled() {
            bail!("YtDlp candidate fallback отменён до следующей попытки");
        }
        match open(candidate) {
            Ok(opened) => return Ok((candidate, opened)),
            Err(CandidateOpenError::ContentProbe(rejection)) => {
                rejection_count = rejection_count.saturating_add(1);
                last_retryable_error = Some(CandidateOpenError::ContentProbe(rejection));
            }
            Err(CandidateOpenError::NetworkUnavailable(error)) => {
                network_unavailable_count = network_unavailable_count.saturating_add(1);
                if network_unavailable_count > MAX_NETWORK_UNAVAILABLE_FALLBACKS {
                    return Err(error.context(format!(
                        "BestPlayable network fallback исчерпан (content_rejections={rejection_count}, unavailable_candidates={network_unavailable_count})"
                    )));
                }
                last_retryable_error = Some(CandidateOpenError::NetworkUnavailable(error));
            }
            Err(CandidateOpenError::Fatal(error)) => return Err(error),
        }
    }

    let exhausted_context = format!(
        "Planner-ranked BestPlayable candidates исчерпаны (content_rejections={rejection_count}, unavailable_candidates={network_unavailable_count})"
    );
    match last_retryable_error {
        Some(CandidateOpenError::ContentProbe(rejection)) => {
            Err(anyhow::Error::new(rejection).context(exhausted_context))
        }
        Some(CandidateOpenError::NetworkUnavailable(error)) => {
            Err(error.context(exhausted_context))
        }
        // Fatal не сохраняется для fallback-а, но match остаётся total при
        // дальнейшем расширении CandidateOpenError.
        Some(CandidateOpenError::Fatal(error)) => Err(error),
        None => bail!("Planner ranking не предоставил candidate для открытия"),
    }
}

/// Выполняет ровно одну Exact/Composed попытку без скрытого fallback-а.
pub(super) fn open_single<Candidate, Opened>(
    candidate: &Candidate,
    open: impl FnOnce(&Candidate) -> std::result::Result<Opened, CandidateOpenError>,
) -> Result<Opened> {
    open(candidate).map_err(CandidateOpenError::into_anyhow)
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    use audio::AudioDecodeCodecFamily;
    use capability_core::{
        BackendCapabilities, BackendDriverInfo, BackendProbeStatus,
        CURRENT_CAPABILITY_SCHEMA_VERSION, SupportedVideoOutput, SystemCapabilities,
    };
    use codec_core::{
        BitDepth, ChromaSubsampling, DecodeBackendId, H264Profile, SupportedVideoDecodeFormat,
        VideoCodec as DecodeVideoCodec, VideoProfile,
    };
    use tempfile::TempDir;
    use video_frame_contract::VideoFrameContract;
    use web_media_core::{
        CandidateFormatIdentity, ExactSelectionIdentity, ExtractionGeneration, SelectionRequest,
        SourceIdentity,
    };

    use super::*;

    /// Изолирует fake `yt-dlp` PATH от параллельных tests текущего process-а.
    const FALLBACK_CHILD_MARKER_ENV: &str = "RUSTIPLAYER_FALLBACK_CHILD";
    /// Передаёт synthetic extractor document только isolated child process-у.
    const FALLBACK_DOCUMENT_ENV: &str = "RUSTIPLAYER_FALLBACK_YTDLP_JSON";
    /// Exact test name исключает повторный запуск всего app test binary.
    const FALLBACK_CHILD_TEST_NAME: &str = "web_media_open::content_probe_fallback::tests::service_snapshot_ranking_keeps_successful_selection_and_exact_is_single_attempt";
    /// Проверяет fallback со selected-only лучшего audio на inventory audio.
    const CATALOG_AUDIO_CHILD_TEST_NAME: &str = "web_media_open::content_probe_fallback::tests::catalog_composition_uses_best_composable_audio";
    /// Проверяет отсутствие fake composition у selected-only video.
    const CATALOG_VIDEO_CHILD_TEST_NAME: &str = "web_media_open::content_probe_fallback::tests::catalog_selected_only_video_keeps_parent_choices_without_composed_target";
    /// Проверяет обычную inventory video+audio composition.
    const CATALOG_INVENTORY_CHILD_TEST_NAME: &str = "web_media_open::content_probe_fallback::tests::catalog_inventory_video_audio_composition_remains_available";

    #[test]
    fn best_playable_retries_only_typed_content_rejection() {
        let candidates = [10_u8, 20_u8];
        let mut attempts = Vec::new();
        let (selected, opened) = open_ranked_best(&candidates, &|| false, |candidate| {
            attempts.push(*candidate);
            if *candidate == 10 {
                Err(ContentProbeRejection::UnsupportedAudio.into())
            } else {
                Ok("opened")
            }
        })
        .expect("second planner-ranked candidate должен открыться");

        assert_eq!(*selected, 20);
        assert_eq!(opened, "opened");
        assert_eq!(attempts, [10, 20]);
    }

    #[test]
    fn best_playable_uses_one_alternate_after_network_unavailable() {
        let candidates = [10_u8, 20_u8];
        let mut attempts = Vec::new();
        let (selected, opened) = open_ranked_best(&candidates, &|| false, |candidate| {
            attempts.push(*candidate);
            if *candidate == 10 {
                Err(CandidateOpenError::from(
                    anyhow::Error::new(TransportOpenError::Transport(
                        TransportFailure::NetworkUnavailable,
                    ))
                    .context("provider добавил безопасный пользовательский контекст"),
                ))
            } else {
                Ok("opened")
            }
        })
        .expect("второй planner-ranked candidate должен открыть тот же BestPlayable intent");

        assert_eq!(*selected, 20);
        assert_eq!(opened, "opened");
        assert_eq!(attempts, [10, 20]);
    }

    #[test]
    fn best_playable_network_fallback_is_bounded_to_one_alternate() {
        let candidates = [10_u8, 20_u8, 30_u8];
        let mut attempts = Vec::new();
        let error = open_ranked_best(&candidates, &|| false, |candidate| {
            attempts.push(*candidate);
            Err::<(), _>(CandidateOpenError::from(anyhow::Error::new(
                TransportOpenError::Transport(TransportFailure::NetworkUnavailable),
            )))
        })
        .expect_err("две недоступные identities должны исчерпать bounded fallback");

        assert_eq!(attempts, [10, 20]);
        assert!(error.to_string().contains("network fallback исчерпан"));
    }

    #[test]
    fn best_playable_timeout_remains_terminal_without_alternate_attempt() {
        let candidates = [10_u8, 20_u8];
        let mut attempts = 0_usize;
        let error = open_ranked_best(&candidates, &|| false, |_| {
            attempts = attempts.saturating_add(1);
            Err::<(), _>(CandidateOpenError::from(anyhow::Error::new(
                TransportOpenError::Transport(TransportFailure::Timeout),
            )))
        })
        .expect_err("timeout не должен умножаться на размер candidate inventory");

        assert_eq!(attempts, 1);
        assert!(matches!(
            error.downcast_ref::<TransportOpenError>(),
            Some(TransportOpenError::Transport(TransportFailure::Timeout))
        ));
    }

    #[test]
    fn best_playable_exhaustion_preserves_most_recent_retryable_error() {
        let candidates = [10_u8, 20_u8];
        let error = open_ranked_best(&candidates, &|| false, |candidate| {
            if *candidate == 10 {
                Err::<(), _>(ContentProbeRejection::UnsupportedVideo.into())
            } else {
                Err::<(), _>(CandidateOpenError::from(anyhow::Error::new(
                    TransportOpenError::Transport(TransportFailure::NetworkUnavailable),
                )))
            }
        })
        .expect_err("exhaustion должен сохранить последнюю фактическую причину");

        assert!(matches!(
            error.downcast_ref::<TransportOpenError>(),
            Some(TransportOpenError::Transport(
                TransportFailure::NetworkUnavailable
            ))
        ));
        assert!(error.to_string().contains("content_rejections=1"));
        assert!(error.to_string().contains("unavailable_candidates=1"));
    }

    #[test]
    fn exact_content_rejection_does_not_try_another_candidate() {
        let mut attempts = 0_usize;
        let error = open_single(&10_u8, |_| {
            attempts += 1;
            Err::<(), _>(ContentProbeRejection::UnsupportedVideo.into())
        })
        .expect_err("Exact content rejection должен остаться terminal");

        assert_eq!(attempts, 1);
        assert_eq!(
            error.downcast_ref::<ContentProbeRejection>(),
            Some(&ContentProbeRejection::UnsupportedVideo)
        );
    }

    #[test]
    fn fatal_best_playable_failure_is_not_masked_by_neighbor() {
        let candidates = [10_u8, 20_u8];
        let mut attempts = 0_usize;
        let error = open_ranked_best(&candidates, &|| false, |_| {
            attempts += 1;
            Err::<(), _>(CandidateOpenError::Fatal(anyhow::anyhow!(
                "terminal provider failure"
            )))
        })
        .expect_err("terminal failure не должен запускать fallback");

        assert_eq!(attempts, 1);
        assert!(error.to_string().contains("terminal provider failure"));
    }

    /// Реальный service snapshot доказывает mapping planner rank → exact active selection.
    #[test]
    fn service_snapshot_ranking_keeps_successful_selection_and_exact_is_single_attempt() {
        if env::var_os(FALLBACK_CHILD_MARKER_ENV).is_some() {
            assert_child_service_snapshot_fallback();
            return;
        }

        run_isolated_service_snapshot_child(
            FALLBACK_CHILD_TEST_NAME,
            fallback_candidate_document(),
        );
    }

    /// Selected-only лучший audio пропускается ради следующего composable audio.
    #[test]
    fn catalog_composition_uses_best_composable_audio() {
        if env::var_os(FALLBACK_CHILD_MARKER_ENV).is_some() {
            assert_child_catalog_choice_count(4);
            return;
        }
        run_isolated_service_snapshot_child(
            CATALOG_AUDIO_CHILD_TEST_NAME,
            selected_audio_catalog_document(),
        );
    }

    /// Selected-only video остаётся parent choice, но не создаёт fake A/V target.
    #[test]
    fn catalog_selected_only_video_keeps_parent_choices_without_composed_target() {
        if env::var_os(FALLBACK_CHILD_MARKER_ENV).is_some() {
            assert_child_catalog_choice_count(2);
            return;
        }
        run_isolated_service_snapshot_child(
            CATALOG_VIDEO_CHILD_TEST_NAME,
            selected_video_catalog_document(),
        );
    }

    /// Обычная inventory video+audio пара по-прежнему создаёт composed choice.
    #[test]
    fn catalog_inventory_video_audio_composition_remains_available() {
        if env::var_os(FALLBACK_CHILD_MARKER_ENV).is_some() {
            assert_child_catalog_choice_count(3);
            return;
        }
        run_isolated_service_snapshot_child(
            CATALOG_INVENTORY_CHILD_TEST_NAME,
            inventory_av_catalog_document(),
        );
    }

    /// Запускает один production-normalization assertion в isolated child process-е.
    fn run_isolated_service_snapshot_child(test_name: &str, document: &str) {
        let fake_tools = TempDir::new().expect("create fallback fake-tools directory");
        install_fake_yt_dlp(fake_tools.path());
        let output = Command::new(env::current_exe().expect("current app-egui test binary"))
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .env(FALLBACK_CHILD_MARKER_ENV, "1")
            .env(FALLBACK_DOCUMENT_ENV, document)
            .env("PATH", path_with_fake_tools_first(fake_tools.path()))
            .output()
            .expect("spawn isolated service snapshot test child");

        assert!(
            output.status.success(),
            "service snapshot child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Child строит реальный service snapshot и считает full catalog projection.
    fn assert_child_catalog_choice_count(expected_choice_count: usize) {
        let locator =
            service_ytdlp::parse_yt_dlp_media_locator("https://page.example.test/catalog")
                .expect("parse synthetic catalog locator");
        let yt_dlp_config = YtDlpConfig::default();
        let snapshot = service_ytdlp::resolve_yt_dlp_candidate_snapshot_with_config(
            &locator,
            SourceIdentity::new(92),
            ExtractionGeneration::new(1),
            &yt_dlp_config,
        )
        .expect("resolve real catalog service snapshot");
        let planning = snapshot
            .planning_snapshot()
            .expect("map catalog snapshot to planner");
        let runtime = super::super::WebOpenRuntime::new(
            &NetworkConfig::default(),
            &rustiplayer_config::PlayerDemuxConfig::default(),
        )
        .expect("create catalog runtime capability registries");
        let system_capabilities = h264_test_system_capabilities();
        let audio_capabilities = audio::AudioDecodeCapabilitySnapshot::empty()
            .with_available_family(AudioDecodeCodecFamily::Opus);
        let capabilities = PlaybackCapabilitySnapshot::new(
            &runtime.transport_capabilities,
            &runtime.demux_capabilities,
            &system_capabilities,
            audio_capabilities,
        );
        let policy =
            super::super::selection_policy(&yt_dlp_config, &[rustiplayer_config::VideoCodec::H264])
                .expect("create catalog playback policy");
        let active_candidate = snapshot
            .accepted_candidates()
            .next()
            .expect("catalog fixture has an accepted candidate");
        let active_selection = snapshot
            .selection_for(active_candidate)
            .expect("catalog active selection");

        let choice_count = super::super::catalog::projected_parent_choice_count(
            super::super::catalog::CatalogAttachmentRequest {
                candidate_snapshot: &snapshot,
                planning_snapshot: &planning,
                capabilities,
                policy: &policy,
                active_selection: &active_selection,
                active_composed: None,
            },
        )
        .expect("project real service catalog choices");
        assert_eq!(choice_count, expected_choice_count);
    }

    /// Capability report содержит один software-compatible H.264 output.
    fn h264_test_system_capabilities() -> SystemCapabilities {
        let backend_id =
            DecodeBackendId::new("catalog_h").expect("valid catalog fixture backend ID");
        let output = SupportedVideoOutput {
            backend: backend_id.clone(),
            decode_format: SupportedVideoDecodeFormat {
                codec: DecodeVideoCodec::H264,
                profile: VideoProfile::H264(H264Profile::Baseline),
                bit_depth: BitDepth::Eight,
                chroma: ChromaSubsampling::Yuv420,
                max_width: Some(3840),
                max_height: Some(2160),
                max_fps: Some(60.0),
                hdr_input: false,
            },
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
        };
        SystemCapabilities {
            schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
            probed_at_unix_seconds: 1,
            video_backends: vec![BackendCapabilities {
                backend_id,
                display_name: "Catalog fixture H.264 backend".to_owned(),
                status: BackendProbeStatus::Available,
                driver: BackendDriverInfo::default(),
                raw_supported_outputs: vec![output.clone()],
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                diagnostics: Vec::new(),
            }],
            render_backends: Vec::new(),
            playable_video_outputs: vec![output],
        }
    }

    /// Child использует production extractor normalization и planner snapshot.
    fn assert_child_service_snapshot_fallback() {
        let locator =
            service_ytdlp::parse_yt_dlp_media_locator("https://page.example.test/runtime-fallback")
                .expect("parse synthetic page locator");
        let yt_dlp_config = YtDlpConfig::default();
        let snapshot = service_ytdlp::resolve_yt_dlp_candidate_snapshot_with_config(
            &locator,
            SourceIdentity::new(91),
            ExtractionGeneration::new(1),
            &yt_dlp_config,
        )
        .expect("resolve real service candidate snapshot");
        let planning = snapshot
            .planning_snapshot()
            .expect("map real service snapshot to planner");
        let runtime = super::super::WebOpenRuntime::new(
            &NetworkConfig::default(),
            &rustiplayer_config::PlayerDemuxConfig::default(),
        )
        .expect("create app runtime capability registries");
        let system_capabilities = capability_core::SystemCapabilities::empty(1);
        let audio_capabilities = audio::AudioDecodeCapabilitySnapshot::empty();
        let capabilities = PlaybackCapabilitySnapshot::new(
            &runtime.transport_capabilities,
            &runtime.demux_capabilities,
            &system_capabilities,
            audio_capabilities,
        );
        let policy =
            super::super::selection_policy(&yt_dlp_config, &[rustiplayer_config::VideoCodec::Vp9])
                .expect("create app playback policy");
        let ranked = ranked_best_playable_candidates(&snapshot, &planning, capabilities, &policy)
            .expect("rank real service candidates");
        assert_eq!(
            ranked.len(),
            2,
            "fixture должна дать две playable alternatives"
        );
        let first_descriptor = ranked[0].descriptor();
        let duplicate_rank = [
            (
                first_descriptor.identity(),
                first_descriptor.semantic_identity(),
            ),
            (
                first_descriptor.identity(),
                first_descriptor.semantic_identity(),
            ),
        ];
        assert!(
            map_ranked_service_candidates(&snapshot, duplicate_rank.into_iter()).is_err(),
            "duplicate planner identity должна fail-closed"
        );
        let missing_exact_identity = CandidateIdentity::new(
            snapshot.source(),
            snapshot.generation(),
            CandidateFormatIdentity::new("missing-planner-candidate")
                .expect("bounded missing test identity"),
        );
        let missing_rank = [(
            &missing_exact_identity,
            first_descriptor.semantic_identity(),
        )];
        assert!(
            map_ranked_service_candidates(&snapshot, missing_rank.into_iter()).is_err(),
            "missing planner identity должна fail-closed"
        );
        let first_selection = snapshot
            .selection_for(ranked[0])
            .expect("first ranked candidate selection");
        let expected_second_selection = snapshot
            .selection_for(ranked[1])
            .expect("second ranked candidate selection");
        let incomplete_planning = PlanningCandidateSnapshot::new(
            planning.source(),
            planning.generation(),
            vec![planning.candidates()[0].clone()],
        )
        .expect("bounded incomplete planning fixture");
        assert_eq!(
            crate::web_media_stream_model::WebMediaStreamConfiguration::from_yt_dlp_snapshot(
                &snapshot,
                &incomplete_planning,
                capabilities,
                &policy,
                &first_selection,
                crate::web_media_stream_model::WebMediaSelectionPreference::GlobalBestPlayable,
            )
            .expect_err("sidebar должен отвергнуть mispaired planning snapshot"),
            crate::web_media_stream_model::WebMediaStreamModelBuildError::CandidateSnapshotAlignmentFailed
        );
        assert!(
            super::super::catalog::projected_parent_choice_count(
                super::super::catalog::CatalogAttachmentRequest {
                    candidate_snapshot: &snapshot,
                    planning_snapshot: &incomplete_planning,
                    capabilities,
                    policy: &policy,
                    active_selection: &first_selection,
                    active_composed: None,
                },
            )
            .is_err(),
            "catalog должен отвергнуть mispaired planning snapshot"
        );

        let stream_configuration =
            crate::web_media_stream_model::WebMediaStreamConfiguration::from_yt_dlp_snapshot(
                &snapshot,
                &planning,
                capabilities,
                &policy,
                &first_selection,
                crate::web_media_stream_model::WebMediaSelectionPreference::GlobalBestPlayable,
            )
            .expect("canonical real snapshot должен построить URL stream model");
        assert_eq!(
            stream_configuration.candidates().len(),
            2,
            "selected + formats duplicate не должен создавать третью URL option"
        );

        let catalog_choice_count = super::super::catalog::projected_parent_choice_count(
            super::super::catalog::CatalogAttachmentRequest {
                candidate_snapshot: &snapshot,
                planning_snapshot: &planning,
                capabilities,
                policy: &policy,
                active_selection: &first_selection,
                active_composed: None,
            },
        )
        .expect("canonical real snapshot должен построить URL catalog");
        assert_eq!(
            catalog_choice_count, 2,
            "selected + formats duplicate не должен создавать третью catalog choice"
        );

        let mut best_attempts = 0_usize;
        let (_, active_selection) =
            open_ranked_best(ranked.iter().copied(), &|| false, |candidate| {
                best_attempts = best_attempts.saturating_add(1);
                let selection = snapshot
                    .selection_for(candidate)
                    .expect("ranked candidate selection");
                if best_attempts == 1 {
                    Err(ContentProbeRejection::UnsupportedAudio.into())
                } else {
                    Ok(selection)
                }
            })
            .expect("second real planner-ranked candidate should succeed");
        assert_eq!(best_attempts, 2);
        assert_eq!(active_selection, expected_second_selection);

        let exact_identity = ExactSelectionIdentity::new(
            first_selection.exact_identity().clone(),
            first_selection.semantic_identity().clone(),
        )
        .expect("same-snapshot exact identity");
        let exact_plan = web_media_playback_plan::plan_playback(
            &planning,
            capabilities,
            &SelectionRequest::Exact(exact_identity),
            &policy,
        )
        .expect("plan same-snapshot exact candidate");
        let exact_candidate = snapshot
            .accepted_candidates()
            .find(|candidate| {
                candidate.descriptor().identity() == exact_plan.selected().exact_identity()
            })
            .expect("map exact plan to real service candidate");
        let mut exact_attempts = 0_usize;
        let exact_error = open_single(exact_candidate, |_| {
            exact_attempts = exact_attempts.saturating_add(1);
            Err::<(), _>(ContentProbeRejection::UnsupportedAudio.into())
        })
        .expect_err("Exact content rejection must remain terminal");
        assert_eq!(exact_attempts, 1);
        assert_eq!(
            exact_error.downcast_ref::<ContentProbeRejection>(),
            Some(&ContentProbeRejection::UnsupportedAudio)
        );
    }

    /// Устанавливает process-compatible fake `yt-dlp` только в child PATH.
    fn install_fake_yt_dlp(fake_tools_directory: &Path) {
        let executable_path = fake_tools_directory.join("yt-dlp");
        let script = concat!(
            "#!/bin/sh\n",
            "set -eu\n",
            "printf '%s\\n' \"${RUSTIPLAYER_FALLBACK_YTDLP_JSON:?missing fixture JSON}\"\n",
        );
        fs::write(&executable_path, script).expect("write fallback fake yt-dlp");
        let mut permissions = fs::metadata(&executable_path)
            .expect("read fallback fake yt-dlp metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable_path, permissions)
            .expect("mark fallback fake yt-dlp executable");
    }

    /// Строит child-only PATH без глобального mutation параллельного test process-а.
    fn path_with_fake_tools_first(fake_tools_directory: &Path) -> OsString {
        let inherited_path = env::var_os("PATH").unwrap_or_default();
        env::join_paths(
            std::iter::once(fake_tools_directory.to_path_buf())
                .chain(env::split_paths(&inherited_path)),
        )
        .expect("join fallback child PATH")
    }

    /// Реальная форма повторяет selected candidate внутри `formats[]` inventory.
    fn fallback_candidate_document() -> &'static str {
        r#"{"id":"runtime-fallback","title":"Runtime fallback","format_id":"higher-quality","url":"https://media.example.test/higher.ogg","protocol":"https","ext":"ogg","container":"ogg","vcodec":null,"acodec":null,"quality":10,"formats":[{"format_id":"higher-quality","url":"https://media.example.test/higher.ogg","protocol":"https","ext":"ogg","container":"ogg","vcodec":null,"acodec":null,"quality":10},{"format_id":"lower-quality","url":"https://media.example.test/lower.ogg","protocol":"https","ext":"ogg","container":"ogg","vcodec":null,"acodec":null,"quality":1}]}"#
    }

    /// Selected-only audio выше inventory audio, но composition обязана выбрать inventory.
    fn selected_audio_catalog_document() -> &'static str {
        r#"{"id":"selected-audio-catalog","title":"Selected audio catalog","format_id":"selected-audio","url":"https://media.example.test/selected.opus","protocol":"https","ext":"opus","container":"ogg","vcodec":"none","acodec":"opus","quality":100,"abr":192,"formats":[{"format_id":"inventory-video","url":"https://media.example.test/video.mp4","protocol":"https","ext":"mp4","container":"mp4","vcodec":"avc1.42001E","acodec":"none","width":1280,"height":720,"fps":30,"dynamic_range":"SDR","quality":5},{"format_id":"inventory-audio","url":"https://media.example.test/audio.opus","protocol":"https","ext":"opus","container":"ogg","vcodec":"none","acodec":"opus","quality":1,"abr":96}]}"#
    }

    /// Selected-only video остаётся самостоятельным parent-ом без inventory composition.
    fn selected_video_catalog_document() -> &'static str {
        r#"{"id":"selected-video-catalog","title":"Selected video catalog","format_id":"selected-video","url":"https://media.example.test/selected.mp4","protocol":"https","ext":"mp4","container":"mp4","vcodec":"avc1.42001E","acodec":"none","width":1280,"height":720,"fps":30,"dynamic_range":"SDR","quality":100,"formats":[{"format_id":"inventory-audio","url":"https://media.example.test/audio.opus","protocol":"https","ext":"opus","container":"ogg","vcodec":"none","acodec":"opus","quality":1,"abr":96}]}"#
    }

    /// Обычные inventory video и audio образуют один дополнительный composed target.
    fn inventory_av_catalog_document() -> &'static str {
        r#"{"id":"inventory-av-catalog","title":"Inventory A/V catalog","formats":[{"format_id":"inventory-video","url":"https://media.example.test/video.mp4","protocol":"https","ext":"mp4","container":"mp4","vcodec":"avc1.42001E","acodec":"none","width":1280,"height":720,"fps":30,"dynamic_range":"SDR","quality":5},{"format_id":"inventory-audio","url":"https://media.example.test/audio.opus","protocol":"https","ext":"opus","container":"ogg","vcodec":"none","acodec":"opus","quality":1,"abr":96}]}"#
    }
}
