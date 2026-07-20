//! Чистые canonical navigation queries и transactional manual preview.

use std::fmt;

use rand::Rng;

use crate::{PlaylistItemId, RepeatMode};

use super::{
    PlaylistQueue, PrepareReservedMutationError, PreparedQueueMutationToken, QueueRevisionSnapshot,
    ReservedQueueMutation, TraversalCurrentItemId,
    shuffle::{ShuffleManualPreview, ShufflePreviewStep},
};

/// Intent обработки одного подтверждённого clean `Ended` текущего элемента.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutomaticEndedIntent {
    repeat_mode: RepeatMode,
}

impl AutomaticEndedIntent {
    /// Создаёт automatic intent без player snapshot или playback side effects.
    pub const fn new(repeat_mode: RepeatMode) -> Self {
        Self { repeat_mode }
    }

    /// Возвращает repeat policy, применённую к clean `Ended`.
    pub const fn repeat_mode(self) -> RepeatMode {
        self.repeat_mode
    }
}

/// Причина, по которой automatic navigation должна остаться остановленной.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomaticStopReason {
    /// В canonical очереди нет ни одного элемента.
    EmptyQueue,
    /// Persisted current отсутствует, поэтому `Ended` нельзя связать с item.
    CurrentItemAbsent,
    /// Текущий элемент является последним, а repeat policy запрещает wrap.
    EndOfQueue { current_item_id: PlaylistItemId },
}

/// Чистое намерение после automatic clean `Ended`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomaticNavigationOutcome {
    /// Контроллер должен открыть следующий committed item.
    OpenItem { item_id: PlaylistItemId },
    /// Контроллер должен replay-нуть текущий active instance без locator reopen.
    ReplayCurrent { item_id: PlaylistItemId },
    /// Контроллер должен оставить playback остановленным.
    Stop(AutomaticStopReason),
}

/// Направление одного явного manual navigation шага.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ManualNavigationDirection {
    /// Перейти к следующему canonical элементу.
    Next,
    /// Перейти к предыдущему canonical элементу.
    Previous,
}

/// Intent одного manual шага с явно названной repeat policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManualNavigationIntent {
    direction: ManualNavigationDirection,
    repeat_mode: RepeatMode,
}

impl ManualNavigationIntent {
    /// Создаёт manual `Next` intent.
    pub const fn next(repeat_mode: RepeatMode) -> Self {
        Self {
            direction: ManualNavigationDirection::Next,
            repeat_mode,
        }
    }

    /// Создаёт manual `Previous` intent.
    pub const fn previous(repeat_mode: RepeatMode) -> Self {
        Self {
            direction: ManualNavigationDirection::Previous,
            repeat_mode,
        }
    }

    /// Возвращает направление шага без позиционного флага.
    pub const fn direction(self) -> ManualNavigationDirection {
        self.direction
    }

    /// Возвращает repeat policy шага.
    pub const fn repeat_mode(self) -> RepeatMode {
        self.repeat_mode
    }
}

/// Typed причина отсутствия item, который следовало бы открыть.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualNavigationNoItem {
    /// В canonical очереди нет элементов.
    EmptyQueue,
    /// Persisted idle queue без current не поддерживает `Previous` fallback.
    PreviousFromPersistedIdle,
    /// Canonical boundary достигнута без разрешённого `RepeatQueue` wrap.
    QueueBoundary {
        current_item_id: PlaylistItemId,
        direction: ManualNavigationDirection,
    },
    /// Speculative cursor вернулся к committed origin и pending open надо отменить.
    ReturnedToCommittedOrigin { item_id: PlaylistItemId },
}

/// Persisted origin, относительно которого построен runtime-only preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualNavigationOrigin {
    /// Очередь восстановлена без committed current.
    PersistedIdle,
    /// Preview начат от committed current item.
    CommittedItem { item_id: PlaylistItemId },
}

/// Typed marker последнего concrete target, завершившегося внешней ошибкой.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FailedManualNavigationTarget {
    item_id: PlaylistItemId,
}

impl FailedManualNavigationTarget {
    /// Возвращает failed target для безопасной correlation в controller state.
    pub const fn item_id(self) -> PlaylistItemId {
        self.item_id
    }
}

