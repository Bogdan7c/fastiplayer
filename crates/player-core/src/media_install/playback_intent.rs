//! Exact-request playback intent для strong media install transaction.
//!
//! Этот модуль хранит только маленькое latest-only control state. Реальный playback state
//! по-прежнему меняет `PlayerSession` на player-owner thread; shared mutex задаёт порядок
//! intent update относительно atomic install commit и никогда не удерживается во время I/O.

use std::fmt;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};

use super::{MediaInstallRequestId, MediaInstanceId};

/// Typed playback intent нового media без позиционного `bool autoplay`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackIntent {
    /// После install начать playback через обычный buffering/preroll path.
    StartPlaying,

    /// После install оставить media на паузе.
    StartPaused,
}

impl PlaybackIntent {
    /// Адаптирует временный compatibility `autoplay` callsite к typed boundary.
    #[must_use]
    pub const fn from_autoplay(autoplay: bool) -> Self {
        if autoplay {
            Self::StartPlaying
        } else {
            Self::StartPaused
        }
    }
}

/// Монотонная revision явного Play/Pause intent внутри одного install request-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlaybackIntentRevision(NonZeroU64);

impl PlaybackIntentRevision {
    /// Начальная revision для compatibility/one-shot install-а.
    pub const INITIAL: Self = Self(NonZeroU64::MIN);

    /// Создаёт revision из проверенного ненулевого значения.
    #[must_use]
    pub const fn from_non_zero(revision: NonZeroU64) -> Self {
        Self(revision)
    }

    /// Возвращает числовое значение для diagnostics/tests.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for PlaybackIntentRevision {
    /// Печатает только безопасную числовую revision.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Exact request-correlated update playback intent-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackIntentUpdate {
    /// Install request, к staged либо just-installed instance которой относится update.
    pub request_id: MediaInstallRequestId,

    /// Монотонная revision latest-only state.
    pub revision: PlaybackIntentRevision,

    /// Новое стабильное Play/Pause намерение.
    pub intent: PlaybackIntent,
}

/// Typed outcome exact playback intent update-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackIntentUpdateOutcome {
    /// Revision записана в matching staged request и войдёт в commit.
    AppliedToStaged,

    /// Revision применена к exact current instance, созданному этим request-ом.
    AppliedToInstalled {
        /// Exact instance, которую изменил player owner.
        media_instance_id: MediaInstanceId,
    },

    /// Revision меньше принятой либо повторяет номер с другим intent.
    StaleRevision {
        /// Наивысшая уже принятая revision этого request-а.
        latest_revision: PlaybackIntentRevision,
    },

    /// Request неизвестен owner-у, отменён или superseded до commit.
    UnknownOrSupersededRequest,

    /// Request когда-то установил media, но current instance уже сменился.
    StaleInstance,
}

/// Shared single-assignment outcome одного update-а.
#[derive(Debug, Default)]
struct PlaybackIntentOutcomeSlot {
    /// Outcome остаётся доступным всем idempotent receipt clone-ам.
    outcome: Mutex<Option<PlaybackIntentUpdateOutcome>>,
}

impl PlaybackIntentOutcomeSlot {
    /// Публикует terminal outcome ровно один раз.
    fn publish(&self, outcome: PlaybackIntentUpdateOutcome) {
        let mut slot = self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(outcome);
        }
    }

    /// Неблокирующе читает clone-able typed outcome.
    fn get(&self) -> Option<PlaybackIntentUpdateOutcome> {
        *self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Caller-owned receipt exact playback intent update-а.
#[derive(Debug, Clone)]
pub struct PlaybackIntentUpdateReceipt {
    /// Correlation request для diagnostics и безопасного polling-а.
    request_id: MediaInstallRequestId,

    /// Correlation revision этого receipt-а.
    revision: PlaybackIntentRevision,

    /// Shared terminal slot; idempotent update может вернуть clone того же результата.
    outcome: Arc<PlaybackIntentOutcomeSlot>,
}

impl PlaybackIntentUpdateReceipt {
    /// Возвращает request identity этого update-а.
    #[must_use]
    pub const fn request_id(&self) -> MediaInstallRequestId {
        self.request_id
    }

    /// Возвращает revision этого update-а.
    #[must_use]
    pub const fn revision(&self) -> PlaybackIntentRevision {
        self.revision
    }

    /// Неблокирующе читает owner outcome без destructive drain.
    #[must_use]
    pub fn try_outcome(&self) -> Option<PlaybackIntentUpdateOutcome> {
        self.outcome.get()
    }
}

/// Intent и revision, которые atomic commit обязан применить до `Installed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AcceptedPlaybackIntent {
    /// Highest accepted revision staged request-а.
    pub(crate) revision: PlaybackIntentRevision,

    /// Intent, соответствующий highest revision.
    pub(crate) intent: PlaybackIntent,
}

