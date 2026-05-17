use std::time::{Duration, Instant};

#[cfg(test)]
use crate::seek_controller::SeekControllerDiagnostics;
use crate::seek_controller::{
    DeferredPreviewStatus, FinishScrubDecision, PlaybackResumeIntent, PreviewDispatchDecision,
    PreviewQueueDecision, ScrubUpdateAcceptance, ScrubUpdatePreviewIntent, SeekController,
    SeekScrubInterruptDecision,
};
use crate::{
    PlaybackState, PlayerCommand, ScrubCommitIntent, ScrubGeneration, ScrubUpdateIntent,
    SeekRequest, SessionScrubCommand,
};

/// Выбор preview-поведения после принятого scrub update-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubPreviewDispatch {
    /// Обычный drag/update path: latest target должен получить live preview.
    Allow,

    /// Release path: latest target нужен только final commit-у, без нового preview.
    SuppressForCommit,
}

/// Решение driver-а после начала interactive scrub.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScrubBeginDecision {
    /// Команда для session-side scrub state machine.
    pub session_command: SessionScrubCommand,

    /// Playback-команда, которой worker временно ставит playback на паузу.
    pub player_command: PlayerCommand,
}

/// Решение driver-а после принятого scrub update-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrubUpdateDecision {
    /// Команда обновления latest target для `PlayerSession`.
    pub session_command: Option<SessionScrubCommand>,

    /// Due preview intent, который worker должен отправить после session update.
    pub due_preview: Option<ScrubDuePreview>,

    /// `true`, если update относится не к текущему active scrub generation.
    pub stale: bool,
}

impl ScrubUpdateDecision {
    /// Возвращает decision для stale update-а без session-side effects.
    const fn stale() -> Self {
        Self {
            session_command: None,
            due_preview: None,
            stale: true,
        }
    }
}

/// Preview seek, который стал due внутри текущего throttle window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrubDuePreview {
    /// Preview intent, который нужно проверить и отправить после session update.
    pub intent: ScrubUpdateIntent,

    /// Монотонное время, которое станет anchor-ом следующего throttle window.
    pub dispatched_at: Instant,
}

/// Решение driver-а по preview scrub seek-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrubPreviewDecision {
    /// Preview intent, для которого принято решение.
    pub intent: Option<ScrubUpdateIntent>,

    /// Preview-команда, которую worker должен отправить в `PlayerSession`.
    pub session_command: Option<SessionScrubCommand>,

    /// `true`, если preview intent больше не соответствует active scrub generation/target.
    pub stale: bool,
}

impl ScrubPreviewDecision {
    /// Возвращает пустой decision, когда preview сейчас не нужен.
    const fn idle() -> Self {
        Self {
            intent: None,
            session_command: None,
            stale: false,
        }
    }

    /// Возвращает stale decision без session-side effects.
    const fn stale(intent: ScrubUpdateIntent) -> Self {
        Self {
            intent: Some(intent),
            session_command: None,
            stale: true,
        }
    }
}

/// Решение driver-а после завершения interactive scrub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrubEndDecision {
    /// Команда финального scrub commit-а для `PlayerSession`.
    pub session_command: Option<SessionScrubCommand>,

    /// Playback intent, сохранённый на `BeginScrub`.
    pub resume_intent: Option<PlaybackResumeIntent>,

    /// `true`, если `EndScrub` относится к старому generation.
    pub stale: bool,
}

impl ScrubEndDecision {
    /// Возвращает decision для stale `EndScrub` без session-side effects.
    const fn stale() -> Self {
        Self {
            session_command: None,
            resume_intent: None,
            stale: true,
        }
    }
}

/// Результат принудительного interrupt-а interactive scrub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrubInterruptDecision {
    /// Был ли активный scrub до interrupt-а.
    pub was_scrubbing: bool,
}

/// Worker-side coordinator для interactive scrub.
///
/// Driver владеет только scrub state и принимает решения, но не знает о каналах,
/// `PlayerSession`, snapshot publishing и render bridge.
#[derive(Debug, Clone)]
pub(crate) struct ScrubDriver {
    /// Generation-aware state machine для seek/scrub intent-ов.
    seek_controller: SeekController,

    /// Последний момент, когда worker запускал live preview seek.
    last_preview_scrub_seek_at: Option<Instant>,

    /// Минимальный интервал между live preview seek-ами во время scrub.
    live_scrub_preview_interval: Duration,
}

