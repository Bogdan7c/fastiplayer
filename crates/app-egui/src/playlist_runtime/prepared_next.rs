//! Bounded owner speculative source/demux preparation следующего queue item.

use std::time::{Duration, Instant};

use player_core::{MediaInstallCancellationCause, PlaybackState, PlayerSnapshot};
use rustiplayer_config::PlaylistConfig;

use crate::app_wake::AppWakePort;
use crate::media_open::{
    MediaOpenSourceRequest, MediaOpenStartError, PreparedMediaOpen, QueuePreloadResourceBudget,
    SafeMediaLabel, SpeculativeMediaPreparation, SpeculativeMediaPreparationPoll,
};
use crate::process_shutdown::{ProcessOwnerShutdownOutcome, ShutdownDeadline};

use super::controller::PlaylistInstallMutation;
use super::{PlannedPlaylistInstall, PlaylistRuntime, PlaylistRuntimeBinding, QueuePreloadTarget};

/// Exact key не позволяет late result-у получить authority над другой queue lineage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreparedNextKey {
    target: QueuePreloadTarget,
}

impl From<QueuePreloadTarget> for PreparedNextKey {
    fn from(target: QueuePreloadTarget) -> Self {
        Self { target }
    }
}

/// Immutable policy snapshot владельца speculative resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreparedNextPolicy {
    enabled: bool,
    resource_budget: QueuePreloadResourceBudget,
    lead_time: Duration,
    max_hold: Duration,
}

impl PreparedNextPolicy {
    fn from_validated_config(config: PlaylistConfig) -> Self {
        Self {
            enabled: config.next_item_preload_enabled,
            resource_budget: QueuePreloadResourceBudget::from_validated_config(
                config.next_item_preload_budget_mb,
            ),
            lead_time: Duration::from_millis(config.next_item_preload_lead_time_ms),
            max_hold: Duration::from_millis(config.next_item_preload_max_hold_ms),
        }
    }

    /// Unknown-duration/live и paused media не создают долгоживущий speculative source.
    fn scheduling_window_is_open(self, snapshot: &PlayerSnapshot) -> bool {
        if !self.enabled || snapshot.playback_state != PlaybackState::Playing {
            return false;
        }
        let Some(duration) = snapshot.duration else {
            return false;
        };
        duration.saturating_sub(snapshot.current_position) <= self.lead_time
    }
}

/// Owner state хранит не больше одного exact target/result.
enum PreparedNextState {
    Idle,
    Preparing {
        key: PreparedNextKey,
    },
    Ready {
        key: PreparedNextKey,
        prepared_at: Instant,
        prepared_open: Box<PreparedMediaOpen>,
        safe_label: SafeMediaLabel,
    },
    Failed {
        key: PreparedNextKey,
    },
}

/// Ready envelope передаётся authoritative strong-open ровно один раз.
pub(crate) struct PreparedNextMedia {
    pub(crate) prepared_open: PreparedMediaOpen,
    pub(crate) safe_label: SafeMediaLabel,
}

/// Process-lifetime owner cancellation, expiration и resource projection.
pub(crate) struct PreparedNextOwner {
    policy: PreparedNextPolicy,
    preparation: SpeculativeMediaPreparation,
    state: PreparedNextState,
}

impl PreparedNextOwner {
    pub(crate) fn new(wake_port: AppWakePort, config: PlaylistConfig) -> Self {
        Self {
            policy: PreparedNextPolicy::from_validated_config(config),
            preparation: SpeculativeMediaPreparation::new(wake_port),
            state: PreparedNextState::Idle,
        }
    }

    /// Settings change инвалидирует старый budget/freshness snapshot до новой работы.
    pub(crate) fn reconfigure(&mut self, config: PlaylistConfig) {
        let requested = PreparedNextPolicy::from_validated_config(config);
        if requested != self.policy {
            self.cancel(MediaInstallCancellationCause::StructuralInvalidation);
            self.policy = requested;
        }
    }