/// Один staged latest-only slot.
#[derive(Debug, Clone, Copy)]
struct StagedPlaybackIntent {
    /// Exact staged request.
    request_id: MediaInstallRequestId,

    /// Highest accepted intent.
    accepted: AcceptedPlaybackIntent,
}

/// Отложенный exact-instance update, который должен применить player owner.
#[derive(Debug)]
struct PendingInstalledPlaybackIntent {
    /// Exact update payload.
    update: PlaybackIntentUpdate,

    /// Outcome всех idempotent callers этой revision.
    outcome: Arc<PlaybackIntentOutcomeSlot>,
}

/// Current installed request/instance и bounded pending update.
#[derive(Debug)]
struct InstalledPlaybackIntent {
    /// Request, которая создала current instance.
    request_id: MediaInstallRequestId,

    /// Exact current instance.
    media_instance_id: MediaInstanceId,

    /// Последний реально применённый owner-ом intent.
    applied: AcceptedPlaybackIntent,

    /// Максимум один latest pending update.
    pending: Option<PendingInstalledPlaybackIntent>,
}

impl InstalledPlaybackIntent {
    /// Highest accepted revision учитывает ещё не обработанный coalesced update.
    fn highest_accepted(&self) -> AcceptedPlaybackIntent {
        self.pending
            .as_ref()
            .map_or(self.applied, |pending| AcceptedPlaybackIntent {
                revision: pending.update.revision,
                intent: pending.update.intent,
            })
    }
}

/// Bounded shared state: один staged, current installed и один stale tombstone.
#[derive(Debug, Default)]
struct PlaybackIntentControlState {
    /// Candidate request до commit/cancel/supersede.
    staged: Option<StagedPlaybackIntent>,

    /// Request exact current installed instance-а.
    installed: Option<InstalledPlaybackIntent>,

    /// Предыдущий installed request нужен для typed `StaleInstance` после смены current.
    stale_installed_request_id: Option<MediaInstallRequestId>,

    /// Latest staged action также должна изменить exact old current media по D52.
    pending_current_for_staged: Option<PendingCurrentPlaybackIntentApply>,
}

/// Result регистрации update-а: receipt плюс необходимость разбудить owner.
pub(crate) struct SubmittedPlaybackIntentUpdate {
    /// Receipt typed outcome-а.
    pub(crate) receipt: PlaybackIntentUpdateReceipt,

    /// `true`, когда exact installed state должен изменить worker thread.
    pub(crate) wake_player_owner: bool,
}

/// Reversible sender-side registration, закрывающая enqueue-vs-fast-owner race.
pub(crate) struct PlaybackIntentStageRegistration {
    /// Новый request, который sender пытается enqueue-ить.
    request_id: MediaInstallRequestId,

    /// Предыдущий staged slot восстанавливается только при transport rejection.
    previous_staged: Option<StagedPlaybackIntent>,
}

/// Payload, забираемый player owner-ом из latest-only installed slot-а.
pub(crate) struct PendingPlaybackIntentApply {
    /// Exact update.
    pub(crate) update: PlaybackIntentUpdate,

    /// Exact instance, к которой request был привязан в момент dequeue.
    pub(crate) media_instance_id: MediaInstanceId,

    /// Terminal outcome slot.
    outcome: Arc<PlaybackIntentOutcomeSlot>,
}

/// Latest-only apply стабильного intent-а к old current, пока candidate ещё staged.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingCurrentPlaybackIntentApply {
    /// Request, staged intent которой породил current update.
    pub(crate) request_id: MediaInstallRequestId,

    /// Exact old current instance; новый commit этот ID не совпадёт.
    pub(crate) media_instance_id: MediaInstanceId,

    /// Стабильный Play/Pause intent пользователя.
    pub(crate) intent: PlaybackIntent,
}

/// Player-owned shared linearization boundary D52.
#[derive(Debug, Default)]
pub(crate) struct PlaybackIntentControl {
    /// Mutex защищает только маленькое in-memory state без I/O и decoder work.
    state: Mutex<PlaybackIntentControlState>,
}

