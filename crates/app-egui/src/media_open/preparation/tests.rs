use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;

use media_core::{
    DemuxSeekResult, Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial,
    DynamicMediaTimelinePortGeneration, DynamicMediaTimelineState, MediaTagMetadata,
};
use player_core::PreparedMediaTimelineMode;

use super::{
    classify_yt_dlp_preparation_failure, merge_yt_dlp_playlist_metadata,
    service_duration_for_timeline,
};
use crate::media_open::{
    MediaPreparationFailureKind, PreparedWebMediaAttachments, PreparedWebMediaSeekAttachment,
    compose_prepared_web_media,
};
use crate::web_media_open::ComponentVariantFinalizationError;

/// Fake demuxer моделирует provider readiness без привязки к VOD/live режиму.
#[derive(Default)]
struct UnavailableFakeDemuxer;

impl Demuxer for UnavailableFakeDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
        Ok(media_core::DemuxReadEvent::TemporarilyUnavailable(
            media_core::DemuxRetryHint::new(Duration::from_millis(1)).expect("test retry hint"),
        ))
    }

    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        panic!("fake live demux seek is outside preparation test")
    }
}

/// Fake port нужен только для проверки ownership внутри общего S41 boundary.
struct FakePreparedDemuxSeekPort;

impl player_core::PreparedDemuxSeekPort for FakePreparedDemuxSeekPort {
    /// Integration helper не выполняет реальный seek во время preparation.
    fn enqueue_seek(
        &self,
        _request_id: player_core::PreparedDemuxSeekRequestId,
        _request: media_core::DemuxSeekRequest,
    ) -> Result<(), player_core::PreparedDemuxSeekEnqueueError> {
        // Вызов означал бы, что preparation незаконно исполняет post-install lifecycle.
        panic!("prepare boundary не должен выполнять demux seek")
    }

    /// До player install fake receipt отсутствует.
    fn poll_seek_receipt(&self) -> Option<player_core::PreparedDemuxSeekReceipt> {
        // `None` сохраняет nonblocking semantics production port-а.
        None
    }
}

#[test]
fn yt_dlp_service_metadata_fills_missing_demux_values() {
    let (duration, metadata) = merge_yt_dlp_playlist_metadata(
        None,
        MediaTagMetadata::default(),
        Some("Настоящее YtDlp название"),
        Some(Duration::from_secs(90)),
    );

    assert_eq!(duration, Some(Duration::from_secs(90)));
    assert_eq!(metadata.title.as_deref(), Some("Настоящее YtDlp название"));
}

#[test]
fn demux_metadata_remains_primary_when_already_known() {
    let demux_metadata = MediaTagMetadata {
        title: Some("Название из контейнера".to_string()),
        artists: vec!["Автор".to_string()],
        ..MediaTagMetadata::default()
    };
    let (duration, metadata) = merge_yt_dlp_playlist_metadata(
        Some(Duration::from_secs(91)),
        demux_metadata,
        Some("Название YtDlp"),
        Some(Duration::from_secs(90)),
    );

    assert_eq!(duration, Some(Duration::from_secs(91)));
    assert_eq!(metadata.title.as_deref(), Some("Название из контейнера"));
    assert_eq!(metadata.artists, ["Автор".to_string()]);
}

#[test]
fn typed_component_failure_survives_anyhow_context_without_string_parsing() {
    let typed_failures = [
        ComponentVariantFinalizationError::ComponentCatalogUnavailable,
        ComponentVariantFinalizationError::SemanticRematch(
            web_media_core::ComponentVariantError::LayoutMismatch,
        ),
        ComponentVariantFinalizationError::Installation(
            crate::web_media_stream_model::component_variants::ComponentVariantInstallationError::ActiveParentMismatch,
        ),
    ];

    for typed_failure in typed_failures {
        let error = anyhow::Error::new(typed_failure).context("наружный YtDlp preparation context");
        assert_eq!(
            classify_yt_dlp_preparation_failure(&error),
            MediaPreparationFailureKind::ComponentCatalogUnavailable
        );
    }
}

#[test]
fn typed_dash_profile_and_schema_failures_survive_anyhow_context() {
    let profile_error = anyhow::Error::new(dash_mpd_core::DashDynamicMpdError::ProfileExcluded(
        dash_mpd_core::DashDynamicProfileExclusion::UnsupportedDeclaredProfile,
    ))
    .context("наружный DASH live preparation context");
    assert_eq!(
        classify_yt_dlp_preparation_failure(&profile_error),
        MediaPreparationFailureKind::DashLiveProfileExcluded
    );

    let schema_error = dash_mpd_core::parse_dynamic_dash_mpd(dash_mpd_core::DashMpdParseRequest {
        document_bytes: b"<NotMpd/>",
        xml_budgets: bounded_xml_reader::XmlBudgets::builder()
            .maximum_document_bytes(1_024)
            .maximum_depth(8)
            .maximum_tokens(32)
            .maximum_attributes_per_element(8)
            .maximum_attribute_count(16)
            .maximum_attribute_bytes(512)
            .maximum_namespace_declarations_per_element(4)
            .maximum_namespace_declaration_count(8)
            .maximum_namespace_bytes(256)
            .maximum_text_bytes(512)
            .build()
            .expect("test XML budgets"),
        limits: dash_mpd_core::DashMpdLimits {
            maximum_periods: 1,
            maximum_adaptation_sets_per_period: 1,
            maximum_representations_per_adaptation_set: 1,
            maximum_segments_per_list: 1,
            maximum_timeline_entries: 1,
            maximum_schema_string_bytes: 256,
        },
    })
    .expect_err("invalid root должен дать schema error");
    let schema_error =
        anyhow::Error::new(schema_error).context("наружный DASH live preparation context");
    assert_eq!(
        classify_yt_dlp_preparation_failure(&schema_error),
        MediaPreparationFailureKind::DashLiveSchemaRejected
    );
}

