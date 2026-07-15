use rustiplayer_config::{PlaylistConfig, PlaylistErrorBehavior as ConfigErrorBehavior};

use playlist_core::RepeatMode;

use super::controller::{PlaylistController, PlaylistErrorBehavior};

/// Typed stage failure distinguishes pre-mutation rejection from partial freeze.
#[derive(Debug)]
pub(crate) enum PlaylistSettingsStageError {
    Failed(String),
    PartialFailure(String),
}

/// Типизированная revision policy snapshot для будущего discovery job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PlaylistDiscoveryPolicyRevision(u64);

impl PlaylistDiscoveryPolicyRevision {
    /// Начальная revision startup policy.
    const INITIAL: Self = Self(0);

    /// Создаёт следующую revision без молчаливого насыщения identity.
    fn next(self) -> Result<Self, String> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| "playlist discovery policy revision exhausted".to_owned())
    }
}

/// Immutable policy captured by the next sibling discovery job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FutureDiscoveryPolicy {
    pub(super) load_siblings: bool,
    pub(super) sibling_media_filter: rustiplayer_config::PlaylistSiblingMediaFilter,
    pub(super) revision: PlaylistDiscoveryPolicyRevision,
}

/// Exact frozen discovery scope returned by the discovery owner port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FrozenDiscoveryScope(u64);

/// Узкий D62 boundary к текущему discovery job.
pub(super) trait PlaylistDiscoverySettingsPort {
    /// Freeze-ит admission exact active scan или возвращает `None`, если scan отсутствует.
    fn freeze_active_scan(&mut self) -> Result<Option<FrozenDiscoveryScope>, String>;
    /// Возобновляет exact scope при rollback до persistence.
    fn resume_frozen_scan(&mut self, scope: FrozenDiscoveryScope) -> Result<(), String>;
    /// Безошибочно и идемпотентно отменяет exact scope после persistence.
    fn finalize_cancel_frozen_scan(&mut self, scope: FrozenDiscoveryScope);
}

/// До Session 14 discovery coordinator не подключён, поэтому active scan отсутствует явно.
#[derive(Default)]
struct DetachedDiscoverySettingsPort;

impl PlaylistDiscoverySettingsPort for DetachedDiscoverySettingsPort {
    fn freeze_active_scan(&mut self) -> Result<Option<FrozenDiscoveryScope>, String> {
        Ok(None)
    }

    fn resume_frozen_scan(&mut self, _scope: FrozenDiscoveryScope) -> Result<(), String> {
        Ok(())
    }

    fn finalize_cancel_frozen_scan(&mut self, _scope: FrozenDiscoveryScope) {}
}

/// Узкий hook к latest-only state save scheduler.
pub(super) trait PlaylistSaveDebouncePort {
    /// Reschedule-ит pending write, сохраняя newest dirty revision.
    fn reschedule_debounce(&mut self, debounce_ms: u64) -> Result<(), String>;
}

/// Stateful shell до подключения реального SaveWorker в Session 14.
struct DetachedSaveDebouncePort {
    configured_debounce_ms: u64,
    pending_dirty_revision: Option<u64>,
}

impl DetachedSaveDebouncePort {
    fn new(configured_debounce_ms: u64) -> Self {
        Self {
            configured_debounce_ms,
            pending_dirty_revision: None,
        }
    }
}

impl PlaylistSaveDebouncePort for DetachedSaveDebouncePort {
    fn reschedule_debounce(&mut self, debounce_ms: u64) -> Result<(), String> {
        // Меняется только deadline policy; latest dirty ownership не переносится и не очищается.
        let pending_dirty_revision = self.pending_dirty_revision;
        self.configured_debounce_ms = debounce_ms;
        self.pending_dirty_revision = pending_dirty_revision;
        Ok(())
    }
}

struct StagedPlaylistSettingsTransaction {
    previous: PlaylistConfig,
    previous_policy_revision: PlaylistDiscoveryPolicyRevision,
    frozen_scope: Option<FrozenDiscoveryScope>,
    debounce_changed: bool,
}

