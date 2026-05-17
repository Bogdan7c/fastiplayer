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

/// Внутренняя операция seek-controller-а, которая владеет scrub-only состоянием.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SeekOperation {
    /// Нет активной операции, поэтому scrub targets и resume intent недоступны.
    Idle,

    /// Активный interactive scrub со всеми данными, привязанными к одному generation.
    InteractiveScrub {
        /// Поколение пользовательской scrub-операции, которому принадлежат targets.
        generation: ScrubGeneration,

        /// Самая свежая цель, принятая от UI drag/update path.
        latest_target: Option<SeekRequest>,

        /// Цель, уже отправленная за worker/session boundary как preview seek.
        in_flight_target: Option<SeekRequest>,

        /// Playback intent, который нужно применить после final commit-а.
        resume_intent: PlaybackResumeIntent,
    },
}

impl SeekOperation {
    /// Собирает active scrub operation из generation и playback state boundary.
    fn interactive_scrub(generation: ScrubGeneration, playback_state: PlaybackState) -> Self {
        Self::InteractiveScrub {
            generation,
            latest_target: None,
            in_flight_target: None,
            resume_intent: PlaybackResumeIntent::from_playback_state(playback_state),
        }
    }

    /// Возвращает compatibility mode без отдельного поля, которое могло бы рассинхрониться.
    const fn current_mode(&self) -> SeekControllerMode {
        match self {
            Self::Idle => SeekControllerMode::Idle,
            Self::InteractiveScrub { .. } => SeekControllerMode::Scrubbing,
        }
    }

    /// Возвращает `true`, только если scrub-local state реально существует.
    const fn is_scrubbing(&self) -> bool {
        matches!(self, Self::InteractiveScrub { .. })
    }

    /// Возвращает latest target только из active scrub operation.
    const fn latest_target(&self) -> Option<SeekRequest> {
        match self {
            Self::Idle => None,
            Self::InteractiveScrub { latest_target, .. } => *latest_target,
        }
    }

    /// Возвращает dispatched preview target только из active scrub operation.
    const fn in_flight_target(&self) -> Option<SeekRequest> {
        match self {
            Self::Idle => None,
            Self::InteractiveScrub {
                in_flight_target, ..
            } => *in_flight_target,
        }
    }

    /// Возвращает active resume intent; idle fallback сохраняет прежний default.
    const fn resume_intent(&self) -> PlaybackResumeIntent {
        match self {
            Self::Idle => PlaybackResumeIntent::Pause,
            Self::InteractiveScrub { resume_intent, .. } => *resume_intent,
        }
    }

    /// Запоминает latest target, если update относится к active scrub generation.
    fn accept_scrub_update(
        &mut self,
        controller_generation: ScrubGeneration,
        intent: ScrubUpdateIntent,
    ) -> bool {
        match self {
            Self::InteractiveScrub {
                generation,
                latest_target,
                ..
            } if controller_generation == intent.generation && *generation == intent.generation => {
                *latest_target = Some(intent.request);
                true
            }
            Self::InteractiveScrub { .. } | Self::Idle => false,
        }
    }

    /// Помечает preview target как in-flight, если это всё ещё текущий latest target.
    fn mark_preview_seek_dispatched(
        &mut self,
        controller_generation: ScrubGeneration,
        intent: ScrubUpdateIntent,
    ) -> bool {
        match self {
            Self::InteractiveScrub {
                generation,
                latest_target,
                in_flight_target,
                ..
            } if controller_generation == intent.generation
                && *generation == intent.generation
                && *latest_target == Some(intent.request) =>
            {
                *in_flight_target = Some(intent.request);
                true
            }
            Self::InteractiveScrub { .. } | Self::Idle => false,
        }
    }

    /// Завершает active scrub и возвращает resume intent, если generation актуален.
    fn finish_scrub(
        &mut self,
        controller_generation: ScrubGeneration,
        intent: ScrubCommitIntent,
    ) -> Option<PlaybackResumeIntent> {
        if !self.matches_active_generation(controller_generation, intent.generation) {
            return None;
        }

        let resume_intent = self.resume_intent();
        *self = Self::Idle;
        Some(resume_intent)
    }

    /// Обновляет resume intent только внутри active scrub operation.
    fn set_resume_intent(&mut self, next_resume_intent: PlaybackResumeIntent) {
        if let Self::InteractiveScrub { resume_intent, .. } = self {
            *resume_intent = next_resume_intent;
        }
    }

    /// Переключает resume intent только внутри active scrub operation.
    fn toggle_resume_intent(&mut self) {
        if let Self::InteractiveScrub { resume_intent, .. } = self {
            *resume_intent = resume_intent.toggled();
        }
    }

    /// Проверяет, что intent относится к текущей active scrub operation.
    fn matches_active_generation(
        &self,
        controller_generation: ScrubGeneration,
        expected_generation: ScrubGeneration,
    ) -> bool {
        if controller_generation != expected_generation {
            return false;
        }

        match self {
            Self::InteractiveScrub { generation, .. } => *generation == expected_generation,
            Self::Idle => false,
        }
    }
}

impl Default for SeekOperation {
    /// Новый controller стартует без active operation и без scrub-only данных.
    fn default() -> Self {
        Self::Idle
    }
}

