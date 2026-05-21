use std::time::{Duration, Instant};

use crate::{PlaybackState, ScrubCommitIntent, ScrubGeneration, ScrubUpdateIntent, SeekRequest};

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

/// Внутренняя команда, которой worker описывает изменение resume intent во время scrub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaybackResumeCommand {
    /// Пользователь явно хочет продолжить playback после завершения scrub.
    Play,

    /// Пользователь явно хочет остаться на паузе после завершения scrub.
    Pause,

    /// Пользователь хочет переключить сохранённый resume intent без изменения session state.
    TogglePlayback,
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

/// Намерение caller-а по preview queue после принятого scrub update-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubUpdatePreviewIntent {
    /// Поставить current latest target в live preview queue.
    QueueLatest,

    /// Не создавать новый preview, потому что update нужен final commit path-у.
    SuppressForCommit,
}

/// Причина, по которой controller не принял scrub update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubUpdateRejection {
    /// Сейчас нет active scrub operation.
    Inactive,

    /// Intent относится не к текущему scrub generation.
    StaleGeneration,
}

/// Controller-level результат постановки accepted update-а в preview queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewQueueDecision {
    /// Latest target стал pending preview для текущего active generation.
    Queued { intent: ScrubUpdateIntent },

    /// Latest target уже отправлен как preview, поэтому новый pending preview не нужен.
    AlreadyInFlight { intent: ScrubUpdateIntent },

    /// Caller осознанно запретил preview для commit/release path-а.
    SuppressedForCommit { intent: ScrubUpdateIntent },
}

/// Typed decision для scrub update boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrubUpdateAcceptance {
    /// Update принадлежит active operation и latest target обновлён.
    Accepted {
        /// Intent, который стал current latest target-ом.
        intent: ScrubUpdateIntent,

        /// Что controller сделал с preview queue для этого update-а.
        preview: PreviewQueueDecision,
    },

    /// Update отброшен без изменения operation state.
    Rejected {
        /// Intent, который не был принят.
        intent: ScrubUpdateIntent,

        /// Точная причина отказа для tests/diagnostics.
        reason: ScrubUpdateRejection,
    },
}

/// Состояние pending preview target-а внутри active operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredPreviewStatus {
    /// Pending preview отсутствует.
    Idle,

    /// Pending preview существует и привязан к active generation.
    Pending { intent: ScrubUpdateIntent },
}

/// Preview seek, который уже отправлен за worker/session boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InFlightScrubPreview {
    /// Intent preview transaction-а; включает scrub generation и пользовательский target.
    pub intent: ScrubUpdateIntent,

    /// Монотонное время dispatch-а, от которого считается replace-stale threshold.
    pub dispatched_at: Instant,
}

/// Причина, по которой preview dispatch не принят controller-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewDispatchRejection {
    /// Сейчас нет active scrub operation.
    Inactive,

    /// Intent относится не к текущему scrub generation.
    StaleGeneration,

    /// Intent больше не совпадает с current latest target-ом.
    NotCurrentLatest,
}

/// Что controller увидел в in-flight slot при accepted preview dispatch-е.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewDispatchState {
    /// Target ещё не был отправлен как preview.
    NewInFlight,

    /// Target уже был отправлен, но caller может сохранить старое поведение dispatch-а.
    AlreadyInFlight,
}

/// Typed decision для preview dispatch boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewDispatchDecision {
    /// Preview можно отправлять в session.
    Dispatch {
        /// Intent, прошедший operation-level проверки.
        intent: ScrubUpdateIntent,

        /// Диагностическое состояние in-flight slot-а до dispatch-а.
        state: PreviewDispatchState,
    },

    /// Preview отброшен без изменения operation state.
    Rejected {
        /// Intent, который не был принят.
        intent: ScrubUpdateIntent,

        /// Точная причина отказа.
        reason: PreviewDispatchRejection,
    },
}

/// Причина, по которой finish scrub не принят controller-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinishScrubRejection {
    /// Сейчас нет active scrub operation.
    Inactive,

    /// Commit intent относится не к текущему scrub generation.
    StaleGeneration,
}