/// Runtime-only состояние manual preview, не входящее в persisted traversal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualNavigationPreviewState {
    /// Latest target готов к preparation/open attempt.
    Ready,
    /// Latest target failed, а preview сохранён для D55 Next/Previous/retry/Cancel.
    AwaitingUserAfterFailure(FailedManualNavigationTarget),
}

/// Opaque discardable canonical cursor поверх неизменённого committed traversal.
pub struct ManualNavigationPreview {
    expected_revision: QueueRevisionSnapshot,
    origin: ManualNavigationOrigin,
    latest_target_item_id: PlaylistItemId,
    has_left_committed_origin: bool,
    state: ManualNavigationPreviewState,
    shuffle_preview: Option<ShuffleManualPreview>,
}

impl ManualNavigationPreview {
    /// Возвращает committed origin без смешивания с active player identity.
    pub const fn origin(&self) -> ManualNavigationOrigin {
        self.origin
    }

    /// Возвращает latest desired concrete target для D53 supersede.
    pub const fn latest_target_item_id(&self) -> PlaylistItemId {
        self.latest_target_item_id
    }

    /// Возвращает typed ready/failed состояние preview.
    pub const fn state(&self) -> ManualNavigationPreviewState {
        self.state
    }

    /// Сохраняет D55 failed-target marker без queue mutation.
    pub fn mark_latest_target_failed(mut self) -> Self {
        self.state =
            ManualNavigationPreviewState::AwaitingUserAfterFailure(FailedManualNavigationTarget {
                item_id: self.latest_target_item_id,
            });
        self
    }
}

impl fmt::Debug for ManualNavigationPreview {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManualNavigationPreview")
            .field("expected_revision", &self.expected_revision)
            .field("origin", &self.origin)
            .field("latest_target_item_id", &self.latest_target_item_id)
            .field("has_left_committed_origin", &self.has_left_committed_origin)
            .field("state", &self.state)
            .finish()
    }
}

/// Результат одного manual query/preview шага.
pub enum ManualNavigationOutcome {
    /// Контроллер получил concrete target и новый latest-only preview.
    OpenItem {
        item_id: PlaylistItemId,
        preview: ManualNavigationPreview,
    },
    /// Открывать item не нужно; причина остаётся typed.
    NoItem(ManualNavigationNoItem),
}

impl fmt::Debug for ManualNavigationOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenItem { item_id, preview } => formatter
                .debug_struct("OpenItem")
                .field("item_id", item_id)
                .field("preview", preview)
                .finish(),
            Self::NoItem(reason) => formatter.debug_tuple("NoItem").field(reason).finish(),
        }
    }
}

/// Preview не может быть продолжен после изменения canonical/traversal base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualNavigationPreviewError {
    /// Structural или traversal revision отличается от preview base.
    QueueChanged {
        expected: QueueRevisionSnapshot,
        actual: QueueRevisionSnapshot,
    },
    /// Latest target исчез вопреки matching revision и нарушил queue invariant.
    TargetNotCommitted { item_id: PlaylistItemId },
}

impl fmt::Display for ManualNavigationPreviewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueChanged { expected, actual } => write!(
                formatter,
                "manual navigation preview invalidated: expected {expected}, actual {actual}"
            ),
            Self::TargetNotCommitted { item_id } => {
                write!(
                    formatter,
                    "manual navigation target {item_id} is not committed"
                )
            }
        }
    }
}

impl std::error::Error for ManualNavigationPreviewError {}

/// Non-Clone D08 token, который сохраняет preview до success/failure resolution.
pub struct PreparedManualNavigationToken {
    preview: ManualNavigationPreview,
    reservation_token: PreparedQueueMutationToken,
}

impl PreparedManualNavigationToken {
    /// Возвращает exact target prepared reservation без раскрытия D08 internals.
    pub const fn target_item_id(&self) -> PlaylistItemId {
        self.preview.latest_target_item_id
    }
}

impl fmt::Debug for PreparedManualNavigationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedManualNavigationToken")
            .field("preview", &self.preview)
            .field("reservation_token", &"opaque")
            .finish()
    }
}

/// Typed prepare failure, возвращающий caller-у некоммиченный preview.
pub struct PrepareManualNavigationFailure {
    preview: Box<ManualNavigationPreview>,
    reason: PrepareReservedMutationError,
}