impl PlaybackIntentControl {
    /// Проверяет, остаётся ли sender-registered request latest staged intent-ом.
    pub(crate) fn staged_request_is_latest(&self, request_id: MediaInstallRequestId) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .staged
            .is_some_and(|staged| staged.request_id == request_id)
    }

    /// До enqueue временно регистрирует request и возвращает rollback token.
    pub(crate) fn begin_staged_registration(
        &self,
        request_id: MediaInstallRequestId,
        initial: AcceptedPlaybackIntent,
    ) -> PlaybackIntentStageRegistration {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous_staged = state.staged.replace(StagedPlaybackIntent {
            request_id,
            accepted: initial,
        });
        PlaybackIntentStageRegistration {
            request_id,
            previous_staged,
        }
    }

    /// Откатывает sender registration, если command transport не принял payload.
    pub(crate) fn rollback_staged_registration(
        &self,
        registration: PlaybackIntentStageRegistration,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .staged
            .is_some_and(|staged| staged.request_id == registration.request_id)
        {
            state.staged = registration.previous_staged;
        }
    }

    /// Регистрирует accepted staged request, сохраняя более новую pre-Ready revision.
    pub(crate) fn register_staged_request(
        &self,
        request_id: MediaInstallRequestId,
        initial: AcceptedPlaybackIntent,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(staged) = state.staged.as_mut()
            && staged.request_id == request_id
        {
            if initial.revision > staged.accepted.revision {
                staged.accepted = initial;
            }
            return;
        }

        state.staged = Some(StagedPlaybackIntent {
            request_id,
            accepted: initial,
        });
    }

    /// Удаляет matching staged slot после failure/cancel; installed mapping не затрагивается.
    pub(crate) fn forget_staged_request(&self, request_id: MediaInstallRequestId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .staged
            .is_some_and(|staged| staged.request_id == request_id)
        {
            state.staged = None;
        }
    }

    /// Под тем же lock переводит request staged→installed и возвращает highest intent.
    pub(crate) fn commit_staged_request(
        &self,
        request_id: MediaInstallRequestId,
        media_instance_id: MediaInstanceId,
        commit_player_ownership: impl FnOnce(AcceptedPlaybackIntent),
    ) -> AcceptedPlaybackIntent {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staged = state
            .staged
            .take()
            .filter(|staged| staged.request_id == request_id)
            .unwrap_or_else(|| {
                panic!("media install commit потерял playback intent request {request_id}")
            });

        if state
            .pending_current_for_staged
            .is_some_and(|pending| pending.request_id == request_id)
        {
            state.pending_current_for_staged = None;
        }

        commit_player_ownership(staged.accepted);

        if let Some(mut previous) = state.installed.take() {
            if let Some(pending) = previous.pending.take() {
                pending
                    .outcome
                    .publish(PlaybackIntentUpdateOutcome::StaleInstance);
            }
            state.stale_installed_request_id = Some(previous.request_id);
        }

        state.installed = Some(InstalledPlaybackIntent {
            request_id,
            media_instance_id,
            applied: staged.accepted,
            pending: None,
        });
        staged.accepted
    }

    /// Линеаризует update относительно staged→installed commit под одним mutex.
    pub(crate) fn submit_update(
        &self,
        update: PlaybackIntentUpdate,
    ) -> SubmittedPlaybackIntentUpdate {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if state
            .staged
            .is_some_and(|staged| staged.request_id == update.request_id)
        {
            let outcome = {
                let staged = state
                    .staged
                    .as_mut()
                    .expect("matching staged request уже проверен");
                compare_and_update_accepted(&mut staged.accepted, update)
            };
            let should_apply_current = outcome == PlaybackIntentUpdateOutcome::AppliedToStaged;
            if should_apply_current && let Some(installed) = state.installed.as_ref() {
                state.pending_current_for_staged = Some(PendingCurrentPlaybackIntentApply {
                    request_id: update.request_id,
                    media_instance_id: installed.media_instance_id,
                    intent: update.intent,
                });
            }
            let mut submitted = submitted_immediate(update, outcome);
            submitted.wake_player_owner = should_apply_current;
            return submitted;
        }

        if let Some(installed) = state.installed.as_mut()
            && installed.request_id == update.request_id
        {
            return submit_installed_update(installed, update);
        }

        let outcome = if state.stale_installed_request_id == Some(update.request_id) {
            PlaybackIntentUpdateOutcome::StaleInstance
        } else {
            PlaybackIntentUpdateOutcome::UnknownOrSupersededRequest
        };
        submitted_immediate(update, outcome)
    }

    /// Забирает максимум один latest installed update для применения owner-ом.
    pub(crate) fn take_pending_installed_update(&self) -> Option<PendingPlaybackIntentApply> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let installed = state.installed.as_mut()?;
        let pending = installed.pending.take()?;
        Some(PendingPlaybackIntentApply {
            update: pending.update,
            media_instance_id: installed.media_instance_id,
            outcome: pending.outcome,
        })
    }

    /// Забирает latest staged action для exact old current instance-а.
    pub(crate) fn take_pending_current_for_staged(
        &self,
    ) -> Option<PendingCurrentPlaybackIntentApply> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending_current_for_staged
            .take()
    }

    /// Завершает exact-instance apply и не позволяет старому result затронуть новый current.
    pub(crate) fn finish_installed_update(
        &self,
        pending: PendingPlaybackIntentApply,
        exact_instance_was_applied: bool,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(installed) = state.installed.as_mut() else {
            pending
                .outcome
                .publish(PlaybackIntentUpdateOutcome::StaleInstance);
            return;
        };
        if installed.request_id != pending.update.request_id
            || installed.media_instance_id != pending.media_instance_id
            || !exact_instance_was_applied
        {
            pending
                .outcome
                .publish(PlaybackIntentUpdateOutcome::StaleInstance);
            return;
        }

        if pending.update.revision >= installed.applied.revision {
            installed.applied = AcceptedPlaybackIntent {
                revision: pending.update.revision,
                intent: pending.update.intent,
            };
        }
        pending
            .outcome
            .publish(PlaybackIntentUpdateOutcome::AppliedToInstalled {
                media_instance_id: pending.media_instance_id,
            });
    }
}

