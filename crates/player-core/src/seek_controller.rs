use crate::{
    PlaybackState, PlayerCommand, ScrubCommitIntent, ScrubGeneration, ScrubUpdateIntent,
    SeekRequest,
};

/// Режим seek/scrub state machine внутри playback worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekControllerMode {
    /// Нет активной seek/scrub операции.
    Idle,

    /// UI ведёт interactive scrub, а worker хранит последнюю цель.
    Scrubbing,
}

impl Default for SeekControllerMode {
    /// Новый controller стартует без активной операции.
    fn default() -> Self {
        Self::Idle
    }
}

/// Намерение возобновления playback после завершения scrub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackResumeIntent {
    /// Вернуться к paused-состоянию.
    Pause,

    /// Вернуться к playing-состоянию.
    Play,
}

impl PlaybackResumeIntent {
    /// Строит resume intent из состояния session на момент `BeginScrub`.
    #[must_use]
    pub const fn from_playback_state(playback_state: PlaybackState) -> Self {
        match playback_state {
            PlaybackState::Playing
            | PlaybackState::Buffering
            | PlaybackState::Seeking
            | PlaybackState::Draining => Self::Play,
            PlaybackState::Idle
            | PlaybackState::Opening
            | PlaybackState::Paused
            | PlaybackState::Ended
            | PlaybackState::Stopped
            | PlaybackState::Failed => Self::Pause,
        }
    }

    /// Возвращает противоположное намерение для `TogglePlayback` во время scrub.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Pause => Self::Play,
            Self::Play => Self::Pause,
        }
    }
}

/// Диагностические счётчики seek-controller-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeekControllerDiagnostics {
    /// Сколько внешних seek/scrub команд было отброшено как stale.
    pub stale_or_ignored_commands: u64,

    /// Сколько активных scrub операций было отменено interrupt-командой.
    pub cancelled_operations: u64,
}

/// Skeleton state machine для будущего live seek и scrub commit pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeekController {
    /// Поколение seek/scrub операции; увеличивается при новом scrub или cancel.
    generation_id: ScrubGeneration,

    /// Текущий режим controller-а.
    current_mode: SeekControllerMode,

    /// Последняя цель, полученная от interactive scrub.
    latest_scrub_target: Option<SeekRequest>,

    /// Цель, которую worker уже передал в session/scheduler boundary.
    in_flight_target: Option<SeekRequest>,

    /// Playback-состояние, к которому нужно вернуться после `EndScrub`.
    resume_intent: PlaybackResumeIntent,

    /// Счётчики для тестов и будущей diagnostics panel.
    diagnostics: SeekControllerDiagnostics,
}

impl SeekController {
    /// Создаёт controller без активной seek/scrub операции.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Возвращает id текущего поколения операции.
    #[must_use]
    pub const fn generation_id(&self) -> ScrubGeneration {
        self.generation_id
    }

    /// Возвращает текущий режим controller-а.
    #[must_use]
    pub const fn current_mode(&self) -> SeekControllerMode {
        self.current_mode
    }

    /// Возвращает последнюю scrub-цель, если scrub активен.
    #[must_use]
    pub const fn latest_scrub_target(&self) -> Option<SeekRequest> {
        self.latest_scrub_target
    }

    /// Возвращает цель, которая уже была отдана в обработку.
    #[must_use]
    pub const fn in_flight_target(&self) -> Option<SeekRequest> {
        self.in_flight_target
    }

    /// Возвращает намерение возобновления после scrub.
    #[must_use]
    pub const fn resume_intent(&self) -> PlaybackResumeIntent {
        self.resume_intent
    }

    /// Возвращает текущие diagnostics counters.
    #[must_use]
    pub const fn diagnostics(&self) -> SeekControllerDiagnostics {
        self.diagnostics
    }

    /// Возвращает `true`, если сейчас активен interactive scrub.
    #[must_use]
    pub const fn is_scrubbing(&self) -> bool {
        matches!(self.current_mode, SeekControllerMode::Scrubbing)
    }

    /// Начинает новый scrub и запоминает playback intent для commit-а.
    pub fn begin_scrub(&mut self, playback_state: PlaybackState) -> ScrubGeneration {
        let generation = self.generation_id.next();
        self.begin_scrub_with_generation(generation, playback_state);
        generation
    }