    /// Не фиксирует shuffle/queue plan раньше lead window; existing state всегда revalidate-ится.
    pub(crate) fn needs_target_reconciliation(&mut self, snapshot: &PlayerSnapshot) -> bool {
        if !self.policy.enabled {
            self.cancel(MediaInstallCancellationCause::StructuralInvalidation);
            return false;
        }
        !matches!(&self.state, PreparedNextState::Idle)
            || self.policy.scheduling_window_is_open(snapshot)
    }

    /// Возвращает true только когда caller должен materialize и submit новый request.
    pub(crate) fn should_start(
        &mut self,
        target: QueuePreloadTarget,
        snapshot: &PlayerSnapshot,
        now: Instant,
    ) -> bool {
        self.refresh(now);
        if snapshot.media_instance_id != Some(target.active.media_instance_id()) {
            self.cancel(MediaInstallCancellationCause::StructuralInvalidation);
            return false;
        }
        let key = PreparedNextKey::from(target);
        match &self.state {
            PreparedNextState::Idle => self.policy.scheduling_window_is_open(snapshot),
            PreparedNextState::Preparing { key: current }
            | PreparedNextState::Ready { key: current, .. }
            | PreparedNextState::Failed { key: current }
                if *current == key =>
            {
                false
            }
            PreparedNextState::Preparing { .. }
            | PreparedNextState::Ready { .. }
            | PreparedNextState::Failed { .. } => {
                self.cancel(MediaInstallCancellationCause::StructuralInvalidation);
                self.policy.scheduling_window_is_open(snapshot)
            }
        }
    }

    /// Запускает bounded request с уменьшенным speculative read-ahead.
    pub(crate) fn start(
        &mut self,
        target: QueuePreloadTarget,
        source_request: MediaOpenSourceRequest,
    ) -> Result<(), MediaOpenStartError> {
        let source_request = source_request.with_queue_preload_budget(self.policy.resource_budget);
        self.preparation.start(source_request)?;
        self.state = PreparedNextState::Preparing {
            key: PreparedNextKey::from(target),
        };
        Ok(())
    }

    /// Source-boundary rejection отмечается только для этого key и не повторяется каждый frame.
    pub(crate) fn mark_failed(&mut self, target: QueuePreloadTarget) {
        self.preparation
            .cancel(MediaInstallCancellationCause::StructuralInvalidation);
        self.state = PreparedNextState::Failed {
            key: PreparedNextKey::from(target),
        };
    }

    /// Забирает matching fresh result; preparing/stale result уступает обычному cold-open.
    pub(crate) fn take_ready(
        &mut self,
        target: QueuePreloadTarget,
        now: Instant,
    ) -> Option<PreparedNextMedia> {
        self.refresh(now);
        let key = PreparedNextKey::from(target);
        let state = std::mem::replace(&mut self.state, PreparedNextState::Idle);
        match state {
            PreparedNextState::Ready {
                key: ready_key,
                prepared_at,
                prepared_open,
                safe_label,
            } if ready_key == key
                && now.saturating_duration_since(prepared_at) <= self.policy.max_hold =>
            {
                Some(PreparedNextMedia {
                    prepared_open: *prepared_open,
                    safe_label,
                })
            }
            PreparedNextState::Preparing { .. } => {
                self.preparation
                    .cancel(MediaInstallCancellationCause::Superseded);
                None
            }
            PreparedNextState::Idle
            | PreparedNextState::Ready { .. }
            | PreparedNextState::Failed { .. } => None,
        }
    }

    /// Любой authoritative open имеет приоритет и освобождает speculative resources.
    pub(crate) fn cancel(&mut self, cause: MediaInstallCancellationCause) {
        self.preparation.cancel(cause);
        self.state = PreparedNextState::Idle;
    }

    pub(crate) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> ProcessOwnerShutdownOutcome {
        self.state = PreparedNextState::Idle;
        self.preparation.shutdown_until(deadline)
    }