impl PrepareManualNavigationFailure {
    /// Возвращает точную D08 preflight причину без сведения к `bool`.
    pub const fn reason(&self) -> PrepareReservedMutationError {
        self.reason
    }

    /// Возвращает preview для re-evaluation, retry или explicit discard.
    pub fn into_preview(self) -> ManualNavigationPreview {
        *self.preview
    }
}

impl fmt::Debug for PrepareManualNavigationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrepareManualNavigationFailure")
            .field("preview", &self.preview)
            .field("reason", &self.reason)
            .finish()
    }
}

impl fmt::Display for PrepareManualNavigationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "manual navigation prepare failed: {}",
            self.reason
        )
    }
}

impl std::error::Error for PrepareManualNavigationFailure {}

/// Итог explicit successful external open + D08 commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManualNavigationCommit {
    traversal_current: TraversalCurrentItemId,
}

impl ManualNavigationCommit {
    /// Возвращает единственный committed current после successful open.
    pub const fn traversal_current(self) -> TraversalCurrentItemId {
        self.traversal_current
    }
}

/// Typed подтверждение explicit discard без committed mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiscardedManualNavigationPreview {
    latest_target_item_id: PlaylistItemId,
    state: ManualNavigationPreviewState,
}

impl DiscardedManualNavigationPreview {
    /// Возвращает последний logical target, который был отброшен.
    pub const fn latest_target_item_id(self) -> PlaylistItemId {
        self.latest_target_item_id
    }

    /// Возвращает ready/failed состояние на момент discard.
    pub const fn state(self) -> ManualNavigationPreviewState {
        self.state
    }
}

impl PlaylistQueue {
    /// Вычисляет automatic clean `Ended` transition без queue mutation.
    pub fn automatic_navigation(&self, intent: AutomaticEndedIntent) -> AutomaticNavigationOutcome {
        let mut random = rand::rng();
        self.automatic_navigation_with_rng(intent, &mut random)
    }

    /// Deterministic automatic query с injectable RNG на shuffle cycle boundary.
    pub fn automatic_navigation_with_rng<R: Rng + ?Sized>(
        &self,
        intent: AutomaticEndedIntent,
        random: &mut R,
    ) -> AutomaticNavigationOutcome {
        if self.is_empty() {
            return AutomaticNavigationOutcome::Stop(AutomaticStopReason::EmptyQueue);
        }
        let Some(current) = self.traversal_current() else {
            return AutomaticNavigationOutcome::Stop(AutomaticStopReason::CurrentItemAbsent);
        };
        let current_item_id = current.item_id();
        if intent.repeat_mode() == RepeatMode::RepeatOne {
            return AutomaticNavigationOutcome::ReplayCurrent {
                item_id: current_item_id,
            };
        }
        if self.shuffle_enabled() {
            return match self.shuffle_next_target_with_rng(intent.repeat_mode(), random) {
                Some(item_id) => AutomaticNavigationOutcome::OpenItem { item_id },
                None => AutomaticNavigationOutcome::Stop(AutomaticStopReason::EndOfQueue {
                    current_item_id,
                }),
            };
        }
        let current_index = self
            .canonical_index_of(current_item_id)
            .expect("validated traversal current must remain committed");
        if let Some(next_item_id) = self.iter_playable_ids().nth(current_index + 1) {
            return AutomaticNavigationOutcome::OpenItem {
                item_id: next_item_id,
            };
        }
        if intent.repeat_mode() == RepeatMode::RepeatQueue {
            return AutomaticNavigationOutcome::OpenItem {
                item_id: self
                    .iter_playable_ids()
                    .next()
                    .expect("non-empty queue must expose first playable Item ID"),
            };
        }
        AutomaticNavigationOutcome::Stop(AutomaticStopReason::EndOfQueue { current_item_id })
    }

    /// Начинает discardable manual preview от committed current либо idle fallback.
    pub fn begin_manual_navigation(
        &self,
        intent: ManualNavigationIntent,
    ) -> ManualNavigationOutcome {
        let mut random = rand::rng();
        self.begin_manual_navigation_with_rng(intent, &mut random)
    }

