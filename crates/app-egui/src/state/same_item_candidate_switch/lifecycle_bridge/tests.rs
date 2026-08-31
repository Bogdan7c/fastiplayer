//! Functional composition starts after safe UI action resolution.
//!
//! `web_media_catalog::tests` and `web_media_stream_model::component_variants_tests` закрепляют
//! action-to-target resolution; здесь ровно production bridge проверяет resolved request ->
//! pending -> strong terminal -> app-owned effects без WGPU.

use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::time::Duration;

use player_core::{MediaInstanceId, PlaybackIntent, PlaybackState, PlayerSnapshot};
use playlist_core::PlaylistItemId;

use super::*;

/// Exact player observation остаётся owner-ом fake strong lifecycle, а не app path-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlaybackObservation {
    media_instance_id: MediaInstanceId,
    position: Duration,
    state: PlaybackState,
}

/// Deterministic terminal script позволяет проверить pending и pre-barrier rollback отдельно.
enum FakePollStep {
    Pending,
    Installed {
        media_instance_id: MediaInstanceId,
        generation: WebMediaStreamGeneration,
    },
    PreBarrierFailure,
}

/// Fake реализует те же два узких port-а и app-owned completion boundary, что production context.
struct FakeSameItemSwitchContext {
    request_id: MediaOpenRequestId,
    poll_steps: VecDeque<FakePollStep>,
    begin_expected_active: Option<ActiveMediaIdentity>,
    begin_playback_intent: Option<PlaybackIntent>,
    controller: crate::web_media_stream_model::UrlSidebarController,
    item_id: PlaylistItemId,
    source_lineage: u64,
    visible_generation: WebMediaStreamGeneration,
    playback: PlaybackObservation,
    remembered_targets: Vec<(
        PlaylistItemId,
        crate::web_media_catalog::WebMediaSelectionTarget,
    )>,
    terminal_selector_errors: Vec<(WebMediaStreamGeneration, UrlSidebarSafeError)>,
    fallback_notice_visible: bool,
}

impl SameItemSwitchLifecycleStartPort for FakeSameItemSwitchContext {
    fn begin_same_lineage(
        &mut self,
        source_request: MediaOpenSourceRequest,
        expected_active: ActiveMediaIdentity,
        playback_intent: PlaybackIntent,
    ) -> Result<MediaOpenRequestId, StrongMediaOpenError> {
        assert!(
            matches!(source_request, MediaOpenSourceRequest::Web(_)),
            "URL action обязан передать lifecycle port-у готовый neutral web reopen request"
        );
        self.begin_expected_active = Some(expected_active);
        self.begin_playback_intent = Some(playback_intent);
        Ok(self.request_id)
    }
}

impl SameItemSwitchLifecyclePollPort for FakeSameItemSwitchContext {
    fn poll_same_lineage(&mut self, request_id: MediaOpenRequestId) -> SameItemSwitchLifecyclePoll {
        if request_id != self.request_id {
            return SameItemSwitchLifecyclePoll::StaleRequest;
        }
        match self
            .poll_steps
            .pop_front()
            .expect("fixture poll script должен содержать следующий step")
        {
            FakePollStep::Pending => SameItemSwitchLifecyclePoll::Pending,
            FakePollStep::Installed {
                media_instance_id,
                generation,
            } => {
                self.playback.media_instance_id = media_instance_id;
                self.visible_generation = generation;
                SameItemSwitchLifecyclePoll::Installed(InstalledSameItemSwitchEvidence {
                    generation: Some(generation),
                    component_catalog_installed: true,
                })
            }
            FakePollStep::PreBarrierFailure => {
                SameItemSwitchLifecyclePoll::Failed(StrongMediaOpenError::Terminal(
                    crate::media_open::MediaOpenTerminalOutcome::PreparationFailed {
                        request_id,
                        safe_label: crate::media_open::SafeMediaLabel::from_service_safe_label(
                            "fixture.invalid",
                        ),
                        kind: crate::media_open::MediaPreparationFailureKind::ExtractorOpen,
                    },
                ))
            }
        }
    }
}