    /// Переносит worker result в owner state и удаляет просроченный envelope.
    fn refresh(&mut self, now: Instant) {
        if matches!(
            &self.state,
            PreparedNextState::Ready { prepared_at, .. }
                if now.saturating_duration_since(*prepared_at) > self.policy.max_hold
        ) {
            self.state = PreparedNextState::Idle;
        }
        let key = match &self.state {
            PreparedNextState::Preparing { key } => *key,
            PreparedNextState::Idle
            | PreparedNextState::Ready { .. }
            | PreparedNextState::Failed { .. } => return,
        };
        match self.preparation.poll() {
            SpeculativeMediaPreparationPoll::Idle
            | SpeculativeMediaPreparationPoll::InvariantLost => {
                self.state = PreparedNextState::Failed { key };
            }
            SpeculativeMediaPreparationPoll::Failed(failure) => {
                tracing::debug!(
                    ?failure,
                    "Next-item speculative preparation завершилась ошибкой"
                );
                self.state = PreparedNextState::Failed { key };
            }
            SpeculativeMediaPreparationPoll::Preparing => {}
            SpeculativeMediaPreparationPoll::Ready {
                prepared_open,
                safe_label,
            } => {
                self.state = PreparedNextState::Ready {
                    key,
                    prepared_at: now,
                    prepared_open,
                    safe_label,
                };
            }
        }
    }
}

impl PlaylistRuntime {
    /// Poll-ит owner и возвращает target только когда нужен новый source request.
    pub(crate) fn poll_next_item_preload_target(
        &mut self,
        binding: PlaylistRuntimeBinding,
        snapshot: &PlayerSnapshot,
    ) -> Option<QueuePreloadTarget> {
        if self.validate_binding(binding).is_err() {
            self.prepared_next
                .cancel(MediaInstallCancellationCause::StructuralInvalidation);
            return None;
        }
        if !self.prepared_next.needs_target_reconciliation(snapshot) {
            return None;
        }
        let Some(controller) = self.controller.as_mut() else {
            self.prepared_next
                .cancel(MediaInstallCancellationCause::StructuralInvalidation);
            return None;
        };
        let Some(target) = controller.next_item_preload_target() else {
            self.prepared_next
                .cancel(MediaInstallCancellationCause::StructuralInvalidation);
            return None;
        };
        if target.active.player_binding_generation() != binding.binding_generation() {
            self.prepared_next
                .cancel(MediaInstallCancellationCause::StructuralInvalidation);
            return None;
        }
        self.prepared_next
            .should_start(target, snapshot, Instant::now())
            .then_some(target)
    }

    /// Запускает уже materialized app source request в отдельном speculative owner-е.
    pub(crate) fn start_next_item_preload(
        &mut self,
        target: QueuePreloadTarget,
        source_request: MediaOpenSourceRequest,
    ) -> Result<(), MediaOpenStartError> {
        self.prepared_next.start(target, source_request)
    }

    /// Запрещает frame-loop повторять заведомо rejected target до следующего key.
    pub(crate) fn mark_next_item_preload_failed(&mut self, target: QueuePreloadTarget) {
        self.prepared_next.mark_failed(target);
    }

    /// Только exact automatic install может consume matching fresh prepared envelope.
    pub(crate) fn take_prepared_next_for_install(
        &mut self,
        install: &PlannedPlaylistInstall,
    ) -> Option<PreparedNextMedia> {
        if !matches!(
            &install.mutation,
            PlaylistInstallMutation::AutomaticTraversal(_)
        ) {
            self.prepared_next
                .cancel(MediaInstallCancellationCause::Superseded);
            return None;
        }
        let controller = self.controller.as_ref()?;
        let active = controller.active_media()?;
        if controller.queue().revision_snapshot() != install.expected_queue_revision {
            self.prepared_next
                .cancel(MediaInstallCancellationCause::StructuralInvalidation);
            return None;
        }
        let target = QueuePreloadTarget {
            active,
            expected_queue_revision: install.expected_queue_revision,
            item_id: install.item_id,
        };
        self.prepared_next.take_ready(target, Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::path::PathBuf;

    use media_core::{DemuxSeekResult, Demuxer};
    use player_core::{MediaInstallRequestId, MediaInstanceId};
    use playlist_core::{
        CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistItemId, PlaylistMediaKind,
        PlaylistQueue,
    };

    use super::*;
    use crate::app_wake::AppWakeOwner;
    use crate::media_open::{
        ActiveMediaSource, AuthorizationDispatchResolution, MediaOpenRequestId,
    };
    use crate::playlist_runtime::controller::{
        ControllerAppendOutcome, ControllerPlayItemOutcome, InstallReadyOutcome,
        PlaylistInstallRequest,
    };
    use crate::playlist_runtime::identity::{
        ActiveMediaIdentity, ActiveMediaLineageId, TransportActionOrigin,
    };

    #[derive(Default)]
    struct EmptyDemuxer;

    impl Demuxer for EmptyDemuxer {
        fn tracks(&self) -> &[media_core::TrackInfo] {
            &[]
        }

        fn duration(&self) -> Option<Duration> {
            None
        }

        fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
            Ok(media_core::DemuxReadEvent::EndOfStream)
        }

        fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            panic!("prepared-next envelope test does not seek")
        }
    }