    /// Начинает manual preview с injectable RNG для exact shuffle outcomes.
    pub fn begin_manual_navigation_with_rng<R: Rng + ?Sized>(
        &self,
        intent: ManualNavigationIntent,
        random: &mut R,
    ) -> ManualNavigationOutcome {
        if self.is_empty() {
            return ManualNavigationOutcome::NoItem(ManualNavigationNoItem::EmptyQueue);
        }
        let origin = match self.traversal_current() {
            Some(current) => ManualNavigationOrigin::CommittedItem {
                item_id: current.item_id(),
            },
            None => ManualNavigationOrigin::PersistedIdle,
        };
        if let Some(shuffle_traversal) = &self.shuffle_traversal {
            let mut shuffle_preview = ShuffleManualPreview::new(shuffle_traversal);
            let canonical_entry_ids = self.iter_top_level_entry_ids().collect::<Vec<_>>();
            let step = shuffle_preview.step(
                intent.direction(),
                intent.repeat_mode(),
                self,
                &canonical_entry_ids,
                self.traversal_current()
                    .map(TraversalCurrentItemId::item_id),
                random,
            );
            return self.build_initial_shuffle_preview_outcome(
                origin,
                intent.direction(),
                shuffle_preview,
                step,
            );
        }
        let current_index = match origin {
            ManualNavigationOrigin::PersistedIdle => {
                if intent.direction() == ManualNavigationDirection::Previous {
                    return ManualNavigationOutcome::NoItem(
                        ManualNavigationNoItem::PreviousFromPersistedIdle,
                    );
                }
                None
            }
            ManualNavigationOrigin::CommittedItem { item_id } => Some(
                self.canonical_index_of(item_id)
                    .expect("validated traversal current must remain committed"),
            ),
        };
        let Some(target_index) = self.manual_target_index(current_index, intent) else {
            let current_item_id = match origin {
                ManualNavigationOrigin::CommittedItem { item_id } => item_id,
                ManualNavigationOrigin::PersistedIdle => {
                    unreachable!("persisted idle Next always selects the first item")
                }
            };
            return ManualNavigationOutcome::NoItem(ManualNavigationNoItem::QueueBoundary {
                current_item_id,
                direction: intent.direction(),
            });
        };
        let target_item_id = self
            .iter_playable_ids()
            .nth(target_index)
            .expect("validated canonical target index must resolve to an Item ID");
        let has_left_committed_origin = match origin {
            ManualNavigationOrigin::PersistedIdle => true,
            ManualNavigationOrigin::CommittedItem { item_id } => item_id != target_item_id,
        };
        let preview = ManualNavigationPreview {
            expected_revision: self.revision_snapshot(),
            origin,
            latest_target_item_id: target_item_id,
            has_left_committed_origin,
            state: ManualNavigationPreviewState::Ready,
            shuffle_preview: None,
        };
        ManualNavigationOutcome::OpenItem {
            item_id: target_item_id,
            preview,
        }
    }

    /// Продолжает D53 cursor от latest desired target, а не от committed current.
    pub fn continue_manual_navigation(
        &self,
        preview: ManualNavigationPreview,
        intent: ManualNavigationIntent,
    ) -> Result<ManualNavigationOutcome, ManualNavigationPreviewError> {
        let mut random = rand::rng();
        self.continue_manual_navigation_with_rng(preview, intent, &mut random)
    }

    /// Продолжает preview с тем же COW base и injectable shuffle RNG.
    pub fn continue_manual_navigation_with_rng<R: Rng + ?Sized>(
        &self,
        mut preview: ManualNavigationPreview,
        intent: ManualNavigationIntent,
        random: &mut R,
    ) -> Result<ManualNavigationOutcome, ManualNavigationPreviewError> {
        self.validate_manual_preview(&preview)?;
        if let Some(mut shuffle_preview) = preview.shuffle_preview.take() {
            let canonical_entry_ids = self.iter_top_level_entry_ids().collect::<Vec<_>>();
            let step = shuffle_preview.step(
                intent.direction(),
                intent.repeat_mode(),
                self,
                &canonical_entry_ids,
                self.traversal_current()
                    .map(TraversalCurrentItemId::item_id),
                random,
            );
            return Ok(self.build_continued_shuffle_preview_outcome(
                preview,
                intent.direction(),
                shuffle_preview,
                step,
            ));
        }
        let latest_index = self
            .canonical_index_of(preview.latest_target_item_id)
            .ok_or(ManualNavigationPreviewError::TargetNotCommitted {
                item_id: preview.latest_target_item_id,
            })?;
        let Some(target_index) = self.manual_target_index(Some(latest_index), intent) else {
            return Ok(ManualNavigationOutcome::NoItem(
                ManualNavigationNoItem::QueueBoundary {
                    current_item_id: preview.latest_target_item_id,
                    direction: intent.direction(),
                },
            ));
        };
        let target_item_id = self
            .iter_playable_ids()
            .nth(target_index)
            .expect("validated canonical target index must resolve to an Item ID");
        if preview.has_left_committed_origin
            && preview.origin
                == (ManualNavigationOrigin::CommittedItem {
                    item_id: target_item_id,
                })
        {
            return Ok(ManualNavigationOutcome::NoItem(
                ManualNavigationNoItem::ReturnedToCommittedOrigin {
                    item_id: target_item_id,
                },
            ));
        }
        if let ManualNavigationOrigin::CommittedItem { item_id } = preview.origin {
            preview.has_left_committed_origin |= item_id != target_item_id;
        }
        preview.latest_target_item_id = target_item_id;
        preview.state = ManualNavigationPreviewState::Ready;
        Ok(ManualNavigationOutcome::OpenItem {
            item_id: target_item_id,
            preview,
        })
    }