/// Process-lifetime owner playlist settings и одной staged settings transaction.
pub(super) struct PlaylistSettingsOwner {
    committed: PlaylistConfig,
    future_discovery_policy_revision: PlaylistDiscoveryPolicyRevision,
    discovery_port: Box<dyn PlaylistDiscoverySettingsPort>,
    save_debounce_port: Box<dyn PlaylistSaveDebouncePort>,
    staged: Option<StagedPlaylistSettingsTransaction>,
}

impl PlaylistSettingsOwner {
    pub(super) fn new(committed: PlaylistConfig) -> Self {
        Self {
            committed,
            future_discovery_policy_revision: PlaylistDiscoveryPolicyRevision::INITIAL,
            discovery_port: Box::<DetachedDiscoverySettingsPort>::default(),
            save_debounce_port: Box::new(DetachedSaveDebouncePort::new(
                committed.state_save_debounce_ms,
            )),
            staged: None,
        }
    }

    pub(super) fn preflight(&self) -> Result<(), String> {
        if self.staged.is_some() {
            return Err("playlist settings transaction уже staged".to_string());
        }
        Ok(())
    }

    /// Инициализирует только новую runtime queue до будущего persisted-state restore.
    #[allow(
        dead_code,
        reason = "Session 14 startup integration consumes this policy"
    )]
    pub(super) fn initialize_new_queue_policy(&self, controller: &mut PlaylistController) {
        controller.set_error_behavior(controller_error_behavior(self.committed.error_behavior));
        controller.repeat_mode = match self.committed.playback_behavior {
            rustiplayer_config::PlaylistPlaybackBehavior::StopAfterLast => RepeatMode::StopAtEnd,
            rustiplayer_config::PlaylistPlaybackBehavior::RepeatQueue => RepeatMode::RepeatQueue,
            rustiplayer_config::PlaylistPlaybackBehavior::RepeatOne => RepeatMode::RepeatOne,
        };
    }

    /// Persisted repeat/shuffle не заменяются config defaults; error policy runtime-only.
    #[allow(
        dead_code,
        reason = "Session 14 startup integration consumes this policy"
    )]
    pub(super) fn initialize_restored_queue_policy(&self, controller: &mut PlaylistController) {
        controller.set_error_behavior(controller_error_behavior(self.committed.error_behavior));
    }

    pub(super) fn stage(
        &mut self,
        requested: PlaylistConfig,
        controller: &mut PlaylistController,
    ) -> Result<bool, PlaylistSettingsStageError> {
        self.preflight()
            .map_err(PlaylistSettingsStageError::Failed)?;
        if requested == self.committed {
            return Ok(false);
        }

        let next_policy_revision = if self.committed.load_siblings != requested.load_siblings
            || self.committed.sibling_media_filter != requested.sibling_media_filter
        {
            Some(
                self.future_discovery_policy_revision
                    .next()
                    .map_err(PlaylistSettingsStageError::Failed)?,
            )
        } else {
            None
        };

        let frozen_scope = if self.committed.load_siblings && !requested.load_siblings {
            self.discovery_port
                .freeze_active_scan()
                .map_err(PlaylistSettingsStageError::Failed)?
        } else {
            None
        };

        let debounce_changed =
            self.committed.state_save_debounce_ms != requested.state_save_debounce_ms;
        if debounce_changed
            && let Err(error) = self
                .save_debounce_port
                .reschedule_debounce(requested.state_save_debounce_ms)
        {
            if let Some(scope) = frozen_scope
                && let Err(resume_error) = self.discovery_port.resume_frozen_scan(scope)
            {
                self.staged = Some(StagedPlaylistSettingsTransaction {
                    previous: self.committed,
                    previous_policy_revision: self.future_discovery_policy_revision,
                    frozen_scope: Some(scope),
                    debounce_changed,
                });
                return Err(PlaylistSettingsStageError::PartialFailure(format!(
                    "debounce reconfigure failed: {error}; exact discovery resume failed: {resume_error}"
                )));
            }
            return Err(PlaylistSettingsStageError::Failed(error));
        }

        let previous = self.committed;
        let previous_policy_revision = self.future_discovery_policy_revision;
        if let Some(next_policy_revision) = next_policy_revision {
            self.future_discovery_policy_revision = next_policy_revision;
        }
        controller.set_error_behavior(controller_error_behavior(requested.error_behavior));
        self.committed = requested;
        self.staged = Some(StagedPlaylistSettingsTransaction {
            previous,
            previous_policy_revision,
            frozen_scope,
            debounce_changed,
        });
        Ok(true)
    }

    pub(super) fn rollback(&mut self, controller: &mut PlaylistController) -> Result<bool, String> {
        let Some(staged) = self.staged.as_ref() else {
            return Ok(false);
        };

        if staged.debounce_changed {
            self.save_debounce_port
                .reschedule_debounce(staged.previous.state_save_debounce_ms)?;
        }
        if let Some(scope) = staged.frozen_scope {
            self.discovery_port.resume_frozen_scan(scope)?;
        }
        controller.set_error_behavior(controller_error_behavior(staged.previous.error_behavior));
        self.committed = staged.previous;
        self.future_discovery_policy_revision = staged.previous_policy_revision;
        self.staged = None;
        Ok(true)
    }

    pub(super) fn future_discovery_policy(&self) -> FutureDiscoveryPolicy {
        FutureDiscoveryPolicy {
            load_siblings: self.committed.load_siblings,
            sibling_media_filter: self.committed.sibling_media_filter,
            revision: self.future_discovery_policy_revision,
        }
    }

    pub(super) fn previous_restart_threshold(&self) -> super::controller::PreviousRestartThreshold {
        super::controller::PreviousRestartThreshold::from_milliseconds(
            self.committed.previous_restart_threshold_ms,
        )
        .expect("validated playlist threshold must fit controller contract")
    }

    pub(super) fn finalize(&mut self) {
        let Some(staged) = self.staged.take() else {
            return;
        };
        if let Some(scope) = staged.frozen_scope {
            self.discovery_port.finalize_cancel_frozen_scan(scope);
        }
    }

    #[cfg(test)]
    pub(super) fn committed(&self) -> PlaylistConfig {
        self.committed
    }

    /// Session 14 подключает process-lifetime worker, не меняя discovery owner.
    pub(super) fn install_save_debounce_port(
        &mut self,
        save_debounce_port: Box<dyn PlaylistSaveDebouncePort>,
    ) {
        self.save_debounce_port = save_debounce_port;
    }

    #[cfg(test)]
    pub(super) fn replace_ports(
        &mut self,
        discovery_port: Box<dyn PlaylistDiscoverySettingsPort>,
        save_debounce_port: Box<dyn PlaylistSaveDebouncePort>,
    ) {
        self.discovery_port = discovery_port;
        self.save_debounce_port = save_debounce_port;
    }
}

