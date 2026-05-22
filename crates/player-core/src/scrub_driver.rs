use std::time::{Duration, Instant};

use tracing::debug;

use crate::seek_controller::{
    DeferredPreviewStatus, FinishScrubDecision, FinishScrubRejection, InFlightScrubPreview,
    PlaybackResumeCommand, PlaybackResumeIntent, PreviewDispatchDecision, PreviewDispatchRejection,
    PreviewDispatchState, PreviewQueueDecision, ScrubUpdateAcceptance, ScrubUpdatePreviewIntent,
    ScrubUpdateRejection, SeekController, SeekControllerDiagnostics, SeekScrubInterruptDecision,
};
use crate::{
    PlaybackState, ScrubCommitIntent, ScrubGeneration, ScrubUpdateIntent, SeekRequest,
    SessionScrubCommand,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrubBeginDecision {
    /// Команда для session-side scrub state machine.
    pub session_command: SessionScrubCommand,

    /// Worker-side playback action, который временно глушит playback на время scrub-а.
    pub playback_action: ScrubBeginPlaybackAction,
}

/// Playback side effect, который worker должен выполнить при начале scrub-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubBeginPlaybackAction {
    /// Временно поставить session на паузу, сохранив resume intent внутри driver-а.
    Pause,
}

/// Решение driver-а после scrub update-а на границе worker-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubUpdateDecision {
    /// Update принят controller-ом и должен быть применён к `PlayerSession`.
    Accepted {
        /// Intent, который driver принял как latest scrub target.
        intent: ScrubUpdateIntent,

        /// Команда обновления latest target для `PlayerSession`.
        session_command: SessionScrubCommand,

        /// Due preview intent, который worker должен отправить после session update.
        due_preview: Option<ScrubDuePreview>,
    },

    /// Update отклонён без session-side effects.
    Rejected {
        /// Intent, который не прошёл operation-level проверки.
        intent: ScrubUpdateIntent,

        /// Типизированная причина отказа для logs/tests.
        reason: ScrubUpdateRejection,
    },
}

/// Preview seek, который стал due внутри текущего throttle window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrubDuePreview {
    /// Preview intent, который нужно проверить и отправить после session update.
    pub intent: ScrubUpdateIntent,

    /// Монотонное время, которое станет anchor-ом следующего throttle window.
    pub dispatched_at: Instant,
}

/// Решение driver-а по preview scrub seek-у на границе worker-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubPreviewDecision {
    /// Preview сейчас не нужен: нет pending target-а или throttle window ещё не истёк.
    Idle,

    /// Preview прошёл controller checks и должен быть отправлен в `PlayerSession`.
    Dispatch {
        /// Intent, который остаётся current latest target-ом.
        intent: ScrubUpdateIntent,

        /// Preview-команда для `PlayerSession`.
        session_command: SessionScrubCommand,
    },

    /// Preview отклонён без session-side effects.
    Rejected {
        /// Intent, который не прошёл operation-level проверки.
        intent: ScrubUpdateIntent,

        /// Типизированная причина отказа для logs/tests.
        reason: PreviewDispatchRejection,
    },
}

/// Решение driver-а после завершения interactive scrub на границе worker-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubEndDecision {
    /// Finish принят controller-ом и должен закрыть scrub в `PlayerSession`.
    Accepted {
        /// Commit intent, который закрыл active operation.
        intent: ScrubCommitIntent,

        /// Команда финального scrub commit-а для `PlayerSession`.
        session_command: SessionScrubCommand,

        /// Playback intent, сохранённый на `BeginScrub`.
        resume_intent: PlaybackResumeIntent,
    },

    /// Finish отклонён без очистки active operation и без session-side effects.
    Rejected {
        /// Commit intent, который не прошёл operation-level проверки.
        intent: ScrubCommitIntent,

        /// Типизированная причина отказа для logs/tests.
        reason: FinishScrubRejection,
    },
}

/// Результат принудительного interrupt-а interactive scrub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubInterruptDecision {
    /// Active scrub был прерван, worker должен выполнить boundary side effects.
    Interrupted,

    /// Active scrub отсутствовал, session-side interrupt work не нужен.
    Idle,
}