    /// Устанавливает D08 lock для latest preview target до external authorization.
    pub fn prepare_manual_navigation(
        &mut self,
        preview: ManualNavigationPreview,
    ) -> Result<PreparedManualNavigationToken, PrepareManualNavigationFailure> {
        let target_item_id = preview.latest_target_item_id;
        match self.prepare_reserved_mutation(
            preview.expected_revision,
            ReservedQueueMutation::select_committed(target_item_id),
        ) {
            Ok(reservation_token) => Ok(PreparedManualNavigationToken {
                preview,
                reservation_token,
            }),
            Err(reason) => Err(PrepareManualNavigationFailure {
                preview: Box::new(preview),
                reason,
            }),
        }
    }

    /// Exact abort снимает D08 lock и возвращает неизменённый preview.
    pub fn abort_manual_navigation(
        &mut self,
        token: PreparedManualNavigationToken,
    ) -> ManualNavigationPreview {
        self.abort_reserved(token.reservation_token);
        token.preview
    }

    /// External failure снимает D08 lock и сохраняет typed D55 target marker.
    pub fn fail_manual_navigation(
        &mut self,
        token: PreparedManualNavigationToken,
    ) -> ManualNavigationPreview {
        self.abort_manual_navigation(token)
            .mark_latest_target_failed()
    }

    /// Единственный success commit меняет current после external open success.
    pub fn commit_manual_navigation(
        &mut self,
        token: PreparedManualNavigationToken,
    ) -> ManualNavigationCommit {
        let PreparedManualNavigationToken {
            preview,
            reservation_token,
        } = token;
        let committed_target = preview.latest_target_item_id;
        let shuffle_preview = preview.shuffle_preview;
        let reservation_commit = self.commit_reserved(reservation_token);
        let traversal_current = reservation_commit.traversal_current();
        assert_eq!(
            traversal_current.item_id(),
            committed_target,
            "manual navigation reservation must commit its exact preview target"
        );
        if let Some(shuffle_preview) = shuffle_preview {
            shuffle_preview.commit_into(
                self.shuffle_traversal
                    .as_mut()
                    .expect("validated shuffle preview requires enabled traversal"),
                committed_target,
            );
        }
        ManualNavigationCommit { traversal_current }
    }

    /// Explicit Cancel discard-ит preview и не трогает queue revisions/current.
    pub fn discard_manual_navigation(
        &self,
        preview: ManualNavigationPreview,
    ) -> DiscardedManualNavigationPreview {
        DiscardedManualNavigationPreview {
            latest_target_item_id: preview.latest_target_item_id,
            state: preview.state,
        }
    }