impl SameItemSwitchCompletionOwner for FakeSameItemSwitchContext {
    fn visible_generation(
        &self,
        _previous_generation: WebMediaStreamGeneration,
    ) -> WebMediaStreamGeneration {
        self.visible_generation
    }

    fn remember_picker_target(
        &mut self,
        item_id: PlaylistItemId,
        target: crate::web_media_catalog::WebMediaSelectionTarget,
    ) {
        self.remembered_targets.push((item_id, target));
    }

    fn clear_web_media_fallback_notice(&mut self) {
        self.fallback_notice_visible = false;
    }
}

impl SameItemSwitchSelectorOwner for FakeSameItemSwitchContext {
    fn record_switch_started(
        &mut self,
        pending: UrlSidebarPendingSelection,
    ) -> Result<(), UrlSidebarTransitionError> {
        self.controller.record_switch_started(pending)
    }

    fn record_switch_failed(
        &mut self,
        pending: &UrlSidebarPendingSelection,
        generation: WebMediaStreamGeneration,
        error: UrlSidebarSafeError,
    ) {
        let _cleared = self
            .controller
            .record_switch_failed(pending, generation, error);
    }

    fn record_switch_terminal_failed(
        &mut self,
        pending: &UrlSidebarPendingSelection,
        generation: WebMediaStreamGeneration,
        error: UrlSidebarSafeError,
    ) {
        self.terminal_selector_errors.push((generation, error));
        let _restored = self
            .controller
            .record_switch_terminal_failed(pending, generation, error);
    }

    fn record_candidate_switch_installed(
        &mut self,
        generation: WebMediaStreamGeneration,
        item_id: Option<PlaylistItemId>,
        preferred_height: Option<u32>,
    ) {
        self.controller
            .record_candidate_switch_installed(generation, item_id, preferred_height);
    }

    fn record_component_switch_installed(&mut self) {
        self.controller.record_component_switch_installed();
    }
}

#[test]
fn playing_resolved_url_action_keeps_position_and_commits_only_after_installed() {
    assert_successful_url_action(PlaybackState::Playing, PlaybackIntent::StartPlaying);
}

#[test]
fn paused_resolved_url_action_keeps_position_and_commits_only_after_installed() {
    assert_successful_url_action(PlaybackState::Paused, PlaybackIntent::StartPaused);
}