impl ScrubDriver {
    /// Создаёт scrub driver с production throttle interval из worker config.
    #[must_use]
    pub(crate) fn new(live_scrub_preview_interval: Duration) -> Self {
        Self {
            seek_controller: SeekController::new(),
            last_preview_scrub_seek_at: None,
            live_scrub_preview_interval,
        }
    }

    /// Возвращает generation для следующего `BeginScrub`.
    #[must_use]
    pub(crate) fn next_begin_generation(&self) -> ScrubGeneration {
        self.seek_controller.generation_id().next()
    }

    /// Возвращает generation текущего active/sentinel scrub intent-а.
    #[must_use]
    pub(crate) fn current_generation(&self) -> ScrubGeneration {
        self.seek_controller.generation_id()
    }

    /// Возвращает последнюю scrub-цель, если она есть в active generation.
    #[must_use]
    pub(crate) fn latest_scrub_target(&self) -> Option<SeekRequest> {
        self.seek_controller.latest_scrub_target()
    }

    /// Возвращает preview/final target, уже отправленный за worker boundary.
    #[must_use]
    pub(crate) fn in_flight_target(&self) -> Option<SeekRequest> {
        self.seek_controller.in_flight_target()
    }

    /// Возвращает diagnostics counters seek-controller-а.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn diagnostics(&self) -> SeekControllerDiagnostics {
        self.seek_controller.diagnostics()
    }

    /// Возвращает `true`, если сейчас активен interactive scrub.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn is_scrubbing(&self) -> bool {
        self.seek_controller.is_scrubbing()
    }

    /// Обрабатывает Play/Pause/Toggle во время scrub без изменения `PlayerSession`.
    pub(crate) fn consume_resume_intent_command(&mut self, command: &PlayerCommand) -> bool {
        self.seek_controller.consume_resume_intent_command(command)
    }

    /// Начинает новый scrub и возвращает команды, которые должен выполнить worker.
    pub(crate) fn begin_scrub(
        &mut self,
        generation: ScrubGeneration,
        playback_state: PlaybackState,
    ) -> ScrubBeginDecision {
        self.reset_preview_scrub_state();
        self.seek_controller
            .begin_scrub_with_generation(generation, playback_state);

        ScrubBeginDecision {
            session_command: SessionScrubCommand::Begin { generation },
            player_command: PlayerCommand::Pause,
        }
    }

    /// Применяет latest scrub update из drag path и разрешает live preview.
    pub(crate) fn apply_latest_scrub_update_for_drag(
        &mut self,
        intent: ScrubUpdateIntent,
    ) -> ScrubUpdateDecision {
        self.apply_scrub_update_with_preview_dispatch(intent, ScrubPreviewDispatch::Allow)
    }

    /// Применяет latest scrub update перед release без запуска нового preview.
    pub(crate) fn apply_latest_scrub_update_for_commit(
        &mut self,
        intent: ScrubUpdateIntent,
    ) -> ScrubUpdateDecision {
        self.apply_scrub_update_with_preview_dispatch(
            intent,
            ScrubPreviewDispatch::SuppressForCommit,
        )
    }

    /// Применяет explicit scrub update от worker command path.
    pub(crate) fn apply_scrub_update(&mut self, intent: ScrubUpdateIntent) -> ScrubUpdateDecision {
        self.apply_scrub_update_with_preview_dispatch(intent, ScrubPreviewDispatch::Allow)
    }

    /// Применяет update и выбирает, должен ли он породить preview seek.
    fn apply_scrub_update_with_preview_dispatch(
        &mut self,
        intent: ScrubUpdateIntent,
        preview_dispatch: ScrubPreviewDispatch,
    ) -> ScrubUpdateDecision {
        let preview_intent = match preview_dispatch {
            ScrubPreviewDispatch::Allow => ScrubUpdatePreviewIntent::QueueLatest,
            ScrubPreviewDispatch::SuppressForCommit => ScrubUpdatePreviewIntent::SuppressForCommit,
        };
        let update_decision = self
            .seek_controller
            .accept_scrub_update(intent, preview_intent);

        let ScrubUpdateAcceptance::Accepted {
            intent: accepted_intent,
            preview: preview_decision,
        } = update_decision
        else {
            return ScrubUpdateDecision::stale();
        };

        let due_preview = match preview_decision {
            PreviewQueueDecision::Queued { .. } => self.take_due_preview_scrub_seek(Instant::now()),
            PreviewQueueDecision::AlreadyInFlight { .. }
            | PreviewQueueDecision::SuppressedForCommit { .. } => None,
        };

        ScrubUpdateDecision {
            session_command: Some(SessionScrubCommand::Update(accepted_intent)),
            due_preview,
            stale: false,
        }
    }

    /// Готовит explicit preview command, если generation и latest target ещё актуальны.
    pub(crate) fn preview_scrub(&mut self, intent: ScrubUpdateIntent) -> ScrubPreviewDecision {
        self.prepare_preview_scrub(intent, None)
    }

    /// Отправляет отложенный preview seek, когда прошёл throttle interval live scrub-а.
    pub(crate) fn dispatch_due_preview_scrub_seek(&mut self, now: Instant) -> ScrubPreviewDecision {
        let Some(due_preview) = self.take_due_preview_scrub_seek(now) else {
            return ScrubPreviewDecision::idle();
        };

        self.prepare_preview_scrub(due_preview.intent, Some(due_preview.dispatched_at))
    }

    /// Готовит preview seek, который стал due по live scrub throttle.
    pub(crate) fn preview_due_scrub(
        &mut self,
        due_preview: ScrubDuePreview,
    ) -> ScrubPreviewDecision {
        self.prepare_preview_scrub(due_preview.intent, Some(due_preview.dispatched_at))
    }

    /// Забирает отложенный preview intent, если throttle window уже прошёл.
    fn take_due_preview_scrub_seek(&mut self, now: Instant) -> Option<ScrubDuePreview> {
        if !matches!(
            self.seek_controller.deferred_preview_status(),
            DeferredPreviewStatus::Pending { .. }
        ) {
            return None;
        }

        if !self.preview_scrub_interval_elapsed(now) {
            return None;
        }

        match self.seek_controller.take_deferred_preview() {
            DeferredPreviewStatus::Pending { intent } => Some(ScrubDuePreview {
                intent,
                dispatched_at: now,
            }),
            DeferredPreviewStatus::Idle => None,
        }
    }

    /// Возвращает время до ближайшего throttled preview seek-а.
    #[must_use]
    pub(crate) fn next_preview_scrub_timeout(&self, now: Instant) -> Option<Duration> {
        if !matches!(
            self.seek_controller.deferred_preview_status(),
            DeferredPreviewStatus::Pending { .. }
        ) {
            return None;
        }

        let Some(last_seek_at) = self.last_preview_scrub_seek_at else {
            return Some(Duration::ZERO);
        };

        let elapsed_since_preview = now.saturating_duration_since(last_seek_at);
        Some(
            self.live_scrub_preview_interval
                .saturating_sub(elapsed_since_preview),
        )
    }

    /// Завершает scrub и возвращает final session command вместе с resume intent-ом.
    pub(crate) fn end_scrub(&mut self, intent: ScrubCommitIntent) -> ScrubEndDecision {
        match self.seek_controller.finish_scrub(intent) {
            FinishScrubDecision::Accepted { resume_intent, .. } => ScrubEndDecision {
                session_command: Some(SessionScrubCommand::End(intent)),
                resume_intent: Some(resume_intent),
                stale: false,
            },
            FinishScrubDecision::Rejected { .. } => ScrubEndDecision::stale(),
        }
    }

    /// Прерывает active scrub для Stop/Open/Shutdown/load boundary команд.
    pub(crate) fn interrupt_scrub(&mut self) -> ScrubInterruptDecision {
        let interrupt_decision = self.seek_controller.interrupt_active_scrub();
        self.reset_preview_scrub_state();
        ScrubInterruptDecision {
            was_scrubbing: matches!(interrupt_decision, SeekScrubInterruptDecision::Interrupted),
        }
    }

    /// Возвращает `true`, если внешний seek нужно отбросить из-за active scrub.
    pub(crate) fn should_ignore_external_seek(&mut self) -> bool {
        self.seek_controller.should_ignore_external_seek()
    }

    /// Сбрасывает worker-only состояние preview seek-а.
    pub(crate) fn reset_preview_scrub_state(&mut self) {
        self.last_preview_scrub_seek_at = None;
    }

    /// Test hook для сценариев, где нужно удержать preview внутри throttle window.
    #[cfg(test)]
    pub(crate) fn set_last_preview_scrub_seek_at_for_tests(
        &mut self,
        last_seek_at: Option<Instant>,
    ) {
        self.last_preview_scrub_seek_at = last_seek_at;
    }

    /// Проверяет throttle window для live preview seek-а.
    fn preview_scrub_interval_elapsed(&self, now: Instant) -> bool {
        match self.last_preview_scrub_seek_at {
            Some(last_seek_at) => {
                now.saturating_duration_since(last_seek_at) >= self.live_scrub_preview_interval
            }
            None => true,
        }
    }

    /// Помечает preview seek как отправленный и готовит session command.
    fn prepare_preview_scrub(
        &mut self,
        intent: ScrubUpdateIntent,
        dispatched_at: Option<Instant>,
    ) -> ScrubPreviewDecision {
        let preview_decision = self.seek_controller.mark_preview_seek_dispatched(intent);

        let accepted_intent = match preview_decision {
            PreviewDispatchDecision::Dispatch { intent, .. } => intent,
            PreviewDispatchDecision::Rejected { intent, .. } => {
                return ScrubPreviewDecision::stale(intent);
            }
        };

        if let Some(dispatched_at) = dispatched_at {
            self.last_preview_scrub_seek_at = Some(dispatched_at);
        }

        ScrubPreviewDecision {
            intent: Some(accepted_intent),
            session_command: Some(SessionScrubCommand::Preview(accepted_intent)),
            stale: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use media_core::MediaTime;

    use super::*;
    use crate::{SeekMode, SeekTarget};

    fn seek_to_millis(milliseconds: u64) -> SeekRequest {
        SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_millis(milliseconds)),
            mode: SeekMode::Accurate,
        }
    }

    fn scrub_update_intent(generation: ScrubGeneration, milliseconds: u64) -> ScrubUpdateIntent {
        ScrubUpdateIntent::new(generation, seek_to_millis(milliseconds))
    }

    #[test]
    fn begin_scrub_returns_session_begin_and_pause_command() {
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = driver.next_begin_generation();

        let decision = driver.begin_scrub(generation, PlaybackState::Playing);

        assert_eq!(
            decision.session_command,
            SessionScrubCommand::Begin { generation }
        );
        assert_eq!(decision.player_command, PlayerCommand::Pause);
        assert!(driver.is_scrubbing());
    }

    #[test]
    fn drag_update_dispatches_immediate_preview_when_not_throttled() {
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Paused);

        let decision =
            driver.apply_latest_scrub_update_for_drag(scrub_update_intent(generation, 250));

        assert_eq!(
            decision.session_command,
            Some(SessionScrubCommand::Update(scrub_update_intent(
                generation, 250
            )))
        );
        assert_eq!(
            decision.due_preview.map(|preview| preview.intent),
            Some(scrub_update_intent(generation, 250))
        );
        assert!(!decision.stale);
    }

    #[test]
    fn commit_update_suppresses_new_preview_seek() {
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Paused);
        driver.set_last_preview_scrub_seek_at_for_tests(Some(Instant::now()));

        let decision =
            driver.apply_latest_scrub_update_for_commit(scrub_update_intent(generation, 900));

        assert_eq!(
            decision.session_command,
            Some(SessionScrubCommand::Update(scrub_update_intent(
                generation, 900
            )))
        );
        assert_eq!(decision.due_preview, None);
        assert!(!decision.stale);
        assert_eq!(driver.next_preview_scrub_timeout(Instant::now()), None);
    }

    #[test]
    fn stale_update_does_not_replace_current_target_or_queue_preview() {
        let now = Instant::now();
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Paused);
        driver.set_last_preview_scrub_seek_at_for_tests(Some(now));

        let current_decision =
            driver.apply_latest_scrub_update_for_drag(scrub_update_intent(generation, 100));
        let stale_decision =
            driver.apply_latest_scrub_update_for_drag(scrub_update_intent(generation.next(), 250));

        assert_eq!(current_decision.due_preview, None);
        assert!(!current_decision.stale);
        assert_eq!(stale_decision.session_command, None);
        assert_eq!(stale_decision.due_preview, None);
        assert!(stale_decision.stale);
        assert_eq!(driver.latest_scrub_target(), Some(seek_to_millis(100)));
        assert!(driver.next_preview_scrub_timeout(now).is_some());
        assert_eq!(driver.diagnostics().stale_or_ignored_commands, 1);
    }

    #[test]
    fn stale_update_path_counts_diagnostics_once() {
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = ScrubGeneration::default().next();

        let decision =
            driver.apply_latest_scrub_update_for_drag(scrub_update_intent(generation, 100));

        assert_eq!(decision.session_command, None);
        assert_eq!(decision.due_preview, None);
        assert!(decision.stale);
        assert_eq!(driver.latest_scrub_target(), None);
        assert_eq!(driver.diagnostics().stale_or_ignored_commands, 1);
    }

    #[test]
    fn throttled_preview_does_not_survive_new_generation() {
        let now = Instant::now();
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let first_generation = driver.next_begin_generation();
        driver.begin_scrub(first_generation, PlaybackState::Paused);
        driver.set_last_preview_scrub_seek_at_for_tests(Some(now));

        let decision =
            driver.apply_latest_scrub_update_for_drag(scrub_update_intent(first_generation, 100));

        assert_eq!(decision.due_preview, None);
        assert!(driver.next_preview_scrub_timeout(now).is_some());

        let second_generation = first_generation.next();
        driver.begin_scrub(second_generation, PlaybackState::Paused);

        let preview_decision =
            driver.dispatch_due_preview_scrub_seek(now + Duration::from_millis(200));

        assert_eq!(driver.next_preview_scrub_timeout(now), None);
        assert_eq!(preview_decision.intent, None);
        assert_eq!(preview_decision.session_command, None);
        assert!(!preview_decision.stale);
        assert_eq!(driver.diagnostics().stale_or_ignored_commands, 0);
    }

    #[test]
    fn throttled_preview_dispatches_only_current_latest_target() {
        let now = Instant::now();
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Paused);
        driver.set_last_preview_scrub_seek_at_for_tests(Some(now));

        let first_decision =
            driver.apply_latest_scrub_update_for_drag(scrub_update_intent(generation, 100));
        let second_decision =
            driver.apply_latest_scrub_update_for_drag(scrub_update_intent(generation, 250));

        assert_eq!(first_decision.due_preview, None);
        assert_eq!(second_decision.due_preview, None);

        let preview_decision =
            driver.dispatch_due_preview_scrub_seek(now + Duration::from_millis(100));

        assert_eq!(
            preview_decision.session_command,
            Some(SessionScrubCommand::Preview(scrub_update_intent(
                generation, 250
            )))
        );
        assert!(!preview_decision.stale);
        assert_eq!(driver.in_flight_target(), Some(seek_to_millis(250)));
    }

    #[test]
    fn deferred_preview_is_cleared_when_latest_is_already_in_flight() {
        let now = Instant::now();
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Paused);

        let first_decision =
            driver.apply_latest_scrub_update_for_drag(scrub_update_intent(generation, 100));
        let due_preview = first_decision
            .due_preview
            .expect("first preview should be due immediately");
        let preview_decision = driver.preview_due_scrub(due_preview);
        assert_eq!(
            preview_decision.session_command,
            Some(SessionScrubCommand::Preview(scrub_update_intent(
                generation, 100
            )))
        );

        driver.set_last_preview_scrub_seek_at_for_tests(Some(now));
        let second_decision =
            driver.apply_latest_scrub_update_for_drag(scrub_update_intent(generation, 250));
        let repeated_first_decision =
            driver.apply_latest_scrub_update_for_drag(scrub_update_intent(generation, 100));

        assert_eq!(second_decision.due_preview, None);
        assert_eq!(repeated_first_decision.due_preview, None);
        assert_eq!(driver.next_preview_scrub_timeout(now), None);
        assert_eq!(driver.latest_scrub_target(), Some(seek_to_millis(100)));
        assert_eq!(driver.in_flight_target(), Some(seek_to_millis(100)));
    }

    #[test]
    fn stale_preview_generation_is_rejected_before_session_command() {
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let first_generation = driver.next_begin_generation();
        driver.begin_scrub(first_generation, PlaybackState::Paused);
        let second_generation = first_generation.next();
        driver.begin_scrub(second_generation, PlaybackState::Paused);

        let decision = driver.preview_scrub(scrub_update_intent(first_generation, 100));

        assert!(decision.stale);
        assert_eq!(decision.session_command, None);
        assert_eq!(driver.diagnostics().stale_or_ignored_commands, 1);
    }

    #[test]
    fn end_scrub_returns_resume_intent_from_begin_playback_state() {
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Playing);

        let decision = driver.end_scrub(ScrubCommitIntent::new(
            generation,
            crate::ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ));

        assert_eq!(
            decision.session_command,
            Some(SessionScrubCommand::End(ScrubCommitIntent::new(
                generation,
                crate::ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            )))
        );
        assert_eq!(decision.resume_intent, Some(PlaybackResumeIntent::Play));
        assert!(!decision.stale);
    }
}