    /// Преобразует первый shuffle step в существующий opaque manual boundary.
    fn build_initial_shuffle_preview_outcome(
        &self,
        origin: ManualNavigationOrigin,
        direction: ManualNavigationDirection,
        shuffle_preview: ShuffleManualPreview,
        step: ShufflePreviewStep,
    ) -> ManualNavigationOutcome {
        match step {
            ShufflePreviewStep::Target(item_id) => ManualNavigationOutcome::OpenItem {
                item_id,
                preview: ManualNavigationPreview {
                    expected_revision: self.revision_snapshot(),
                    origin,
                    latest_target_item_id: item_id,
                    has_left_committed_origin: true,
                    state: ManualNavigationPreviewState::Ready,
                    shuffle_preview: Some(shuffle_preview),
                },
            },
            ShufflePreviewStep::PreviousFromPersistedIdle => {
                ManualNavigationOutcome::NoItem(ManualNavigationNoItem::PreviousFromPersistedIdle)
            }
            ShufflePreviewStep::Boundary => match origin {
                ManualNavigationOrigin::PersistedIdle => ManualNavigationOutcome::NoItem(
                    ManualNavigationNoItem::PreviousFromPersistedIdle,
                ),
                ManualNavigationOrigin::CommittedItem { item_id } => {
                    ManualNavigationOutcome::NoItem(ManualNavigationNoItem::QueueBoundary {
                        current_item_id: item_id,
                        direction,
                    })
                }
            },
            ShufflePreviewStep::ReturnedToCommittedOrigin(item_id) => {
                ManualNavigationOutcome::NoItem(ManualNavigationNoItem::ReturnedToCommittedOrigin {
                    item_id,
                })
            }
        }
    }

    /// Возвращает обновлённый preview либо typed no-item без committed mutation.
    fn build_continued_shuffle_preview_outcome(
        &self,
        mut preview: ManualNavigationPreview,
        direction: ManualNavigationDirection,
        shuffle_preview: ShuffleManualPreview,
        step: ShufflePreviewStep,
    ) -> ManualNavigationOutcome {
        match step {
            ShufflePreviewStep::Target(item_id) => {
                preview.latest_target_item_id = item_id;
                preview.state = ManualNavigationPreviewState::Ready;
                preview.shuffle_preview = Some(shuffle_preview);
                ManualNavigationOutcome::OpenItem { item_id, preview }
            }
            ShufflePreviewStep::PreviousFromPersistedIdle => {
                ManualNavigationOutcome::NoItem(ManualNavigationNoItem::PreviousFromPersistedIdle)
            }
            ShufflePreviewStep::Boundary => {
                ManualNavigationOutcome::NoItem(ManualNavigationNoItem::QueueBoundary {
                    current_item_id: preview.latest_target_item_id,
                    direction,
                })
            }
            ShufflePreviewStep::ReturnedToCommittedOrigin(item_id) => {
                ManualNavigationOutcome::NoItem(ManualNavigationNoItem::ReturnedToCommittedOrigin {
                    item_id,
                })
            }
        }
    }

    /// Ищет canonical index через read-only owner surface.
    fn canonical_index_of(&self, item_id: PlaylistItemId) -> Option<usize> {
        self.iter_playable_ids()
            .position(|candidate_item_id| candidate_item_id == item_id)
    }

    /// Вычисляет соседний canonical index с manual repeat semantics D33.
    fn manual_target_index(
        &self,
        current_index: Option<usize>,
        intent: ManualNavigationIntent,
    ) -> Option<usize> {
        let item_count = self.retained_item_count();
        match (current_index, intent.direction()) {
            (None, ManualNavigationDirection::Next) => Some(0),
            (None, ManualNavigationDirection::Previous) => None,
            (Some(index), ManualNavigationDirection::Next) if index + 1 < item_count => {
                Some(index + 1)
            }
            (Some(index), ManualNavigationDirection::Previous) if index > 0 => Some(index - 1),
            (Some(_), _) if intent.repeat_mode() == RepeatMode::RepeatQueue => {
                Some(match intent.direction() {
                    ManualNavigationDirection::Next => 0,
                    ManualNavigationDirection::Previous => item_count - 1,
                })
            }
            (Some(_), _) => None,
        }
    }

    /// Проверяет только structural/traversal base; metadata patch preview не invalidates.
    fn validate_manual_preview(
        &self,
        preview: &ManualNavigationPreview,
    ) -> Result<(), ManualNavigationPreviewError> {
        let actual = self.revision_snapshot();
        let expected = preview.expected_revision;
        if expected.structural() == actual.structural()
            && expected.traversal() == actual.traversal()
        {
            Ok(())
        } else {
            Err(ManualNavigationPreviewError::QueueChanged { expected, actual })
        }
    }
}

#[cfg(test)]
mod tests;