/// Оба stable states проходят один production app path, но с разным exact intent.
fn assert_successful_url_action(state: PlaybackState, expected_intent: PlaybackIntent) {
    let item_id = playlist_item_id(41);
    let old_instance = media_instance_id(51);
    let new_instance = media_instance_id(52);
    let old_generation = WebMediaStreamGeneration::for_test(61, 1);
    let new_generation = WebMediaStreamGeneration::for_test(61, 2);
    let exact_position = Duration::from_millis(37_250);
    let old_playback = PlaybackObservation {
        media_instance_id: old_instance,
        position: exact_position,
        state,
    };
    let mut context = FakeSameItemSwitchContext {
        request_id: media_open_request_id(71),
        poll_steps: VecDeque::from([
            FakePollStep::Pending,
            FakePollStep::Installed {
                media_instance_id: new_instance,
                generation: new_generation,
            },
        ]),
        begin_expected_active: None,
        begin_playback_intent: None,
        controller: crate::web_media_stream_model::UrlSidebarController::default(),
        item_id,
        source_lineage: 61,
        visible_generation: old_generation,
        playback: old_playback,
        remembered_targets: Vec::new(),
        terminal_selector_errors: Vec::new(),
        fallback_notice_visible: true,
    };
    let mut path = test_app_path();

    assert_eq!(
        path.start(
            app_start(item_id, old_instance, old_generation, state),
            &mut context,
        )
        .expect("URL action start должен пройти fake strong boundary"),
        UrlSidebarActionApplyOutcome::Started
    );
    assert!(path.pending.is_some(), "action обязан стать pending");
    assert_eq!(context.playback, old_playback);
    assert!(context.remembered_targets.is_empty());
    assert_eq!(context.begin_playback_intent, Some(expected_intent));
    let expected_active = context
        .begin_expected_active
        .expect("strong begin получает exact active identity");
    assert_eq!(expected_active.item_id(), Some(item_id));
    assert_eq!(expected_active.media_instance_id(), old_instance);

    assert_eq!(path.poll(&mut context), SameItemSwitchAppPoll::Pending);
    assert!(path.pending.is_some());
    assert_eq!(context.playback, old_playback);
    assert!(context.remembered_targets.is_empty());
    assert!(context.fallback_notice_visible);

    assert_eq!(path.poll(&mut context), SameItemSwitchAppPoll::Installed);
    assert!(path.pending.is_none());
    assert_eq!(context.item_id, item_id, "playlist item не меняется");
    assert_eq!(context.source_lineage, 61, "source lineage не меняется");
    assert_ne!(context.playback.media_instance_id, old_instance);
    assert_eq!(context.playback.media_instance_id, new_instance);
    assert_eq!(context.visible_generation, new_generation);
    assert_eq!(context.playback.position, exact_position);
    assert_eq!(context.playback.state, state);
    assert!(context.terminal_selector_errors.is_empty());
    assert_eq!(
        context.remembered_targets,
        vec![(
            item_id,
            crate::web_media_catalog::WebMediaSelectionTarget::Fixture(1_080),
        )]
    );
    assert!(!context.fallback_notice_visible);
}

#[test]
fn resolved_url_action_pre_barrier_failure_preserves_playback_and_restores_selector() {
    let item_id = playlist_item_id(81);
    let old_instance = media_instance_id(82);
    let old_generation = WebMediaStreamGeneration::for_test(83, 1);
    let old_playback = PlaybackObservation {
        media_instance_id: old_instance,
        position: Duration::from_secs(19),
        state: PlaybackState::Playing,
    };
    let previous_preference = (
        item_id,
        crate::web_media_catalog::WebMediaSelectionTarget::Fixture(720),
    );
    let mut context = FakeSameItemSwitchContext {
        request_id: media_open_request_id(84),
        poll_steps: VecDeque::from([FakePollStep::Pending, FakePollStep::PreBarrierFailure]),
        begin_expected_active: None,
        begin_playback_intent: None,
        controller: crate::web_media_stream_model::UrlSidebarController::default(),
        item_id,
        source_lineage: 83,
        visible_generation: old_generation,
        playback: old_playback,
        remembered_targets: vec![previous_preference.clone()],
        terminal_selector_errors: Vec::new(),
        fallback_notice_visible: true,
    };
    let mut path = test_app_path();

    path.start(
        app_start(
            item_id,
            old_instance,
            old_generation,
            PlaybackState::Playing,
        ),
        &mut context,
    )
    .expect("URL action должен перейти в pending");
    assert_eq!(path.poll(&mut context), SameItemSwitchAppPoll::Pending);
    assert_eq!(
        path.poll(&mut context),
        SameItemSwitchAppPoll::Failed(UrlSidebarSafeError::SourceUnavailable)
    );

    assert!(
        path.pending.is_none(),
        "terminal failure снимает pending selector"
    );
    assert_eq!(context.item_id, item_id);
    assert_eq!(context.source_lineage, 83);
    assert_eq!(context.visible_generation, old_generation);
    assert_eq!(context.playback, old_playback);
    assert_eq!(context.remembered_targets, vec![previous_preference]);
    assert_eq!(
        context.terminal_selector_errors,
        vec![(old_generation, UrlSidebarSafeError::SourceUnavailable)]
    );
    assert!(context.fallback_notice_visible);

    context.poll_steps = VecDeque::from([FakePollStep::Pending]);
    path.start(
        app_start(
            item_id,
            old_instance,
            old_generation,
            PlaybackState::Playing,
        ),
        &mut context,
    )
    .expect("restored selector должен принимать следующий action");
    assert!(path.pending.is_some());
}