/// Skeleton state machine для будущего live seek и scrub commit pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeekController {
    /// Поколение seek/scrub операции; увеличивается при новом scrub или cancel.
    generation_id: ScrubGeneration,

    /// Текущая операция controller-а; владеет данными, допустимыми только во время scrub.
    operation: SeekOperation,

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
        self.operation.current_mode()
    }

    /// Возвращает последнюю scrub-цель, если scrub активен.
    #[must_use]
    pub const fn latest_scrub_target(&self) -> Option<SeekRequest> {
        self.operation.latest_target()
    }

    /// Возвращает цель, которая уже была отдана в обработку.
    #[must_use]
    pub const fn in_flight_target(&self) -> Option<SeekRequest> {
        self.operation.in_flight_target()
    }

    /// Возвращает намерение возобновления после scrub.
    #[must_use]
    pub const fn resume_intent(&self) -> PlaybackResumeIntent {
        self.operation.resume_intent()
    }

    /// Возвращает текущие diagnostics counters.
    #[must_use]
    pub const fn diagnostics(&self) -> SeekControllerDiagnostics {
        self.diagnostics
    }

    /// Возвращает `true`, если сейчас активен interactive scrub.
    #[must_use]
    pub const fn is_scrubbing(&self) -> bool {
        self.operation.is_scrubbing()
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
        self.operation = SeekOperation::interactive_scrub(generation, playback_state);
    }

    /// Запоминает latest scrub target без объявления seek transaction-а in-flight.
    pub(crate) fn accept_scrub_update(&mut self, intent: ScrubUpdateIntent) -> bool {
        if self
            .operation
            .accept_scrub_update(self.generation_id, intent)
        {
            return true;
        }

        self.count_stale_or_ignored_command();
        false
    }

    /// Помечает preview seek как реально отправленный за worker/session boundary.
    pub(crate) fn mark_preview_seek_dispatched(&mut self, intent: ScrubUpdateIntent) -> bool {
        if self
            .operation
            .mark_preview_seek_dispatched(self.generation_id, intent)
        {
            return true;
        }

        self.count_stale_or_ignored_command();
        false
    }

    /// Обрабатывает Play/Pause/Toggle во время scrub без изменения session state.
    pub fn consume_resume_intent_command(&mut self, command: &PlayerCommand) -> bool {
        if !self.is_scrubbing() {
            return false;
        }

        match command {
            PlayerCommand::Play => {
                self.operation.set_resume_intent(PlaybackResumeIntent::Play);
                true
            }
            PlayerCommand::Pause => {
                self.operation
                    .set_resume_intent(PlaybackResumeIntent::Pause);
                true
            }
            PlayerCommand::TogglePlayback => {
                self.operation.toggle_resume_intent();
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
        let Some(resume_intent) = self.operation.finish_scrub(self.generation_id, intent) else {
            self.count_stale_or_ignored_command();
            return None;
        };

        Some(resume_intent)
    }

    /// Прерывает scrub из-за Stop/Open/Shutdown и очищает pending цели.
    pub fn interrupt_scrub(&mut self) {
        if self.is_scrubbing() {
            self.diagnostics.cancelled_operations =
                self.diagnostics.cancelled_operations.saturating_add(1);
        }

        self.generation_id = self.generation_id.next();
        self.operation = SeekOperation::Idle;
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
            operation: SeekOperation::Idle,
            diagnostics: SeekControllerDiagnostics::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use media_core::MediaTime;

    use super::*;
    use crate::{ScrubCommitPolicy, SeekMode, SeekTarget};

    fn absolute_seek_request(seconds: u64) -> SeekRequest {
        SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_secs(seconds)),
            mode: SeekMode::Accurate,
        }
    }

    fn commit_intent(generation: ScrubGeneration) -> ScrubCommitIntent {
        ScrubCommitIntent::new(generation, ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE)
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
    fn finish_scrub_clears_latest_target_with_operation_state() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Playing);
        let request = absolute_seek_request(11);

        assert!(controller.accept_scrub_update(ScrubUpdateIntent::new(generation, request)));

        assert_eq!(
            controller.finish_scrub(commit_intent(generation)),
            Some(PlaybackResumeIntent::Play)
        );
        assert_eq!(controller.current_mode(), SeekControllerMode::Idle);
        assert_eq!(controller.latest_scrub_target(), None);
    }

    #[test]
    fn finish_scrub_cannot_leave_in_flight_target_without_active_scrub() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let request = absolute_seek_request(13);
        let intent = ScrubUpdateIntent::new(generation, request);

        assert!(controller.accept_scrub_update(intent));
        assert!(controller.mark_preview_seek_dispatched(intent));
        assert_eq!(controller.in_flight_target(), Some(request));

        assert_eq!(
            controller.finish_scrub(commit_intent(generation)),
            Some(PlaybackResumeIntent::Pause)
        );
        assert!(!controller.is_scrubbing());
        assert_eq!(controller.in_flight_target(), None);
    }

    #[test]
    fn stale_generation_update_is_ignored_without_replacing_current_target() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let current_request = absolute_seek_request(17);
        let stale_request = absolute_seek_request(19);

        assert!(
            controller.accept_scrub_update(ScrubUpdateIntent::new(generation, current_request,))
        );

        let accepted = controller
            .accept_scrub_update(ScrubUpdateIntent::new(generation.next(), stale_request));

        assert!(!accepted);
        assert_eq!(controller.latest_scrub_target(), Some(current_request));
        assert_eq!(controller.diagnostics().stale_or_ignored_commands, 1);
    }

    #[test]
    fn stale_finish_generation_is_ignored_without_closing_active_scrub() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Playing);
        let request = absolute_seek_request(23);

        assert!(controller.accept_scrub_update(ScrubUpdateIntent::new(generation, request)));

        assert_eq!(
            controller.finish_scrub(commit_intent(generation.next())),
            None
        );
        assert!(controller.is_scrubbing());
        assert_eq!(controller.latest_scrub_target(), Some(request));
        assert_eq!(controller.diagnostics().stale_or_ignored_commands, 1);
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