    fn non_zero(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("test identity is non-zero")
    }

    fn item_id(value: u64) -> PlaylistItemId {
        PlaylistItemId::from_persistence_value(value).expect("test item identity is non-zero")
    }

    fn target(next_item_value: u64) -> QueuePreloadTarget {
        let current_item = item_id(11);
        QueuePreloadTarget {
            active: ActiveMediaIdentity::installed(
                Some(current_item),
                ActiveMediaLineageId::from_non_zero(non_zero(21)),
                player_core::MediaInstanceId::from_non_zero(non_zero(31)),
                super::super::PlaylistBindingGeneration(41),
            ),
            expected_queue_revision: PlaylistQueue::new().revision_snapshot(),
            item_id: item_id(next_item_value),
        }
    }

    fn prepared_open() -> PreparedMediaOpen {
        let fixture_path = PathBuf::from("prepared-next-fixture.wav");
        PreparedMediaOpen::from_caller_prepared(
            player_core::PreparedMedia::from_external_label(
                "prepared-next-fixture.wav",
                Box::new(EmptyDemuxer),
            ),
            ActiveMediaSource::LocalFile(fixture_path.clone()),
            SafeMediaLabel::from_local_path(&fixture_path),
        )
    }

    fn ready_state(target: QueuePreloadTarget, prepared_at: Instant) -> PreparedNextState {
        PreparedNextState::Ready {
            key: PreparedNextKey::from(target),
            prepared_at,
            prepared_open: Box::new(prepared_open()),
            safe_label: SafeMediaLabel::from_local_path(&PathBuf::from(
                "prepared-next-fixture.wav",
            )),
        }
    }

    fn snapshot(state: PlaybackState, position_seconds: u64) -> PlayerSnapshot {
        let mut snapshot = PlayerSnapshot::empty();
        snapshot.playback_state = state;
        snapshot.current_position = Duration::from_secs(position_seconds);
        snapshot.duration = Some(Duration::from_secs(120));
        snapshot
    }