/// Применяет monotonic revision к staged state.
fn compare_and_update_accepted(
    accepted: &mut AcceptedPlaybackIntent,
    update: PlaybackIntentUpdate,
) -> PlaybackIntentUpdateOutcome {
    if update.revision < accepted.revision
        || (update.revision == accepted.revision && update.intent != accepted.intent)
    {
        return PlaybackIntentUpdateOutcome::StaleRevision {
            latest_revision: accepted.revision,
        };
    }
    if update.revision > accepted.revision {
        *accepted = AcceptedPlaybackIntent {
            revision: update.revision,
            intent: update.intent,
        };
    }
    PlaybackIntentUpdateOutcome::AppliedToStaged
}

/// Регистрирует update exact installed instance-а либо возвращает idempotent/stale outcome.
fn submit_installed_update(
    installed: &mut InstalledPlaybackIntent,
    update: PlaybackIntentUpdate,
) -> SubmittedPlaybackIntentUpdate {
    let highest = installed.highest_accepted();
    if update.revision < highest.revision
        || (update.revision == highest.revision && update.intent != highest.intent)
    {
        return submitted_immediate(
            update,
            PlaybackIntentUpdateOutcome::StaleRevision {
                latest_revision: highest.revision,
            },
        );
    }

    if update.revision == highest.revision {
        if let Some(pending) = installed.pending.as_ref() {
            return SubmittedPlaybackIntentUpdate {
                receipt: receipt_for(update, Arc::clone(&pending.outcome)),
                wake_player_owner: true,
            };
        }
        return submitted_immediate(
            update,
            PlaybackIntentUpdateOutcome::AppliedToInstalled {
                media_instance_id: installed.media_instance_id,
            },
        );
    }

    if let Some(replaced) = installed.pending.take() {
        replaced
            .outcome
            .publish(PlaybackIntentUpdateOutcome::StaleRevision {
                latest_revision: update.revision,
            });
    }
    let outcome = Arc::new(PlaybackIntentOutcomeSlot::default());
    installed.pending = Some(PendingInstalledPlaybackIntent {
        update,
        outcome: Arc::clone(&outcome),
    });
    SubmittedPlaybackIntentUpdate {
        receipt: receipt_for(update, outcome),
        wake_player_owner: true,
    }
}

/// Создаёт уже завершённый receipt для immediate outcomes.
fn submitted_immediate(
    update: PlaybackIntentUpdate,
    outcome: PlaybackIntentUpdateOutcome,
) -> SubmittedPlaybackIntentUpdate {
    let outcome_slot = Arc::new(PlaybackIntentOutcomeSlot::default());
    outcome_slot.publish(outcome);
    SubmittedPlaybackIntentUpdate {
        receipt: receipt_for(update, outcome_slot),
        wake_player_owner: false,
    }
}

/// Собирает correlated receipt поверх выбранного shared slot-а.
fn receipt_for(
    update: PlaybackIntentUpdate,
    outcome: Arc<PlaybackIntentOutcomeSlot>,
) -> PlaybackIntentUpdateReceipt {
    PlaybackIntentUpdateReceipt {
        request_id: update.request_id,
        revision: update.revision,
        outcome,
    }
}

#[cfg(test)]
mod tests;