#[test]
fn pending_switch_blocks_conflicting_action_without_touching_playback() {
    let item_id = playlist_item_id(91);
    let old_instance = media_instance_id(92);
    let generation = WebMediaStreamGeneration::for_test(93, 1);
    let playback = PlaybackObservation {
        media_instance_id: old_instance,
        position: Duration::from_secs(23),
        state: PlaybackState::Paused,
    };
    let mut context = FakeSameItemSwitchContext {
        request_id: media_open_request_id(94),
        poll_steps: VecDeque::from([FakePollStep::Pending]),
        begin_expected_active: None,
        begin_playback_intent: None,
        controller: crate::web_media_stream_model::UrlSidebarController::default(),
        item_id,
        source_lineage: 93,
        visible_generation: generation,
        playback,
        remembered_targets: Vec::new(),
        terminal_selector_errors: Vec::new(),
        fallback_notice_visible: true,
    };
    let mut path = test_app_path();

    path.start(
        app_start(item_id, old_instance, generation, PlaybackState::Paused),
        &mut context,
    )
    .expect("первый action должен занять single-flight slot");
    let first_request_id = path
        .pending
        .as_ref()
        .expect("первый switch остаётся pending")
        .request_id;

    assert!(matches!(
        path.start(
            app_start(item_id, old_instance, generation, PlaybackState::Paused),
            &mut context,
        ),
        Err(SameItemSwitchError::Busy)
    ));
    assert_eq!(
        path.pending
            .as_ref()
            .expect("conflicting action не снимает первый pending")
            .request_id,
        first_request_id
    );
    assert_eq!(context.playback, playback);
    assert!(context.remembered_targets.is_empty());
}

/// Создаёт path с теми же controller/pending owners, которые временно извлекает AppState.
fn test_app_path() -> SameItemSwitchAppPath {
    SameItemSwitchAppPath { pending: None }
}

/// Строит resolved picker action и использует production playback-intent mapping.
fn app_start(
    item_id: PlaylistItemId,
    media_instance_id: MediaInstanceId,
    parent_generation: WebMediaStreamGeneration,
    state: PlaybackState,
) -> SameItemSwitchAppStart {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.media_instance_id = Some(media_instance_id);
    snapshot.playback_state = state;
    snapshot.set_timeline_position(media_core::MediaTime::from_duration(Duration::from_millis(
        37_250,
    )));
    let locator = crate::direct_progressive_open::classify_direct_media_url(
        "https://media.example.test/same-item-switch.mp4",
    )
    .expect("direct URL fixture locator валиден");
    SameItemSwitchAppStart {
        source_request: MediaOpenSourceRequest::Web(
            crate::media_open::WebMediaOpenRequest::direct(
                locator,
                rustiplayer_config::NetworkConfig::default(),
                rustiplayer_config::PlayerDemuxConfig::default(),
            ),
        ),
        expected_active: ActiveMediaIdentity::for_same_item_switch_test(item_id, media_instance_id),
        playback_intent: super::super::super::playback_intent_from_snapshot(&snapshot),
        kind: SameItemSwitchKind::Picker {
            parent_generation,
            action: crate::web_media_catalog::WebMediaFacetAction::resolution_for_test(91, 1),
            target: crate::web_media_catalog::WebMediaSelectionTarget::Fixture(1_080),
        },
    }
}

fn playlist_item_id(value: u64) -> PlaylistItemId {
    PlaylistItemId::from_persistence_value(value).expect("item fixture ID is non-zero")
}

fn media_instance_id(value: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(NonZeroU64::new(value).expect("media fixture ID is non-zero"))
}

fn media_open_request_id(value: u64) -> MediaOpenRequestId {
    MediaOpenRequestId::from_non_zero(
        NonZeroU64::new(value).expect("request fixture ID is non-zero"),
    )
}