fn controller_error_behavior(behavior: ConfigErrorBehavior) -> PlaylistErrorBehavior {
    match behavior {
        ConfigErrorBehavior::Stop => PlaylistErrorBehavior::Stop,
        ConfigErrorBehavior::Skip => PlaylistErrorBehavior::Skip,
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use playlist_core::RepeatMode;
    use rustiplayer_config::{
        PlaylistErrorBehavior as ConfigErrorBehavior, PlaylistPlaybackBehavior,
        PlaylistSiblingMediaFilter,
    };

    use super::*;

    #[derive(Debug, Default)]
    struct DiscoveryState {
        calls: Vec<&'static str>,
        fail_resume: bool,
    }

    struct RecordingDiscoveryPort(Rc<RefCell<DiscoveryState>>);

    impl PlaylistDiscoverySettingsPort for RecordingDiscoveryPort {
        fn freeze_active_scan(&mut self) -> Result<Option<FrozenDiscoveryScope>, String> {
            self.0.borrow_mut().calls.push("freeze");
            Ok(Some(FrozenDiscoveryScope(41)))
        }

        fn resume_frozen_scan(&mut self, scope: FrozenDiscoveryScope) -> Result<(), String> {
            assert_eq!(scope, FrozenDiscoveryScope(41));
            self.0.borrow_mut().calls.push("resume");
            if self.0.borrow().fail_resume {
                return Err("resume rejected".to_string());
            }
            Ok(())
        }

        fn finalize_cancel_frozen_scan(&mut self, scope: FrozenDiscoveryScope) {
            assert_eq!(scope, FrozenDiscoveryScope(41));
            self.0.borrow_mut().calls.push("cancel");
        }
    }

    #[derive(Debug)]
    struct DebounceState {
        calls: Vec<u64>,
        latest_dirty_revision: u64,
        fail_next: bool,
    }

    struct RecordingDebouncePort(Rc<RefCell<DebounceState>>);

    impl PlaylistSaveDebouncePort for RecordingDebouncePort {
        fn reschedule_debounce(&mut self, debounce_ms: u64) -> Result<(), String> {
            let mut state = self.0.borrow_mut();
            state.calls.push(debounce_ms);
            if state.fail_next {
                state.fail_next = false;
                return Err("debounce rejected".to_string());
            }
            Ok(())
        }
    }

    fn owner_with_ports(
        discovery: Rc<RefCell<DiscoveryState>>,
        debounce: Rc<RefCell<DebounceState>>,
    ) -> PlaylistSettingsOwner {
        let mut owner = PlaylistSettingsOwner::new(PlaylistConfig::default());
        owner.replace_ports(
            Box::new(RecordingDiscoveryPort(discovery)),
            Box::new(RecordingDebouncePort(debounce)),
        );
        owner
    }

    #[test]
    fn persist_failure_rollback_resumes_exact_scan_and_restores_policy() {
        let discovery = Rc::new(RefCell::new(DiscoveryState::default()));
        let debounce = Rc::new(RefCell::new(DebounceState {
            calls: Vec::new(),
            latest_dirty_revision: 17,
            fail_next: false,
        }));
        let mut owner = owner_with_ports(discovery.clone(), debounce.clone());
        let mut controller = PlaylistController::new();
        let requested = PlaylistConfig {
            load_siblings: false,
            error_behavior: ConfigErrorBehavior::Skip,
            state_save_debounce_ms: 3_000,
            ..PlaylistConfig::default()
        };

        assert!(owner.stage(requested, &mut controller).expect("stage"));
        assert!(owner.rollback(&mut controller).expect("rollback"));

        assert_eq!(discovery.borrow().calls, vec!["freeze", "resume"]);
        assert_eq!(debounce.borrow().calls, vec![3_000, 2_000]);
        assert_eq!(debounce.borrow().latest_dirty_revision, 17);
        assert_eq!(owner.committed(), PlaylistConfig::default());
    }

    #[test]
    fn successful_persistence_finalizes_cancel_without_changing_existing_repeat() {
        let discovery = Rc::new(RefCell::new(DiscoveryState::default()));
        let debounce = Rc::new(RefCell::new(DebounceState {
            calls: Vec::new(),
            latest_dirty_revision: 23,
            fail_next: false,
        }));
        let mut owner = owner_with_ports(discovery.clone(), debounce);
        let mut controller = PlaylistController::new();
        controller.repeat_mode = RepeatMode::RepeatQueue;
        let requested = PlaylistConfig {
            load_siblings: false,
            playback_behavior: PlaylistPlaybackBehavior::RepeatOne,
            ..PlaylistConfig::default()
        };

        owner.stage(requested, &mut controller).expect("stage");
        owner.finalize();
        owner.finalize();

        assert_eq!(discovery.borrow().calls, vec!["freeze", "cancel"]);
        assert_eq!(controller.repeat_mode, RepeatMode::RepeatQueue);
        assert_eq!(owner.committed(), requested);
    }

    #[test]
    fn enabling_and_filter_change_only_update_future_policy_without_scan() {
        let discovery = Rc::new(RefCell::new(DiscoveryState::default()));
        let debounce = Rc::new(RefCell::new(DebounceState {
            calls: Vec::new(),
            latest_dirty_revision: 5,
            fail_next: false,
        }));
        let initial = PlaylistConfig {
            load_siblings: false,
            ..PlaylistConfig::default()
        };
        let mut owner = PlaylistSettingsOwner::new(initial);
        owner.replace_ports(
            Box::new(RecordingDiscoveryPort(discovery.clone())),
            Box::new(RecordingDebouncePort(debounce)),
        );
        let mut controller = PlaylistController::new();
        let mut requested = initial;
        requested.load_siblings = true;
        requested.sibling_media_filter = PlaylistSiblingMediaFilter::AudioOnly;

        owner.stage(requested, &mut controller).expect("stage");
        owner.finalize();

        assert!(discovery.borrow().calls.is_empty());
        assert_eq!(owner.committed(), requested);
        let future = owner.future_discovery_policy();
        assert!(future.load_siblings);
        assert_eq!(
            future.sibling_media_filter,
            PlaylistSiblingMediaFilter::AudioOnly
        );
        assert_eq!(future.revision, PlaylistDiscoveryPolicyRevision(1));
    }

    #[test]
    fn exhausted_policy_revision_rejects_before_runtime_mutation() {
        let discovery = Rc::new(RefCell::new(DiscoveryState::default()));
        let debounce = Rc::new(RefCell::new(DebounceState {
            calls: Vec::new(),
            latest_dirty_revision: 29,
            fail_next: false,
        }));
        let mut owner = owner_with_ports(discovery.clone(), debounce.clone());
        owner.future_discovery_policy_revision = PlaylistDiscoveryPolicyRevision(u64::MAX);
        let mut controller = PlaylistController::new();
        let requested = PlaylistConfig {
            sibling_media_filter: PlaylistSiblingMediaFilter::AudioOnly,
            ..PlaylistConfig::default()
        };

        let error = owner
            .stage(requested, &mut controller)
            .expect_err("revision exhaustion must reject the stage");

        assert!(matches!(error, PlaylistSettingsStageError::Failed(_)));
        assert!(discovery.borrow().calls.is_empty());
        assert!(debounce.borrow().calls.is_empty());
        assert_eq!(owner.committed(), PlaylistConfig::default());
    }

    #[test]
    fn failed_debounce_and_resume_is_partial_and_remains_compensatable() {
        let discovery = Rc::new(RefCell::new(DiscoveryState {
            calls: Vec::new(),
            fail_resume: true,
        }));
        let debounce = Rc::new(RefCell::new(DebounceState {
            calls: Vec::new(),
            latest_dirty_revision: 29,
            fail_next: true,
        }));
        let mut owner = owner_with_ports(discovery.clone(), debounce.clone());
        let mut controller = PlaylistController::new();
        let requested = PlaylistConfig {
            load_siblings: false,
            state_save_debounce_ms: 3_000,
            ..PlaylistConfig::default()
        };

        let error = owner
            .stage(requested, &mut controller)
            .expect_err("failed exact resume is a partial mutation");
        assert!(matches!(
            error,
            PlaylistSettingsStageError::PartialFailure(_)
        ));
        assert!(owner.preflight().is_err());

        discovery.borrow_mut().fail_resume = false;
        assert!(owner.rollback(&mut controller).expect("retry compensation"));
        assert_eq!(discovery.borrow().calls, vec!["freeze", "resume", "resume"]);
        assert_eq!(debounce.borrow().latest_dirty_revision, 29);
        assert_eq!(owner.committed(), PlaylistConfig::default());
    }

    #[test]
    fn startup_default_repeat_initializes_new_queue_once() {
        let config = PlaylistConfig {
            playback_behavior: PlaylistPlaybackBehavior::RepeatOne,
            error_behavior: ConfigErrorBehavior::Skip,
            ..PlaylistConfig::default()
        };
        let owner = PlaylistSettingsOwner::new(config);
        let mut controller = PlaylistController::new();

        owner.initialize_new_queue_policy(&mut controller);

        assert_eq!(controller.repeat_mode, RepeatMode::RepeatOne);
    }
}