    /// Начинает scrub с generation, который уже выдал worker command boundary.
    pub(crate) fn begin_scrub_with_generation(
        &mut self,
        generation: ScrubGeneration,
        playback_state: PlaybackState,
    ) {
        self.generation_id = generation;
        self.current_mode = SeekControllerMode::Scrubbing;
        self.latest_scrub_target = None;
        self.in_flight_target = None;
        self.resume_intent = PlaybackResumeIntent::from_playback_state(playback_state);
    }

    /// Запоминает latest scrub target без объявления seek transaction-а in-flight.
    pub(crate) fn accept_scrub_update(&mut self, intent: ScrubUpdateIntent) -> bool {
        if !self.intent_matches_active_scrub(intent.generation) {
            self.count_stale_or_ignored_command();
            return false;
        }

        self.latest_scrub_target = Some(intent.request);
        true
    }

    /// Помечает preview seek как реально отправленный за worker/session boundary.
    pub(crate) fn mark_preview_seek_dispatched(&mut self, intent: ScrubUpdateIntent) -> bool {
        if !self.intent_matches_active_scrub(intent.generation)
            || self.latest_scrub_target != Some(intent.request)
        {
            self.count_stale_or_ignored_command();
            return false;
        }

        self.in_flight_target = Some(intent.request);
        true
    }

    /// Обрабатывает Play/Pause/Toggle во время scrub без изменения session state.
    pub fn consume_resume_intent_command(&mut self, command: &PlayerCommand) -> bool {
        if !self.is_scrubbing() {
            return false;
        }

        match command {
            PlayerCommand::Play => {
                self.resume_intent = PlaybackResumeIntent::Play;
                true
            }
            PlayerCommand::Pause => {
                self.resume_intent = PlaybackResumeIntent::Pause;
                true
            }
            PlayerCommand::TogglePlayback => {
                self.resume_intent = self.resume_intent.toggled();
                true
            }
            _ => false,
        }
    }

    /// Отмечает внешний seek как stale, если scrub уже активен.
    pub fn should_ignore_external_seek(&mut self) -> bool {
        if !self.is_scrubbing() {
            return false;
        }

        self.count_stale_or_ignored_command();
        true
    }

    /// Завершает scrub и возвращает сохранённый resume intent.
    pub(crate) fn finish_scrub(
        &mut self,
        intent: ScrubCommitIntent,
    ) -> Option<PlaybackResumeIntent> {
        if !self.intent_matches_active_scrub(intent.generation) {
            self.count_stale_or_ignored_command();
            return None;
        }

        let resume_intent = self.resume_intent;
        self.in_flight_target = self.latest_scrub_target;
        self.current_mode = SeekControllerMode::Idle;
        self.latest_scrub_target = None;
        Some(resume_intent)
    }

    /// Прерывает scrub из-за Stop/Open/Shutdown и очищает pending цели.
    pub fn interrupt_scrub(&mut self) {
        if self.is_scrubbing() {
            self.diagnostics.cancelled_operations =
                self.diagnostics.cancelled_operations.saturating_add(1);
        }

        self.generation_id = self.generation_id.next();
        self.current_mode = SeekControllerMode::Idle;
        self.latest_scrub_target = None;
        self.in_flight_target = None;
        self.resume_intent = PlaybackResumeIntent::Pause;
    }

    /// Проверяет, что command относится к текущему live scrub.
    fn intent_matches_active_scrub(&self, generation: ScrubGeneration) -> bool {
        self.is_scrubbing() && self.generation_id == generation
    }

    /// Единая точка инкремента diagnostics для stale scrub intent-ов.
    fn count_stale_or_ignored_command(&mut self) {
        self.diagnostics.stale_or_ignored_commands =
            self.diagnostics.stale_or_ignored_commands.saturating_add(1);
    }
}

impl Default for SeekController {
    /// Возвращает начальный controller state без hidden runtime side effects.
    fn default() -> Self {
        Self {
            generation_id: ScrubGeneration::default(),
            current_mode: SeekControllerMode::Idle,
            latest_scrub_target: None,
            in_flight_target: None,
            resume_intent: PlaybackResumeIntent::Pause,
            diagnostics: SeekControllerDiagnostics::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use media_core::MediaTime;

    use super::*;
    use crate::{SeekMode, SeekTarget};

    fn absolute_seek_request(seconds: u64) -> SeekRequest {
        SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_secs(seconds)),
            mode: SeekMode::Accurate,
        }
    }