    fn playlist_draft(index: usize) -> PlaylistItemDraft {
        let label = format!("prepared-next-track-{index}.mkv");
        PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from(&label)),
            None,
            CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video),
        )
    }

    /// Собирает настоящий controller install, чтобы policy test проходил через natural runtime poll.
    fn runtime_with_installed_queue(
        initial_config: PlaylistConfig,
    ) -> (PlaylistRuntime, PlaylistRuntimeBinding, ActiveMediaIdentity) {
        let mut runtime = PlaylistRuntime::new_with_config(
            AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime),
            initial_config,
        );
        runtime.resolve_missing_state_for_test();
        let binding = runtime
            .bind_resumed_app_state()
            .expect("test runtime accepts resumed binding");
        let controller = runtime
            .controller
            .as_mut()
            .expect("resolved runtime owns controller");
        let item_ids = match controller
            .append((0..2).map(playlist_draft).collect())
            .expect("test queue append succeeds")
        {
            ControllerAppendOutcome::Added { item_ids, .. } => item_ids,
            ControllerAppendOutcome::NoItemsProvided => panic!("test append is non-empty"),
        };
        let ControllerPlayItemOutcome::StartInstall { install, .. } =
            controller.play_item(item_ids[0], TransportActionOrigin::Ui)
        else {
            panic!("first committed item starts a real install");
        };
        let request_id = MediaOpenRequestId::from_non_zero(non_zero(101));
        let player_request_id = MediaInstallRequestId::from_non_zero(non_zero(102));
        controller
            .accept_install_request(PlaylistInstallRequest {
                request_id,
                player_request_id,
                target_item_id: Some(install.item_id),
                origin: install.pending_origin,
                intent_revision: install.intent_revision,
                expected_queue_revision: install.expected_queue_revision,
                mutation: install.mutation,
            })
            .expect("test install request is admitted");
        assert!(matches!(
            controller.on_ready_to_commit(request_id),
            InstallReadyOutcome::RequestAuthorization { .. }
        ));
        controller
            .begin_authorization_dispatch(request_id)
            .expect("authorization dispatch begins");
        assert!(
            controller
                .resolve_authorization_dispatch(
                    request_id,
                    AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
                )
                .expect("authorization enqueue wins")
                .is_none()
        );
        let installed = controller
            .on_installed(
                request_id,
                player_request_id,
                MediaInstanceId::from_non_zero(non_zero(103)),
                binding.binding_generation(),
            )
            .expect("exact Installed commits queue identity")
            .active_media
            .expect("installed drain carries active identity");

        (runtime, binding, installed)
    }

    fn installed_snapshot(active: ActiveMediaIdentity, position_seconds: u64) -> PlayerSnapshot {
        let mut snapshot = snapshot(PlaybackState::Playing, position_seconds);
        snapshot.media_instance_id = Some(active.media_instance_id());
        snapshot
    }

    #[test]
    fn default_policy_opens_only_bounded_playing_lead_window() {
        let policy = PreparedNextPolicy::from_validated_config(PlaylistConfig::default());

        assert!(!policy.scheduling_window_is_open(&snapshot(PlaybackState::Playing, 89)));
        assert!(policy.scheduling_window_is_open(&snapshot(PlaybackState::Playing, 90)));
        assert!(!policy.scheduling_window_is_open(&snapshot(PlaybackState::Paused, 100)));
        let mut live_snapshot = snapshot(PlaybackState::Playing, 100);
        live_snapshot.duration = None;
        assert!(!policy.scheduling_window_is_open(&live_snapshot));
    }

    #[test]
    fn disabled_owner_refuses_target_reconciliation_and_keeps_worker_lazy() {
        let config = PlaylistConfig {
            next_item_preload_enabled: false,
            ..PlaylistConfig::default()
        };
        let mut owner = PreparedNextOwner::new(
            AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime),
            config,
        );

        assert!(!owner.needs_target_reconciliation(&snapshot(PlaybackState::Playing, 119,)));
        assert!(matches!(&owner.state, PreparedNextState::Idle));
        assert_eq!(
            owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::Completed
        );
    }

    #[test]
    fn ready_envelope_requires_exact_key_and_hold_window_or_cold_fallback_wins() {
        let config = PlaylistConfig::default();
        let mut owner = PreparedNextOwner::new(
            AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime),
            config,
        );
        let exact_target = target(12);
        let different_target = target(13);
        let now = Instant::now();

        owner.state = ready_state(exact_target, now);
        assert!(owner.take_ready(exact_target, now).is_some());

        owner.state = ready_state(exact_target, now);
        assert!(owner.take_ready(different_target, now).is_none());
        assert!(matches!(&owner.state, PreparedNextState::Idle));

        owner.state = ready_state(
            exact_target,
            now.checked_sub(owner.policy.max_hold + Duration::from_millis(1))
                .expect("test instant can represent expired envelope"),
        );
        assert!(owner.take_ready(exact_target, now).is_none());
        assert!(matches!(&owner.state, PreparedNextState::Idle));
        assert_eq!(
            owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::Completed
        );
    }

    #[test]
    fn staged_preload_policy_reconfigures_natural_poll_and_rolls_back_without_media_restart() {
        let initial_config = PlaylistConfig::default();
        let (mut runtime, binding, installed) = runtime_with_installed_queue(initial_config);
        let initial_target = runtime
            .poll_next_item_preload_target(binding, &installed_snapshot(installed, 90))
            .expect("default policy opens its natural lead window");
        runtime.prepared_next.state = ready_state(initial_target, Instant::now());
        let disabled = PlaylistConfig {
            next_item_preload_enabled: false,
            next_item_preload_budget_mb: 80,
            next_item_preload_lead_time_ms: 10_000,
            next_item_preload_max_hold_ms: 20_000,
            ..initial_config
        };

        assert!(
            runtime
                .stage_playlist_settings(disabled)
                .expect("disabled preload policy stage succeeds")
        );
        assert!(matches!(
            &runtime.prepared_next.state,
            PreparedNextState::Idle
        ));
        assert_eq!(
            runtime.prepared_next.policy,
            PreparedNextPolicy::from_validated_config(disabled)
        );
        assert_eq!(runtime.controller.active_media(), Some(installed));
        assert!(
            runtime
                .poll_next_item_preload_target(binding, &installed_snapshot(installed, 119))
                .is_none(),
            "disabled policy must suppress even an open lead window"
        );

        runtime.finalize_playlist_settings();
        assert!(
            !runtime
                .stage_playlist_settings(disabled)
                .expect("same preload policy is a safe no-op")
        );

        let enabled = PlaylistConfig {
            next_item_preload_enabled: true,
            next_item_preload_budget_mb: 96,
            next_item_preload_lead_time_ms: 5_000,
            next_item_preload_max_hold_ms: 10_000,
            ..disabled
        };
        assert!(
            runtime
                .stage_playlist_settings(enabled)
                .expect("enabled preload policy stage succeeds")
        );
        assert_eq!(
            runtime.prepared_next.policy.resource_budget,
            QueuePreloadResourceBudget::from_validated_config(enabled.next_item_preload_budget_mb,)
        );
        assert!(
            runtime
                .poll_next_item_preload_target(binding, &installed_snapshot(installed, 114))
                .is_none(),
            "new five-second lead window must reject six seconds remaining"
        );
        let enabled_target = runtime
            .poll_next_item_preload_target(binding, &installed_snapshot(installed, 115))
            .expect("new enabled policy starts at its exact lead boundary");
        assert_eq!(enabled_target.active, installed);
        assert_eq!(runtime.controller.active_media(), Some(installed));

        runtime.prepared_next.state = ready_state(enabled_target, Instant::now());
        assert!(
            runtime
                .poll_next_item_preload_target(binding, &installed_snapshot(installed, 115))
                .is_none(),
            "new ten-second hold must keep a fresh ready envelope"
        );
        runtime.prepared_next.state = ready_state(
            enabled_target,
            Instant::now()
                .checked_sub(Duration::from_millis(10_001))
                .expect("test instant represents expired configured hold"),
        );
        assert_eq!(
            runtime.poll_next_item_preload_target(binding, &installed_snapshot(installed, 115),),
            Some(enabled_target),
            "natural poll must expire the envelope at the new hold boundary"
        );

        runtime.finalize_playlist_settings();
        runtime.prepared_next.state = PreparedNextState::Preparing {
            key: PreparedNextKey::from(enabled_target),
        };
        let rollback_candidate = PlaylistConfig {
            next_item_preload_budget_mb: 112,
            next_item_preload_lead_time_ms: 6_000,
            next_item_preload_max_hold_ms: 12_000,
            ..enabled
        };
        assert!(
            runtime
                .stage_playlist_settings(rollback_candidate)
                .expect("replacement preload policy stage succeeds")
        );
        assert!(matches!(
            &runtime.prepared_next.state,
            PreparedNextState::Idle
        ));
        assert_eq!(runtime.controller.active_media(), Some(installed));

        assert_eq!(runtime.rollback_playlist_settings(), Ok(true));
        assert_eq!(
            runtime.prepared_next.policy,
            PreparedNextPolicy::from_validated_config(enabled)
        );
        assert_eq!(runtime.controller.active_media(), Some(installed));
    }
}