/// Worker-side coordinator для interactive scrub.
///
/// Driver владеет только scrub state и принимает решения, но не знает о каналах,
/// `PlayerSession`, snapshot publishing и render bridge.
///
/// Preview state machine разделён по уровням владения:
/// latest user target и requested/in-flight preview принадлежат `SeekController`;
/// visible frame, completed target preview, expired/failed preview принадлежат
/// `PlayerSession`, потому что только session видит decoded frames и seek gates.
#[derive(Debug, Clone)]
pub(crate) struct ScrubDriver {
    /// Generation-aware state machine для seek/scrub intent-ов.
    seek_controller: SeekController,

    /// Последний момент, когда worker запускал live preview seek.
    last_preview_scrub_seek_at: Option<Instant>,

    /// Минимальный интервал между live preview seek-ами во время scrub.
    live_scrub_preview_interval: Duration,

    /// Возраст in-flight preview, после которого latest target может заменить старый preview.
    stale_in_flight_preview_replace_after: Duration,
}

impl ScrubDriver {
    /// Создаёт scrub driver с production throttle interval из worker config.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn new(live_scrub_preview_interval: Duration) -> Self {
        Self::with_preview_policy(live_scrub_preview_interval, live_scrub_preview_interval)
    }

    /// Создаёт scrub driver с явной preview throttle/replacement policy.
    #[must_use]
    pub(crate) fn with_preview_policy(
        live_scrub_preview_interval: Duration,
        stale_in_flight_preview_replace_after: Duration,
    ) -> Self {
        Self {
            seek_controller: SeekController::new(),
            last_preview_scrub_seek_at: None,
            live_scrub_preview_interval,
            stale_in_flight_preview_replace_after,
        }
    }

    /// Test helper: возвращает generation для следующего `BeginScrub`.
    #[must_use]
    #[cfg(test)]
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

    /// Возвращает preview intent, который сейчас считается отправленным в session.
    #[must_use]
    pub(crate) fn in_flight_preview(&self) -> Option<InFlightScrubPreview> {
        self.seek_controller.in_flight_preview()
    }

    /// Возвращает diagnostics counters seek-controller-а.
    #[must_use]
    pub(crate) fn diagnostics(&self) -> SeekControllerDiagnostics {
        self.seek_controller.diagnostics()
    }

    /// Возвращает `true`, если сейчас активен interactive scrub.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn is_scrubbing(&self) -> bool {
        self.seek_controller.is_scrubbing()
    }

    /// Возвращает сохранённый resume intent для worker-level tests.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn resume_intent(&self) -> PlaybackResumeIntent {
        self.seek_controller.resume_intent()
    }

    /// Обрабатывает типизированный Play/Pause/Toggle intent во время scrub без изменения `PlayerSession`.
    pub(crate) fn consume_resume_intent_command(&mut self, command: PlaybackResumeCommand) -> bool {
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
            playback_action: ScrubBeginPlaybackAction::Pause,
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

    /// Test helper: применяет explicit scrub update без sender-side coalescing.
    #[cfg(test)]
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

        let (accepted_intent, preview_decision) = match update_decision {
            ScrubUpdateAcceptance::Accepted { intent, preview } => (intent, preview),
            ScrubUpdateAcceptance::Rejected { intent, reason } => {
                return ScrubUpdateDecision::Rejected { intent, reason };
            }
        };

        let due_preview = match preview_decision {
            PreviewQueueDecision::Queued { .. } => self.take_due_preview_scrub_seek(Instant::now()),
            PreviewQueueDecision::AlreadyInFlight { .. }
            | PreviewQueueDecision::SuppressedForCommit { .. } => None,
        };

        ScrubUpdateDecision::Accepted {
            intent: accepted_intent,
            session_command: SessionScrubCommand::Update(accepted_intent),
            due_preview,
        }
    }

    /// Готовит explicit preview command, если generation и latest target ещё актуальны.
    pub(crate) fn preview_scrub(&mut self, intent: ScrubUpdateIntent) -> ScrubPreviewDecision {
        self.prepare_preview_scrub(intent, None)
    }

    /// Отправляет отложенный preview seek, когда прошёл throttle interval live scrub-а.
    pub(crate) fn dispatch_due_preview_scrub_seek(&mut self, now: Instant) -> ScrubPreviewDecision {
        let Some(due_preview) = self.take_due_preview_scrub_seek(now) else {
            return ScrubPreviewDecision::Idle;
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
        let DeferredPreviewStatus::Pending { intent } =
            self.seek_controller.deferred_preview_status()
        else {
            return None;
        };

        if !self.preview_scrub_interval_elapsed(now) {
            return None;
        }

        if self.preview_replacement_delay(intent, now) != Some(Duration::ZERO) {
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
        let DeferredPreviewStatus::Pending { intent } =
            self.seek_controller.deferred_preview_status()
        else {
            return None;
        };

        let interval_delay = match self.last_preview_scrub_seek_at {
            Some(last_seek_at) => {
                let elapsed_since_preview = now.saturating_duration_since(last_seek_at);
                self.live_scrub_preview_interval
                    .saturating_sub(elapsed_since_preview)
            }
            None => Duration::ZERO,
        };

        let replacement_delay = self.preview_replacement_delay(intent, now)?;
        Some(interval_delay.max(replacement_delay))
    }

    /// Завершает scrub и возвращает final session command вместе с resume intent-ом.
    pub(crate) fn end_scrub(&mut self, intent: ScrubCommitIntent) -> ScrubEndDecision {
        match self.seek_controller.finish_scrub(intent) {
            FinishScrubDecision::Accepted {
                intent,
                resume_intent,
            } => ScrubEndDecision::Accepted {
                intent,
                session_command: SessionScrubCommand::End(intent),
                resume_intent,
            },
            FinishScrubDecision::Rejected { intent, reason } => {
                ScrubEndDecision::Rejected { intent, reason }
            }
        }
    }

    /// Прерывает active scrub для Stop/Open/Shutdown/load boundary команд.
    pub(crate) fn interrupt_scrub(&mut self) -> ScrubInterruptDecision {
        let interrupt_decision = self.seek_controller.interrupt_active_scrub();
        self.reset_preview_scrub_state();
        match interrupt_decision {
            SeekScrubInterruptDecision::Interrupted => ScrubInterruptDecision::Interrupted,
            SeekScrubInterruptDecision::Idle => ScrubInterruptDecision::Idle,
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

    /// Очищает in-flight preview после terminal state на session side.
    pub(crate) fn clear_in_flight_preview(&mut self, intent: ScrubUpdateIntent) -> bool {
        self.seek_controller
            .clear_in_flight_preview(intent)
            .cleared()
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

    /// Возвращает задержку до момента, когда in-flight preview можно заменить новым target-ом.
    fn preview_replacement_delay(
        &self,
        intent: ScrubUpdateIntent,
        now: Instant,
    ) -> Option<Duration> {
        self.seek_controller.in_flight_replacement_delay(
            intent,
            now,
            self.stale_in_flight_preview_replace_after,
        )
    }

    /// Помечает preview seek как отправленный и готовит session command.
    fn prepare_preview_scrub(
        &mut self,
        intent: ScrubUpdateIntent,
        dispatched_at: Option<Instant>,
    ) -> ScrubPreviewDecision {
        let effective_dispatched_at = dispatched_at.unwrap_or_else(Instant::now);
        let preview_decision = self
            .seek_controller
            .mark_preview_seek_dispatched(intent, effective_dispatched_at);

        let accepted_intent = match preview_decision {
            PreviewDispatchDecision::Dispatch { intent, state } => {
                self.log_preview_dispatch_state(intent, state, effective_dispatched_at);
                intent
            }
            PreviewDispatchDecision::Rejected { intent, reason } => {
                return ScrubPreviewDecision::Rejected { intent, reason };
            }
        };

        if let Some(dispatched_at) = dispatched_at {
            self.last_preview_scrub_seek_at = Some(dispatched_at);
        }

        ScrubPreviewDecision::Dispatch {
            intent: accepted_intent,
            session_command: SessionScrubCommand::Preview(accepted_intent),
        }
    }

    /// Логирует только policy-level replacement, не раскрывая session/pipeline internals.
    fn log_preview_dispatch_state(
        &self,
        intent: ScrubUpdateIntent,
        state: PreviewDispatchState,
        dispatched_at: Instant,
    ) {
        let PreviewDispatchState::ReplacedInFlight { replaced } = state else {
            return;
        };

        debug!(
            generation = intent.generation.as_u64(),
            request = ?intent.request,
            replaced_request = ?replaced.intent.request,
            replaced_age_ms = dispatched_at
                .saturating_duration_since(replaced.dispatched_at)
                .as_millis(),
            "Live scrub preview replaced in-flight transaction"
        );
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

    fn accepted_update_parts(
        decision: ScrubUpdateDecision,
    ) -> (
        ScrubUpdateIntent,
        SessionScrubCommand,
        Option<ScrubDuePreview>,
    ) {
        match decision {
            ScrubUpdateDecision::Accepted {
                intent,
                session_command,
                due_preview,
            } => (intent, session_command, due_preview),
            other_decision => {
                panic!("expected accepted scrub update decision, got {other_decision:?}")
            }
        }
    }

    fn rejected_update_parts(
        decision: ScrubUpdateDecision,
    ) -> (ScrubUpdateIntent, ScrubUpdateRejection) {
        match decision {
            ScrubUpdateDecision::Rejected { intent, reason } => (intent, reason),
            other_decision => {
                panic!("expected rejected scrub update decision, got {other_decision:?}")
            }
        }
    }

    fn dispatched_preview_parts(
        decision: ScrubPreviewDecision,
    ) -> (ScrubUpdateIntent, SessionScrubCommand) {
        match decision {
            ScrubPreviewDecision::Dispatch {
                intent,
                session_command,
            } => (intent, session_command),
            other_decision => {
                panic!("expected dispatched scrub preview decision, got {other_decision:?}")
            }
        }
    }

    fn rejected_preview_parts(
        decision: ScrubPreviewDecision,
    ) -> (ScrubUpdateIntent, PreviewDispatchRejection) {
        match decision {
            ScrubPreviewDecision::Rejected { intent, reason } => (intent, reason),
            other_decision => {
                panic!("expected rejected scrub preview decision, got {other_decision:?}")
            }
        }
    }

    fn accepted_end_parts(
        decision: ScrubEndDecision,
    ) -> (ScrubCommitIntent, SessionScrubCommand, PlaybackResumeIntent) {
        match decision {
            ScrubEndDecision::Accepted {
                intent,
                session_command,
                resume_intent,
            } => (intent, session_command, resume_intent),
            other_decision => {
                panic!("expected accepted scrub end decision, got {other_decision:?}")
            }
        }
    }

    fn rejected_end_parts(decision: ScrubEndDecision) -> (ScrubCommitIntent, FinishScrubRejection) {
        match decision {
            ScrubEndDecision::Rejected { intent, reason } => (intent, reason),
            other_decision => {
                panic!("expected rejected scrub end decision, got {other_decision:?}")
            }
        }
    }

    #[test]
    fn begin_scrub_returns_session_begin_and_internal_pause_action() {
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = driver.next_begin_generation();

        let decision = driver.begin_scrub(generation, PlaybackState::Playing);

        assert_eq!(
            decision.session_command,
            SessionScrubCommand::Begin { generation }
        );
        assert_eq!(decision.playback_action, ScrubBeginPlaybackAction::Pause);
        assert!(driver.is_scrubbing());
    }

    #[test]
    fn resume_command_during_scrub_updates_driver_resume_intent() {
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = driver.next_begin_generation();

        driver.begin_scrub(generation, PlaybackState::Playing);

        assert!(driver.consume_resume_intent_command(PlaybackResumeCommand::Pause));
        assert_eq!(driver.resume_intent(), PlaybackResumeIntent::Pause);
        assert!(driver.consume_resume_intent_command(PlaybackResumeCommand::Play));
        assert_eq!(driver.resume_intent(), PlaybackResumeIntent::Play);
        assert!(driver.consume_resume_intent_command(PlaybackResumeCommand::TogglePlayback));
        assert_eq!(driver.resume_intent(), PlaybackResumeIntent::Pause);
    }

    #[test]
    fn drag_update_dispatches_immediate_preview_when_not_throttled() {
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Paused);

        let decision =
            driver.apply_latest_scrub_update_for_drag(scrub_update_intent(generation, 250));
        let expected_intent = scrub_update_intent(generation, 250);
        let (accepted_intent, session_command, due_preview) = accepted_update_parts(decision);

        assert_eq!(accepted_intent, expected_intent);
        assert_eq!(
            session_command,
            SessionScrubCommand::Update(expected_intent)
        );
        assert_eq!(
            due_preview.map(|preview| preview.intent),
            Some(expected_intent)
        );
    }

    #[test]
    fn commit_update_suppresses_new_preview_seek() {
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Paused);
        driver.set_last_preview_scrub_seek_at_for_tests(Some(Instant::now()));

        let decision =
            driver.apply_latest_scrub_update_for_commit(scrub_update_intent(generation, 900));
        let expected_intent = scrub_update_intent(generation, 900);
        let (accepted_intent, session_command, due_preview) = accepted_update_parts(decision);

        assert_eq!(accepted_intent, expected_intent);
        assert_eq!(
            session_command,
            SessionScrubCommand::Update(expected_intent)
        );
        assert_eq!(due_preview, None);
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
        let (_current_intent, _session_command, current_due_preview) =
            accepted_update_parts(current_decision);
        let (rejected_intent, rejected_reason) = rejected_update_parts(stale_decision);

        assert_eq!(current_due_preview, None);
        assert_eq!(rejected_intent, scrub_update_intent(generation.next(), 250));
        assert_eq!(rejected_reason, ScrubUpdateRejection::StaleGeneration);
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
        let (rejected_intent, rejected_reason) = rejected_update_parts(decision);

        assert_eq!(rejected_intent, scrub_update_intent(generation, 100));
        assert_eq!(rejected_reason, ScrubUpdateRejection::Inactive);
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
        let (_accepted_intent, _session_command, due_preview) = accepted_update_parts(decision);

        assert_eq!(due_preview, None);
        assert!(driver.next_preview_scrub_timeout(now).is_some());

        let second_generation = first_generation.next();
        driver.begin_scrub(second_generation, PlaybackState::Paused);

        let preview_decision =
            driver.dispatch_due_preview_scrub_seek(now + Duration::from_millis(200));

        assert_eq!(driver.next_preview_scrub_timeout(now), None);
        assert_eq!(preview_decision, ScrubPreviewDecision::Idle);
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
        let (_first_intent, _first_command, first_due_preview) =
            accepted_update_parts(first_decision);
        let (_second_intent, _second_command, second_due_preview) =
            accepted_update_parts(second_decision);

        assert_eq!(first_due_preview, None);
        assert_eq!(second_due_preview, None);

        let preview_decision =
            driver.dispatch_due_preview_scrub_seek(now + Duration::from_millis(100));
        let (preview_intent, session_command) = dispatched_preview_parts(preview_decision);
        let expected_intent = scrub_update_intent(generation, 250);

        assert_eq!(preview_intent, expected_intent);
        assert_eq!(
            session_command,
            SessionScrubCommand::Preview(expected_intent)
        );
        assert_eq!(driver.in_flight_target(), Some(seek_to_millis(250)));
    }

    #[test]
    fn slow_drag_at_live_interval_dispatches_multiple_previews() {
        let now = Instant::now() + Duration::from_secs(60);
        let mut driver =
            ScrubDriver::with_preview_policy(Duration::from_millis(33), Duration::from_millis(33));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Paused);
        let first_intent = scrub_update_intent(generation, 100);
        let second_intent = scrub_update_intent(generation, 133);
        let third_intent = scrub_update_intent(generation, 166);

        let (_first_accepted, _first_command, first_due_preview) =
            accepted_update_parts(driver.apply_latest_scrub_update_for_drag(first_intent));
        assert_eq!(
            first_due_preview.map(|preview| preview.intent),
            Some(first_intent)
        );
        let first_preview_decision = driver.preview_due_scrub(ScrubDuePreview {
            intent: first_intent,
            dispatched_at: now,
        });
        assert_eq!(
            dispatched_preview_parts(first_preview_decision).0,
            first_intent
        );
        assert!(driver.clear_in_flight_preview(first_intent));

        let (_second_accepted, _second_command, second_due_preview) =
            accepted_update_parts(driver.apply_latest_scrub_update_for_drag(second_intent));
        assert_eq!(second_due_preview, None);
        let second_preview_decision =
            driver.dispatch_due_preview_scrub_seek(now + Duration::from_millis(33));
        assert_eq!(
            dispatched_preview_parts(second_preview_decision).0,
            second_intent
        );
        assert!(driver.clear_in_flight_preview(second_intent));

        let (_third_accepted, _third_command, third_due_preview) =
            accepted_update_parts(driver.apply_latest_scrub_update_for_drag(third_intent));
        assert_eq!(third_due_preview, None);
        let third_preview_decision =
            driver.dispatch_due_preview_scrub_seek(now + Duration::from_millis(66));
        assert_eq!(
            dispatched_preview_parts(third_preview_decision).0,
            third_intent
        );

        assert_eq!(driver.in_flight_target(), Some(seek_to_millis(166)));
        assert_eq!(driver.diagnostics().replaced_in_flight_previews, 0);
    }

    #[test]
    fn fast_drag_keeps_only_latest_deferred_preview() {
        let now = Instant::now() + Duration::from_secs(60);
        let mut driver =
            ScrubDriver::with_preview_policy(Duration::from_millis(33), Duration::from_millis(33));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Paused);
        driver.set_last_preview_scrub_seek_at_for_tests(Some(now));

        for target_millis in 0..1000 {
            let update_decision = driver
                .apply_latest_scrub_update_for_drag(scrub_update_intent(generation, target_millis));
            let (_accepted_intent, _session_command, due_preview) =
                accepted_update_parts(update_decision);
            assert_eq!(due_preview, None);
        }

        let preview_decision =
            driver.dispatch_due_preview_scrub_seek(now + Duration::from_millis(33));
        let (preview_intent, _session_command) = dispatched_preview_parts(preview_decision);

        assert_eq!(preview_intent, scrub_update_intent(generation, 999));
        assert_eq!(driver.in_flight_target(), Some(seek_to_millis(999)));
        assert_eq!(driver.diagnostics().replaced_in_flight_previews, 0);
    }

    #[test]
    fn changed_target_waits_until_in_flight_preview_is_replaceable() {
        let now = Instant::now();
        let mut driver =
            ScrubDriver::with_preview_policy(Duration::from_millis(16), Duration::from_millis(100));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Paused);
        let first_intent = scrub_update_intent(generation, 100);
        let second_intent = scrub_update_intent(generation, 250);

        let (_first_accepted, _first_command, _first_due_preview) =
            accepted_update_parts(driver.apply_latest_scrub_update_for_drag(first_intent));
        let first_preview_decision = driver.preview_due_scrub(ScrubDuePreview {
            intent: first_intent,
            dispatched_at: now,
        });
        assert!(matches!(
            first_preview_decision,
            ScrubPreviewDecision::Dispatch { .. }
        ));

        let (_second_accepted, _second_command, second_due_preview) =
            accepted_update_parts(driver.apply_latest_scrub_update_for_drag(second_intent));

        assert_eq!(second_due_preview, None);
        assert_eq!(
            driver.dispatch_due_preview_scrub_seek(now + Duration::from_millis(16)),
            ScrubPreviewDecision::Idle
        );
        assert_eq!(
            driver.next_preview_scrub_timeout(now + Duration::from_millis(16)),
            Some(Duration::from_millis(84))
        );

        let replacement_decision =
            driver.dispatch_due_preview_scrub_seek(now + Duration::from_millis(100));
        let (preview_intent, _session_command) = dispatched_preview_parts(replacement_decision);

        assert_eq!(preview_intent, second_intent);
        assert_eq!(driver.in_flight_target(), Some(seek_to_millis(250)));
        assert_eq!(driver.diagnostics().replaced_in_flight_previews, 1);
    }

    #[test]
    fn completed_preview_allows_next_slow_drag_preview_after_interval() {
        let now = Instant::now();
        let mut driver =
            ScrubDriver::with_preview_policy(Duration::from_millis(16), Duration::from_millis(100));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Paused);
        let first_intent = scrub_update_intent(generation, 100);
        let second_intent = scrub_update_intent(generation, 133);

        let (_first_accepted, _first_command, _first_due_preview) =
            accepted_update_parts(driver.apply_latest_scrub_update_for_drag(first_intent));
        let first_preview_decision = driver.preview_due_scrub(ScrubDuePreview {
            intent: first_intent,
            dispatched_at: now,
        });
        assert!(matches!(
            first_preview_decision,
            ScrubPreviewDecision::Dispatch { .. }
        ));
        assert!(driver.clear_in_flight_preview(first_intent));

        let (_second_accepted, _second_command, second_due_preview) =
            accepted_update_parts(driver.apply_latest_scrub_update_for_drag(second_intent));

        assert_eq!(second_due_preview, None);
        let next_preview_decision =
            driver.dispatch_due_preview_scrub_seek(now + Duration::from_millis(16));
        let (preview_intent, _session_command) = dispatched_preview_parts(next_preview_decision);

        assert_eq!(preview_intent, second_intent);
        assert_eq!(driver.in_flight_target(), Some(seek_to_millis(133)));
    }

    #[test]
    fn deferred_preview_is_cleared_when_latest_is_already_in_flight() {
        let now = Instant::now();
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Paused);

        let first_decision =
            driver.apply_latest_scrub_update_for_drag(scrub_update_intent(generation, 100));
        let (_first_intent, _first_command, first_due_preview) =
            accepted_update_parts(first_decision);
        let due_preview = first_due_preview.expect("first preview should be due immediately");
        let preview_decision = driver.preview_due_scrub(due_preview);
        let (preview_intent, session_command) = dispatched_preview_parts(preview_decision);
        let first_intent = scrub_update_intent(generation, 100);
        assert_eq!(preview_intent, first_intent);
        assert_eq!(session_command, SessionScrubCommand::Preview(first_intent));

        driver.set_last_preview_scrub_seek_at_for_tests(Some(now));
        let second_decision =
            driver.apply_latest_scrub_update_for_drag(scrub_update_intent(generation, 250));
        let repeated_first_decision =
            driver.apply_latest_scrub_update_for_drag(scrub_update_intent(generation, 100));
        let (_second_intent, _second_command, second_due_preview) =
            accepted_update_parts(second_decision);
        let (_repeated_intent, _repeated_command, repeated_due_preview) =
            accepted_update_parts(repeated_first_decision);

        assert_eq!(second_due_preview, None);
        assert_eq!(repeated_due_preview, None);
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
        let (rejected_intent, rejected_reason) = rejected_preview_parts(decision);

        assert_eq!(rejected_intent, scrub_update_intent(first_generation, 100));
        assert_eq!(rejected_reason, PreviewDispatchRejection::StaleGeneration);
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
        let (accepted_intent, session_command, resume_intent) = accepted_end_parts(decision);
        let expected_intent = ScrubCommitIntent::new(
            generation,
            crate::ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        );

        assert_eq!(accepted_intent, expected_intent);
        assert_eq!(session_command, SessionScrubCommand::End(expected_intent));
        assert_eq!(resume_intent, PlaybackResumeIntent::Play);
    }

    #[test]
    fn stale_end_scrub_returns_rejected_without_clearing_active_operation() {
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Playing);

        let stale_intent = ScrubCommitIntent::new(
            generation.next(),
            crate::ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        );
        let decision = driver.end_scrub(stale_intent);
        let (rejected_intent, rejected_reason) = rejected_end_parts(decision);

        assert_eq!(rejected_intent, stale_intent);
        assert_eq!(rejected_reason, FinishScrubRejection::StaleGeneration);
        assert!(driver.is_scrubbing());
        assert_eq!(driver.current_generation(), generation);
        assert_eq!(driver.diagnostics().stale_or_ignored_commands, 1);
    }

    #[test]
    fn preview_rejects_not_current_latest_separately_from_stale_generation() {
        let mut driver = ScrubDriver::new(Duration::from_millis(100));
        let generation = driver.next_begin_generation();
        driver.begin_scrub(generation, PlaybackState::Paused);

        let first_intent = scrub_update_intent(generation, 100);
        let second_intent = scrub_update_intent(generation, 250);
        let (_first_accepted, _first_command, first_due_preview) =
            accepted_update_parts(driver.apply_latest_scrub_update_for_drag(first_intent));

        assert!(first_due_preview.is_some());
        let (_second_accepted, _second_command, _second_due_preview) =
            accepted_update_parts(driver.apply_latest_scrub_update_for_drag(second_intent));

        let decision = driver.preview_scrub(first_intent);
        let (rejected_intent, rejected_reason) = rejected_preview_parts(decision);

        assert_eq!(rejected_intent, first_intent);
        assert_eq!(rejected_reason, PreviewDispatchRejection::NotCurrentLatest);
        assert_eq!(driver.diagnostics().stale_or_ignored_commands, 1);
    }
}