#[test]
fn unrelated_ytdlp_failure_keeps_generic_classification() {
    let error = anyhow::anyhow!("обычная provider ошибка");

    assert_eq!(
        classify_yt_dlp_preparation_failure(&error),
        MediaPreparationFailureKind::ExtractorOpen
    );
}

/// Cancelled neutral request обязан завершиться до adapter I/O и не менять error semantics.
#[test]
fn cancelled_web_request_stops_before_adapter_dispatch() {
    let cancellation = super::super::executor::PreparationCancellation::new();
    cancellation.cancel(player_core::MediaInstallCancellationCause::UserCancelled);
    let locator = crate::direct_progressive_open::classify_direct_media_url(
        "https://unreachable.example.test/cancelled-before-io.mp4",
    )
    .expect("direct fixture locator валиден");
    let request = crate::media_open::MediaOpenSourceRequest::Web(
        crate::media_open::WebMediaOpenRequest::direct(
            locator,
            rustiplayer_config::NetworkConfig::default(),
            rustiplayer_config::PlayerDemuxConfig::default(),
        ),
    );

    let result = super::prepare_source(request, &cancellation);

    assert!(matches!(
        result,
        Err(MediaPreparationFailureKind::Cancelled)
    ));
}

#[test]
fn live_timeline_is_installed_before_barrier_and_service_duration_stays_unknown() {
    let (timeline_port, _publisher) =
        media_core::dynamic_media_timeline(DynamicMediaTimelineInitial {
            port_generation: DynamicMediaTimelinePortGeneration::new(
                NonZeroU64::new(1).expect("non-zero test generation"),
            ),
            source_epoch: DynamicMediaTimelineEpoch::new(0),
            state: DynamicMediaTimelineState::without_dvr(Duration::from_secs(30).into()),
        });
    assert_eq!(
        service_duration_for_timeline(Some(&timeline_port), Some(Duration::from_secs(3_600))),
        None
    );
    let prepared = compose_prepared_web_media(
        "live",
        Box::new(UnavailableFakeDemuxer),
        PreparedWebMediaAttachments {
            timeline_port: Some(timeline_port),
            ..PreparedWebMediaAttachments::default()
        },
    )
    .expect("live timeline attaches before barrier");
    assert_eq!(prepared.duration(), None);
    assert!(matches!(
        prepared.timeline_mode(),
        PreparedMediaTimelineMode::Live { .. }
    ));
}

/// Static providers получают seek/window attachments через тот же intent-boundary.
#[test]
fn static_seek_and_playback_window_share_one_pre_barrier_prepared_media_path() {
    // Fake port остаётся снаружи, чтобы проверить передачу ownership без вызова I/O.
    let seek_port = Arc::new(FakePreparedDemuxSeekPort);
    // Trait-object clone имитирует concrete DASH/Smooth/HDS port.
    let erased_seek_port: Arc<dyn player_core::PreparedDemuxSeekPort> = seek_port.clone();
    // Ненулевой absolute origin моделирует HDS presentation window.
    let playback_window = player_core::MediaPlaybackWindow::new(
        Duration::from_secs(5).into(),
        Some(Duration::from_secs(12).into()),
    )
    .expect("static test window валидно");
    // Общий helper прикрепляет все static intents до strong-install barrier-а.
    let prepared = compose_prepared_web_media(
        "static segmented",
        Box::new(UnavailableFakeDemuxer),
        PreparedWebMediaAttachments {
            demux_seek: Some(PreparedWebMediaSeekAttachment::WorkerReceipted(
                erased_seek_port,
            )),
            playback_window: Some(playback_window),
            ..PreparedWebMediaAttachments::default()
        },
    )
    .expect("static attachments совместимы");
    // Static source не должен случайно получить live timeline mode.
    assert!(matches!(
        prepared.timeline_mode(),
        PreparedMediaTimelineMode::Static { .. }
    ));
    // Window проходит в player без provider-specific timestamp rewriting.
    assert_eq!(prepared.playback_window(), Some(playback_window));
    // Один Arc остаётся у test owner-а, второй — внутри PreparedMedia.
    assert_eq!(Arc::strong_count(&seek_port), 2);
}

/// Static playback window и dynamic live mode остаются взаимно исключающимися.
#[test]
fn live_timeline_and_static_window_conflict_fails_before_strong_install_barrier() {
    // Dynamic port моделирует единственные Implemented live rows HLS/DASH.
    let (timeline_port, _publisher) =
        media_core::dynamic_media_timeline(DynamicMediaTimelineInitial {
            port_generation: DynamicMediaTimelinePortGeneration::new(
                NonZeroU64::new(2).expect("non-zero test generation"),
            ),
            source_epoch: DynamicMediaTimelineEpoch::new(0),
            state: DynamicMediaTimelineState::without_dvr(Duration::from_secs(30).into()),
        });
    // Static window моделирует VOD-only HDS semantics.
    let playback_window = player_core::MediaPlaybackWindow::new(
        Duration::from_secs(5).into(),
        Some(Duration::from_secs(12).into()),
    )
    .expect("static test window валидно");
    // Конфликт обязан terminal-resolve как recoverable preparation error.
    let result = compose_prepared_web_media(
        "invalid mixed timeline",
        Box::new(UnavailableFakeDemuxer),
        PreparedWebMediaAttachments {
            timeline_port: Some(timeline_port),
            playback_window: Some(playback_window),
            ..PreparedWebMediaAttachments::default()
        },
    );
    // Никакой mixed provider state не достигает Ready/authorize phase.
    assert!(result.is_err());
}