    #[test]
    fn begin_scrub_creates_new_generation_and_resume_intent() {
        let mut controller = SeekController::new();

        controller.begin_scrub(PlaybackState::Playing);

        assert_eq!(controller.generation_id().as_u64(), 1);
        assert_eq!(controller.current_mode(), SeekControllerMode::Scrubbing);
        assert_eq!(controller.resume_intent(), PlaybackResumeIntent::Play);
    }

    #[test]
    fn inactive_scrub_update_is_counted_as_stale() {
        let mut controller = SeekController::new();

        let accepted = controller.accept_scrub_update(ScrubUpdateIntent::new(
            controller.generation_id(),
            absolute_seek_request(7),
        ));

        assert!(!accepted);
        assert_eq!(controller.diagnostics().stale_or_ignored_commands, 1);
    }

    #[test]
    fn play_pause_during_scrub_only_updates_resume_intent() {
        let mut controller = SeekController::new();
        controller.begin_scrub(PlaybackState::Playing);

        assert!(controller.consume_resume_intent_command(&PlayerCommand::Pause));
        assert_eq!(controller.resume_intent(), PlaybackResumeIntent::Pause);
        assert!(controller.consume_resume_intent_command(&PlayerCommand::Play));
        assert_eq!(controller.resume_intent(), PlaybackResumeIntent::Play);
        assert!(controller.consume_resume_intent_command(&PlayerCommand::TogglePlayback));
        assert_eq!(controller.resume_intent(), PlaybackResumeIntent::Pause);
    }

    #[test]
    fn interrupt_scrub_clears_targets_and_counts_cancel() {
        let mut controller = SeekController::new();
        controller.begin_scrub(PlaybackState::Paused);
        controller.accept_scrub_update(ScrubUpdateIntent::new(
            controller.generation_id(),
            absolute_seek_request(3),
        ));

        controller.interrupt_scrub();

        assert_eq!(controller.current_mode(), SeekControllerMode::Idle);
        assert_eq!(controller.latest_scrub_target(), None);
        assert_eq!(controller.in_flight_target(), None);
        assert_eq!(controller.diagnostics().cancelled_operations, 1);
    }

    #[test]
    fn relative_seek_request_keeps_diagnostic_shape() {
        let request = SeekRequest {
            target: SeekTarget::Relative(Duration::from_secs(5)),
            mode: SeekMode::KeyframeBefore,
        };
        let mut controller = SeekController::new();
        controller.begin_scrub(PlaybackState::Paused);

        assert!(
            controller
                .accept_scrub_update(ScrubUpdateIntent::new(controller.generation_id(), request,))
        );
        assert_eq!(controller.latest_scrub_target(), Some(request));
    }

    #[test]
    fn scrub_update_does_not_mark_target_in_flight() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let request = absolute_seek_request(9);

        assert!(controller.accept_scrub_update(ScrubUpdateIntent::new(generation, request,)));

        assert_eq!(controller.latest_scrub_target(), Some(request));
        assert_eq!(controller.in_flight_target(), None);
    }

    #[test]
    fn preview_dispatch_marks_current_latest_target_in_flight() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let request = absolute_seek_request(9);
        let intent = ScrubUpdateIntent::new(generation, request);

        assert!(controller.accept_scrub_update(intent));
        assert!(controller.mark_preview_seek_dispatched(intent));

        assert_eq!(controller.in_flight_target(), Some(request));
    }

    #[test]
    fn stale_preview_generation_is_counted_and_ignored() {
        let mut controller = SeekController::new();
        let first_generation = controller.begin_scrub(PlaybackState::Paused);
        let stale_request = absolute_seek_request(4);
        controller.begin_scrub(PlaybackState::Paused);

        let dispatched = controller
            .mark_preview_seek_dispatched(ScrubUpdateIntent::new(first_generation, stale_request));

        assert!(!dispatched);
        assert_eq!(controller.in_flight_target(), None);
        assert_eq!(controller.diagnostics().stale_or_ignored_commands, 1);
    }
}