/// Typed decision для завершения active scrub operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinishScrubDecision {
    /// Finish принят, operation state очищен, resume intent возвращён caller-у.
    Accepted {
        /// Commit intent, который закрыл active operation.
        intent: ScrubCommitIntent,

        /// Playback intent, сохранённый на `BeginScrub`.
        resume_intent: PlaybackResumeIntent,
    },

    /// Finish отброшен без очистки active operation.
    Rejected {
        /// Commit intent, который не был принят.
        intent: ScrubCommitIntent,

        /// Точная причина отказа.
        reason: FinishScrubRejection,
    },
}

/// Typed decision для interrupt path-а, которым пользуется worker-side driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekScrubInterruptDecision {
    /// Active scrub был прерван, generation сдвинут, operation state очищен.
    Interrupted,

    /// Прерывать было нечего, controller state не изменился.
    Idle,
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

        /// Preview intent, уже отправленный за worker/session boundary.
        in_flight_preview: Option<InFlightScrubPreview>,

        /// Current latest-цель, ожидающая worker throttle window для preview seek-а.
        deferred_preview_target: Option<SeekRequest>,

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
            in_flight_preview: None,
            deferred_preview_target: None,
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
                in_flight_preview, ..
            } => match in_flight_preview {
                Some(preview) => Some(preview.intent.request),
                None => None,
            },
        }
    }

    /// Возвращает in-flight preview только из active scrub operation.
    const fn in_flight_preview(&self) -> Option<InFlightScrubPreview> {
        match self {
            Self::Idle => None,
            Self::InteractiveScrub {
                in_flight_preview, ..
            } => *in_flight_preview,
        }
    }

    /// Возвращает pending preview intent, если он принадлежит active generation.
    const fn deferred_preview_intent(&self) -> Option<ScrubUpdateIntent> {
        match self {
            Self::Idle => None,
            Self::InteractiveScrub {
                generation,
                deferred_preview_target: Some(request),
                ..
            } => Some(ScrubUpdateIntent::new(*generation, *request)),
            Self::InteractiveScrub {
                deferred_preview_target: None,
                ..
            } => None,
        }
    }

    /// Возвращает typed status для pending preview target-а.
    const fn deferred_preview_status(&self) -> DeferredPreviewStatus {
        match self.deferred_preview_intent() {
            Some(intent) => DeferredPreviewStatus::Pending { intent },
            None => DeferredPreviewStatus::Idle,
        }
    }

    /// Возвращает active resume intent; idle fallback сохраняет прежний default.
    const fn resume_intent(&self) -> PlaybackResumeIntent {
        match self {
            Self::Idle => PlaybackResumeIntent::Pause,
            Self::InteractiveScrub { resume_intent, .. } => *resume_intent,
        }
    }

    /// Запоминает latest target и принимает operation-level preview decision одним шагом.
    fn accept_scrub_update(
        &mut self,
        controller_generation: ScrubGeneration,
        intent: ScrubUpdateIntent,
        preview_intent: ScrubUpdatePreviewIntent,
    ) -> ScrubUpdateAcceptance {
        match self {
            Self::InteractiveScrub {
                generation,
                latest_target,
                in_flight_preview,
                deferred_preview_target,
                ..
            } if controller_generation == intent.generation && *generation == intent.generation => {
                *latest_target = Some(intent.request);
                *deferred_preview_target = None;

                let preview = match preview_intent {
                    ScrubUpdatePreviewIntent::QueueLatest
                        if in_flight_preview
                            .as_ref()
                            .is_some_and(|preview| preview.intent.request == intent.request) =>
                    {
                        PreviewQueueDecision::AlreadyInFlight { intent }
                    }
                    ScrubUpdatePreviewIntent::QueueLatest => {
                        *deferred_preview_target = Some(intent.request);
                        PreviewQueueDecision::Queued { intent }
                    }
                    ScrubUpdatePreviewIntent::SuppressForCommit => {
                        PreviewQueueDecision::SuppressedForCommit { intent }
                    }
                };

                ScrubUpdateAcceptance::Accepted { intent, preview }
            }
            Self::InteractiveScrub { .. } => ScrubUpdateAcceptance::Rejected {
                intent,
                reason: ScrubUpdateRejection::StaleGeneration,
            },
            Self::Idle => ScrubUpdateAcceptance::Rejected {
                intent,
                reason: ScrubUpdateRejection::Inactive,
            },
        }
    }

    /// Забирает pending preview intent, оставляя latest/in-flight state неизменным.
    fn take_deferred_preview(&mut self) -> DeferredPreviewStatus {
        match self {
            Self::Idle => DeferredPreviewStatus::Idle,
            Self::InteractiveScrub {
                generation,
                deferred_preview_target,
                ..
            } => {
                let Some(request) = deferred_preview_target.take() else {
                    return DeferredPreviewStatus::Idle;
                };
                DeferredPreviewStatus::Pending {
                    intent: ScrubUpdateIntent::new(*generation, request),
                }
            }
        }
    }

    /// Помечает preview target как in-flight, если это всё ещё текущий latest target.
    fn mark_preview_seek_dispatched(
        &mut self,
        controller_generation: ScrubGeneration,
        intent: ScrubUpdateIntent,
        dispatched_at: Instant,
    ) -> PreviewDispatchDecision {
        match self {
            Self::InteractiveScrub {
                generation,
                latest_target,
                in_flight_preview,
                deferred_preview_target,
                ..
            } if controller_generation == intent.generation
                && *generation == intent.generation
                && *latest_target == Some(intent.request) =>
            {
                let state = if in_flight_preview
                    .as_ref()
                    .is_some_and(|preview| preview.intent.request == intent.request)
                {
                    PreviewDispatchState::AlreadyInFlight
                } else {
                    PreviewDispatchState::NewInFlight
                };
                *in_flight_preview = Some(InFlightScrubPreview {
                    intent,
                    dispatched_at,
                });
                if *deferred_preview_target == Some(intent.request) {
                    *deferred_preview_target = None;
                }
                PreviewDispatchDecision::Dispatch { intent, state }
            }
            Self::InteractiveScrub { generation, .. }
                if controller_generation == intent.generation
                    && *generation == intent.generation =>
            {
                PreviewDispatchDecision::Rejected {
                    intent,
                    reason: PreviewDispatchRejection::NotCurrentLatest,
                }
            }
            Self::InteractiveScrub { .. } => PreviewDispatchDecision::Rejected {
                intent,
                reason: PreviewDispatchRejection::StaleGeneration,
            },
            Self::Idle => PreviewDispatchDecision::Rejected {
                intent,
                reason: PreviewDispatchRejection::Inactive,
            },
        }
    }

    /// Возвращает задержку до момента, когда in-flight preview можно заменить новым target-ом.
    fn in_flight_replacement_delay(
        &self,
        controller_generation: ScrubGeneration,
        intent: ScrubUpdateIntent,
        now: Instant,
        replace_after: Duration,
    ) -> Option<Duration> {
        match self {
            Self::InteractiveScrub {
                generation,
                latest_target,
                in_flight_preview,
                ..
            } if controller_generation == intent.generation
                && *generation == intent.generation
                && *latest_target == Some(intent.request) =>
            {
                let Some(preview) = in_flight_preview.as_ref() else {
                    return Some(Duration::ZERO);
                };
                if preview.intent.request == intent.request {
                    return Some(Duration::ZERO);
                }

                let preview_age = now.saturating_duration_since(preview.dispatched_at);
                Some(replace_after.saturating_sub(preview_age))
            }
            Self::InteractiveScrub { .. } | Self::Idle => None,
        }
    }

    /// Очищает in-flight marker, если session сообщила terminal state этого preview intent-а.
    fn clear_in_flight_preview(&mut self, intent: ScrubUpdateIntent) -> bool {
        match self {
            Self::InteractiveScrub {
                generation,
                in_flight_preview,
                ..
            } if *generation == intent.generation
                && in_flight_preview
                    .as_ref()
                    .is_some_and(|preview| preview.intent == intent) =>
            {
                *in_flight_preview = None;
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
    ) -> FinishScrubDecision {
        match self {
            Self::InteractiveScrub {
                generation,
                resume_intent,
                ..
            } if controller_generation == intent.generation && *generation == intent.generation => {
                let accepted_resume_intent = *resume_intent;
                *self = Self::Idle;
                FinishScrubDecision::Accepted {
                    intent,
                    resume_intent: accepted_resume_intent,
                }
            }
            Self::InteractiveScrub { .. } => FinishScrubDecision::Rejected {
                intent,
                reason: FinishScrubRejection::StaleGeneration,
            },
            Self::Idle => FinishScrubDecision::Rejected {
                intent,
                reason: FinishScrubRejection::Inactive,
            },
        }
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

    /// Возвращает in-flight preview intent с dispatch-временем для diagnostics/tests.
    #[must_use]
    pub(crate) const fn in_flight_preview(&self) -> Option<InFlightScrubPreview> {
        self.operation.in_flight_preview()
    }

    /// Возвращает typed status pending preview target-а для worker throttle logic.
    #[must_use]
    pub(crate) const fn deferred_preview_status(&self) -> DeferredPreviewStatus {
        self.operation.deferred_preview_status()
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

    /// Запоминает latest scrub target и возвращает typed operation decision.
    pub(crate) fn accept_scrub_update(
        &mut self,
        intent: ScrubUpdateIntent,
        preview_intent: ScrubUpdatePreviewIntent,
    ) -> ScrubUpdateAcceptance {
        let decision =
            self.operation
                .accept_scrub_update(self.generation_id, intent, preview_intent);

        if let ScrubUpdateAcceptance::Rejected { .. } = decision {
            self.count_stale_or_ignored_command();
        }

        decision
    }

    /// Забирает pending preview target, привязанный к active generation.
    pub(crate) fn take_deferred_preview(&mut self) -> DeferredPreviewStatus {
        self.operation.take_deferred_preview()
    }

    /// Помечает preview seek как реально отправленный за worker/session boundary.
    pub(crate) fn mark_preview_seek_dispatched(
        &mut self,
        intent: ScrubUpdateIntent,
        dispatched_at: Instant,
    ) -> PreviewDispatchDecision {
        let decision =
            self.operation
                .mark_preview_seek_dispatched(self.generation_id, intent, dispatched_at);

        if let PreviewDispatchDecision::Rejected { .. } = decision {
            self.count_stale_or_ignored_command();
        }

        decision
    }

    /// Возвращает задержку до разрешённой замены текущего in-flight preview.
    ///
    /// `None` означает, что intent уже не принадлежит текущей operation и dispatch
    /// должен пройти обычную rejection path, не меняя state заранее.
    #[must_use]
    pub(crate) fn in_flight_replacement_delay(
        &self,
        intent: ScrubUpdateIntent,
        now: Instant,
        replace_after: Duration,
    ) -> Option<Duration> {
        self.operation
            .in_flight_replacement_delay(self.generation_id, intent, now, replace_after)
    }

    /// Очищает in-flight preview после terminal state на session side.
    pub(crate) fn clear_in_flight_preview(&mut self, intent: ScrubUpdateIntent) -> bool {
        self.operation.clear_in_flight_preview(intent)
    }

    /// Обрабатывает типизированную resume-команду во время scrub без изменения session state.
    pub(crate) fn consume_resume_intent_command(&mut self, command: PlaybackResumeCommand) -> bool {
        if !self.is_scrubbing() {
            return false;
        }

        match command {
            PlaybackResumeCommand::Play => {
                self.operation.set_resume_intent(PlaybackResumeIntent::Play);
                true
            }
            PlaybackResumeCommand::Pause => {
                self.operation
                    .set_resume_intent(PlaybackResumeIntent::Pause);
                true
            }
            PlaybackResumeCommand::TogglePlayback => {
                self.operation.toggle_resume_intent();
                true
            }
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
    pub(crate) fn finish_scrub(&mut self, intent: ScrubCommitIntent) -> FinishScrubDecision {
        let decision = self.operation.finish_scrub(self.generation_id, intent);

        if let FinishScrubDecision::Rejected { .. } = decision {
            self.count_stale_or_ignored_command();
        }

        decision
    }

    /// Прерывает active scrub для worker boundary без изменения idle generation.
    pub(crate) fn interrupt_active_scrub(&mut self) -> SeekScrubInterruptDecision {
        if !self.is_scrubbing() {
            return SeekScrubInterruptDecision::Idle;
        }

        self.interrupt_scrub();
        SeekScrubInterruptDecision::Interrupted
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

    fn accept_update_with_preview(
        controller: &mut SeekController,
        intent: ScrubUpdateIntent,
    ) -> ScrubUpdateAcceptance {
        controller.accept_scrub_update(intent, ScrubUpdatePreviewIntent::QueueLatest)
    }

    fn accept_update_for_commit(
        controller: &mut SeekController,
        intent: ScrubUpdateIntent,
    ) -> ScrubUpdateAcceptance {
        controller.accept_scrub_update(intent, ScrubUpdatePreviewIntent::SuppressForCommit)
    }

    fn mark_preview_dispatched(
        controller: &mut SeekController,
        intent: ScrubUpdateIntent,
    ) -> PreviewDispatchDecision {
        controller.mark_preview_seek_dispatched(intent, Instant::now())
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
        let intent = ScrubUpdateIntent::new(controller.generation_id(), absolute_seek_request(7));

        let decision = accept_update_with_preview(&mut controller, intent);

        assert_eq!(
            decision,
            ScrubUpdateAcceptance::Rejected {
                intent,
                reason: ScrubUpdateRejection::Inactive,
            }
        );
        assert_eq!(controller.diagnostics().stale_or_ignored_commands, 1);
    }

    #[test]
    fn play_resume_command_during_scrub_sets_play_intent() {
        let mut controller = SeekController::new();
        controller.begin_scrub(PlaybackState::Paused);

        assert!(controller.consume_resume_intent_command(PlaybackResumeCommand::Play));
        assert_eq!(controller.resume_intent(), PlaybackResumeIntent::Play);
    }

    #[test]
    fn pause_resume_command_during_scrub_sets_pause_intent() {
        let mut controller = SeekController::new();
        controller.begin_scrub(PlaybackState::Playing);

        assert!(controller.consume_resume_intent_command(PlaybackResumeCommand::Pause));
        assert_eq!(controller.resume_intent(), PlaybackResumeIntent::Pause);
    }

    #[test]
    fn toggle_resume_command_during_scrub_toggles_resume_intent() {
        let mut controller = SeekController::new();
        controller.begin_scrub(PlaybackState::Playing);

        assert!(controller.consume_resume_intent_command(PlaybackResumeCommand::TogglePlayback));
        assert_eq!(controller.resume_intent(), PlaybackResumeIntent::Pause);
        assert!(controller.consume_resume_intent_command(PlaybackResumeCommand::TogglePlayback));
        assert_eq!(controller.resume_intent(), PlaybackResumeIntent::Play);
    }

    #[test]
    fn resume_command_outside_scrub_is_no_op() {
        let mut controller = SeekController::new();
        let generation_before_command = controller.generation_id();
        let diagnostics_before_command = controller.diagnostics();

        assert!(!controller.consume_resume_intent_command(PlaybackResumeCommand::Play));
        assert!(!controller.consume_resume_intent_command(PlaybackResumeCommand::Pause));
        assert!(!controller.consume_resume_intent_command(PlaybackResumeCommand::TogglePlayback));
        assert_eq!(controller.current_mode(), SeekControllerMode::Idle);
        assert_eq!(controller.resume_intent(), PlaybackResumeIntent::Pause);
        assert_eq!(controller.generation_id(), generation_before_command);
        assert_eq!(controller.diagnostics(), diagnostics_before_command);
    }

    #[test]
    fn interrupt_scrub_clears_targets_and_counts_cancel() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let request = absolute_seek_request(3);
        let intent = ScrubUpdateIntent::new(generation, request);

        assert_eq!(
            accept_update_with_preview(&mut controller, intent),
            ScrubUpdateAcceptance::Accepted {
                intent,
                preview: PreviewQueueDecision::Queued { intent },
            }
        );

        controller.interrupt_scrub();

        assert_eq!(controller.generation_id(), generation.next());
        assert_eq!(controller.current_mode(), SeekControllerMode::Idle);
        assert_eq!(controller.latest_scrub_target(), None);
        assert_eq!(controller.in_flight_target(), None);
        assert_eq!(
            controller.deferred_preview_status(),
            DeferredPreviewStatus::Idle
        );
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
        let intent = ScrubUpdateIntent::new(controller.generation_id(), request);

        assert!(matches!(
            accept_update_with_preview(&mut controller, intent),
            ScrubUpdateAcceptance::Accepted { .. }
        ));
        assert_eq!(controller.latest_scrub_target(), Some(request));
    }

    #[test]
    fn accepted_update_with_allow_preview_returns_typed_queued_decision() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let intent = ScrubUpdateIntent::new(generation, absolute_seek_request(8));

        let decision = accept_update_with_preview(&mut controller, intent);

        assert_eq!(
            decision,
            ScrubUpdateAcceptance::Accepted {
                intent,
                preview: PreviewQueueDecision::Queued { intent },
            }
        );
        assert_eq!(
            controller.deferred_preview_status(),
            DeferredPreviewStatus::Pending { intent }
        );
    }

    #[test]
    fn accepted_update_with_commit_suppression_clears_pending_preview() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let first_intent = ScrubUpdateIntent::new(generation, absolute_seek_request(8));
        let release_intent = ScrubUpdateIntent::new(generation, absolute_seek_request(9));

        assert!(matches!(
            accept_update_with_preview(&mut controller, first_intent),
            ScrubUpdateAcceptance::Accepted {
                preview: PreviewQueueDecision::Queued { .. },
                ..
            }
        ));

        let decision = accept_update_for_commit(&mut controller, release_intent);

        assert_eq!(
            decision,
            ScrubUpdateAcceptance::Accepted {
                intent: release_intent,
                preview: PreviewQueueDecision::SuppressedForCommit {
                    intent: release_intent,
                },
            }
        );
        assert_eq!(
            controller.deferred_preview_status(),
            DeferredPreviewStatus::Idle
        );
        assert_eq!(
            controller.latest_scrub_target(),
            Some(release_intent.request)
        );
    }

    #[test]
    fn scrub_update_does_not_mark_target_in_flight() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let request = absolute_seek_request(9);
        let intent = ScrubUpdateIntent::new(generation, request);

        assert!(matches!(
            accept_update_with_preview(&mut controller, intent),
            ScrubUpdateAcceptance::Accepted { .. }
        ));

        assert_eq!(controller.latest_scrub_target(), Some(request));
        assert_eq!(controller.in_flight_target(), None);
    }

    #[test]
    fn preview_dispatch_marks_current_latest_target_in_flight() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let request = absolute_seek_request(9);
        let intent = ScrubUpdateIntent::new(generation, request);

        assert!(matches!(
            accept_update_with_preview(&mut controller, intent),
            ScrubUpdateAcceptance::Accepted { .. }
        ));
        assert_eq!(
            mark_preview_dispatched(&mut controller, intent),
            PreviewDispatchDecision::Dispatch {
                intent,
                state: PreviewDispatchState::NewInFlight,
            }
        );

        assert_eq!(controller.in_flight_target(), Some(request));
    }

    #[test]
    fn finish_scrub_clears_latest_target_with_operation_state() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Playing);
        let request = absolute_seek_request(11);
        let intent = ScrubUpdateIntent::new(generation, request);

        assert_eq!(
            accept_update_with_preview(&mut controller, intent),
            ScrubUpdateAcceptance::Accepted {
                intent,
                preview: PreviewQueueDecision::Queued { intent },
            }
        );

        assert_eq!(
            controller.finish_scrub(commit_intent(generation)),
            FinishScrubDecision::Accepted {
                intent: commit_intent(generation),
                resume_intent: PlaybackResumeIntent::Play,
            }
        );
        assert_eq!(controller.current_mode(), SeekControllerMode::Idle);
        assert_eq!(controller.latest_scrub_target(), None);
        assert_eq!(
            controller.deferred_preview_status(),
            DeferredPreviewStatus::Idle
        );
    }

    #[test]
    fn finish_scrub_cannot_leave_in_flight_target_without_active_scrub() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let request = absolute_seek_request(13);
        let intent = ScrubUpdateIntent::new(generation, request);

        assert!(matches!(
            accept_update_with_preview(&mut controller, intent),
            ScrubUpdateAcceptance::Accepted { .. }
        ));
        assert!(matches!(
            mark_preview_dispatched(&mut controller, intent),
            PreviewDispatchDecision::Dispatch { .. }
        ));
        assert_eq!(controller.in_flight_target(), Some(request));

        assert_eq!(
            controller.finish_scrub(commit_intent(generation)),
            FinishScrubDecision::Accepted {
                intent: commit_intent(generation),
                resume_intent: PlaybackResumeIntent::Pause,
            }
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
        let current_intent = ScrubUpdateIntent::new(generation, current_request);
        let stale_intent = ScrubUpdateIntent::new(generation.next(), stale_request);

        assert!(matches!(
            accept_update_with_preview(&mut controller, current_intent),
            ScrubUpdateAcceptance::Accepted { .. }
        ));

        let decision = accept_update_with_preview(&mut controller, stale_intent);

        assert_eq!(
            decision,
            ScrubUpdateAcceptance::Rejected {
                intent: stale_intent,
                reason: ScrubUpdateRejection::StaleGeneration,
            }
        );
        assert_eq!(controller.latest_scrub_target(), Some(current_request));
        assert_eq!(
            controller.deferred_preview_status(),
            DeferredPreviewStatus::Pending {
                intent: current_intent,
            }
        );
        assert_eq!(controller.diagnostics().stale_or_ignored_commands, 1);
    }

    #[test]
    fn stale_finish_generation_is_ignored_without_closing_active_scrub() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Playing);
        let request = absolute_seek_request(23);
        let update_intent = ScrubUpdateIntent::new(generation, request);
        let stale_commit_intent = commit_intent(generation.next());

        assert!(matches!(
            accept_update_with_preview(&mut controller, update_intent),
            ScrubUpdateAcceptance::Accepted { .. }
        ));

        assert_eq!(
            controller.finish_scrub(stale_commit_intent),
            FinishScrubDecision::Rejected {
                intent: stale_commit_intent,
                reason: FinishScrubRejection::StaleGeneration,
            }
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

        let dispatched = controller.mark_preview_seek_dispatched(
            ScrubUpdateIntent::new(first_generation, stale_request),
            Instant::now(),
        );

        assert_eq!(
            dispatched,
            PreviewDispatchDecision::Rejected {
                intent: ScrubUpdateIntent::new(first_generation, stale_request),
                reason: PreviewDispatchRejection::StaleGeneration,
            }
        );
        assert_eq!(controller.in_flight_target(), None);
        assert_eq!(controller.diagnostics().stale_or_ignored_commands, 1);
    }

    #[test]
    fn stale_preview_does_not_replace_current_targets() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let current_request = absolute_seek_request(31);
        let current_intent = ScrubUpdateIntent::new(generation, current_request);
        let stale_intent = ScrubUpdateIntent::new(generation.next(), absolute_seek_request(37));

        assert!(matches!(
            accept_update_with_preview(&mut controller, current_intent),
            ScrubUpdateAcceptance::Accepted { .. }
        ));
        assert!(matches!(
            mark_preview_dispatched(&mut controller, current_intent),
            PreviewDispatchDecision::Dispatch { .. }
        ));

        assert_eq!(
            mark_preview_dispatched(&mut controller, stale_intent),
            PreviewDispatchDecision::Rejected {
                intent: stale_intent,
                reason: PreviewDispatchRejection::StaleGeneration,
            }
        );
        assert_eq!(controller.latest_scrub_target(), Some(current_request));
        assert_eq!(controller.in_flight_target(), Some(current_request));
        assert_eq!(
            controller.deferred_preview_status(),
            DeferredPreviewStatus::Idle
        );
        assert_eq!(controller.diagnostics().stale_or_ignored_commands, 1);
    }

    #[test]
    fn new_generation_clears_deferred_preview_target() {
        let mut controller = SeekController::new();
        let first_generation = controller.begin_scrub(PlaybackState::Paused);
        let first_intent = ScrubUpdateIntent::new(first_generation, absolute_seek_request(41));

        assert_eq!(
            accept_update_with_preview(&mut controller, first_intent),
            ScrubUpdateAcceptance::Accepted {
                intent: first_intent,
                preview: PreviewQueueDecision::Queued {
                    intent: first_intent,
                },
            }
        );
        assert_eq!(
            controller.deferred_preview_status(),
            DeferredPreviewStatus::Pending {
                intent: first_intent,
            }
        );

        let second_generation = controller.begin_scrub(PlaybackState::Paused);

        assert_eq!(second_generation, first_generation.next());
        assert_eq!(controller.latest_scrub_target(), None);
        assert_eq!(controller.in_flight_target(), None);
        assert_eq!(
            controller.deferred_preview_status(),
            DeferredPreviewStatus::Idle
        );
    }

    #[test]
    fn preview_dispatch_requires_current_latest_target() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let first_request = absolute_seek_request(43);
        let second_request = absolute_seek_request(47);
        let first_intent = ScrubUpdateIntent::new(generation, first_request);
        let second_intent = ScrubUpdateIntent::new(generation, second_request);

        assert!(matches!(
            accept_update_with_preview(&mut controller, first_intent),
            ScrubUpdateAcceptance::Accepted { .. }
        ));
        assert!(matches!(
            accept_update_with_preview(&mut controller, second_intent),
            ScrubUpdateAcceptance::Accepted { .. }
        ));

        assert_eq!(
            mark_preview_dispatched(&mut controller, first_intent),
            PreviewDispatchDecision::Rejected {
                intent: first_intent,
                reason: PreviewDispatchRejection::NotCurrentLatest,
            }
        );
        assert_eq!(controller.latest_scrub_target(), Some(second_request));
        assert_eq!(controller.in_flight_target(), None);
        assert_eq!(controller.diagnostics().stale_or_ignored_commands, 1);
    }

    #[test]
    fn already_in_flight_target_does_not_create_deferred_preview() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let intent = ScrubUpdateIntent::new(generation, absolute_seek_request(53));

        assert!(matches!(
            accept_update_with_preview(&mut controller, intent),
            ScrubUpdateAcceptance::Accepted {
                preview: PreviewQueueDecision::Queued { .. },
                ..
            }
        ));
        assert!(matches!(
            mark_preview_dispatched(&mut controller, intent),
            PreviewDispatchDecision::Dispatch { .. }
        ));

        let repeated_decision = accept_update_with_preview(&mut controller, intent);

        assert_eq!(
            repeated_decision,
            ScrubUpdateAcceptance::Accepted {
                intent,
                preview: PreviewQueueDecision::AlreadyInFlight { intent },
            }
        );
        assert_eq!(
            controller.deferred_preview_status(),
            DeferredPreviewStatus::Idle
        );
    }

    #[test]
    fn in_flight_replacement_delay_tracks_stale_preview_threshold() {
        let mut controller = SeekController::new();
        let now = Instant::now();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let first_intent = ScrubUpdateIntent::new(generation, absolute_seek_request(53));
        let second_intent = ScrubUpdateIntent::new(generation, absolute_seek_request(57));

        assert!(matches!(
            accept_update_with_preview(&mut controller, first_intent),
            ScrubUpdateAcceptance::Accepted { .. }
        ));
        assert!(matches!(
            controller.mark_preview_seek_dispatched(first_intent, now),
            PreviewDispatchDecision::Dispatch { .. }
        ));
        assert!(matches!(
            accept_update_with_preview(&mut controller, second_intent),
            ScrubUpdateAcceptance::Accepted { .. }
        ));

        assert_eq!(
            controller.in_flight_replacement_delay(
                second_intent,
                now + Duration::from_millis(25),
                Duration::from_millis(100),
            ),
            Some(Duration::from_millis(75))
        );
        assert_eq!(
            controller.in_flight_replacement_delay(
                second_intent,
                now + Duration::from_millis(100),
                Duration::from_millis(100),
            ),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn clear_in_flight_preview_does_not_clear_newer_preview_intent() {
        let mut controller = SeekController::new();
        let generation = controller.begin_scrub(PlaybackState::Paused);
        let first_intent = ScrubUpdateIntent::new(generation, absolute_seek_request(53));
        let second_intent = ScrubUpdateIntent::new(generation, absolute_seek_request(57));

        assert!(matches!(
            accept_update_with_preview(&mut controller, first_intent),
            ScrubUpdateAcceptance::Accepted { .. }
        ));
        assert!(matches!(
            mark_preview_dispatched(&mut controller, first_intent),
            PreviewDispatchDecision::Dispatch { .. }
        ));
        assert!(matches!(
            accept_update_with_preview(&mut controller, second_intent),
            ScrubUpdateAcceptance::Accepted { .. }
        ));
        assert!(matches!(
            mark_preview_dispatched(&mut controller, second_intent),
            PreviewDispatchDecision::Dispatch { .. }
        ));

        assert!(!controller.clear_in_flight_preview(first_intent));
        assert_eq!(controller.in_flight_target(), Some(second_intent.request));
        assert!(controller.clear_in_flight_preview(second_intent));
        assert_eq!(controller.in_flight_target(), None);
    }
}
