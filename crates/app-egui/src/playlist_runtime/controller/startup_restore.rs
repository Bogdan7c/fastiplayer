//! D04/D22 restored-current preparation и bounded fallback policy.

use std::sync::Arc;

use player_core::{MediaInstallRequestId, PlaybackIntent, PlaybackIntentRevision};
use playlist_core::{
    AutomaticEndedIntent, AutomaticTraversalAdvance, AutomaticTraversalPlan,
    AutomaticTraversalStart, PlaylistItemId, RepeatMode,
};

use super::PlaylistController;
use super::automatic_lifecycle::{AutomaticStopCause, PlaylistErrorBehavior, SkipChainStart};
use super::install::{
    PlaylistInstallAdmissionError, PlaylistInstallMutation, PlaylistInstallRequest,
};
use super::transport::PlannedPlaylistInstall;
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::identity::{
    PendingTargetOrigin, PlaylistItemErrorCategory, PlaylistItemErrorPhase,
};
use crate::playlist_runtime::operational_open::{
    OperationalOpenLocatorError, operational_open_locator,
};

/// Startup open всегда явно говорит, оставлять начало или восстанавливать checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupPosition {
    KeepStart,
    Restore(std::time::Duration),
}

/// Opaque startup target сохраняет exact traversal mutation до `Installed`.
pub(crate) struct StartupRestoreTarget {
    /// Identity строки очереди (ключ resume-checkpoint, stale-guard); НЕ для сетевого open.
    pub(crate) locator: playlist_core::PlaylistLocator,
    /// Locator, который реально открывается (собственная identity ролика коллекции).
    pub(crate) open_locator: Box<playlist_core::PlaylistLocator>,
    pub(crate) position: StartupPosition,
    pub(super) install: PlannedPlaylistInstall,
}

impl StartupRestoreTarget {
    /// Correlation ID существующей persisted row; allocator здесь не вызывается.
    pub(crate) const fn item_id(&self) -> PlaylistItemId {
        self.install.item_id
    }

    /// Возвращает playback intent, который startup owner применит после position receipt.
    pub(crate) const fn playback_intent(&self) -> PlaybackIntent {
        self.install.playback_intent
    }

    /// Привязывает persisted restore к актуальной startup policy до media-open admission.
    pub(crate) fn set_playback_intent(&mut self, playback_intent: PlaybackIntent) {
        self.install.playback_intent = playback_intent;
    }

    /// Position policy читается strong-open транзакцией до передачи domain install plan.
    pub(crate) const fn position(&self) -> StartupPosition {
        self.position
    }

    pub(crate) fn set_position(&mut self, position: StartupPosition) {
        self.position = position;
    }
}

/// Следующий шаг D22 до построения open target (внутренний, без locator-ов).
enum StartupRestoreStep {
    OpenItem {
        item_id: PlaylistItemId,
        plan: Box<AutomaticTraversalPlan>,
    },
    Stopped {
        cause: AutomaticStopCause,
    },
}

/// Итог попытки построить open target для строки очереди.
enum StartupRestoreTargetBuild {
    Ready(StartupRestoreTarget),
    /// Строка исчезла из очереди.
    Missing,
    /// Открывать отдельно нечего; install возвращается для продолжения цепочки.
    Refused {
        install: PlannedPlaylistInstall,
        refusal: OperationalOpenLocatorError,
    },
}

/// Результат D22 preparation failure для одного restored target.
pub(crate) enum StartupRestoreFailureOutcome {
    Stopped { cause: AutomaticStopCause },
    OpenItem { target: StartupRestoreTarget },
}

impl PlaylistController {
    /// Pre-barrier media-open terminal продолжает тот же D22 chain; enqueue-win остаётся fatal.
    pub(crate) fn report_startup_restore_install_failure(
        &mut self,
        request_id: MediaOpenRequestId,
        safe_summary: Arc<str>,
    ) -> Option<StartupRestoreFailureOutcome> {
        let request = self.take_awaiting_startup_restore_failure(request_id)?;
        let item_id = request.target_item_id?;
        // Строка могла исчезнуть: тогда продолжать цепочку не от чего.
        self.queue.item(item_id)?;
        Some(self.report_startup_restore_install_failed(
            PlannedPlaylistInstall {
                item_id,
                playback_intent: PlaybackIntent::StartPaused,
                intent_revision: request.intent_revision,
                pending_origin: request.origin,
                expected_queue_revision: request.expected_queue_revision,
                mutation: request.mutation,
            },
            safe_summary,
        ))
    }

    /// Коррелирует opaque restore target с player staging, не меняя traversal до Installed.
    pub(crate) fn accept_startup_restore_install(
        &mut self,
        request_id: MediaOpenRequestId,
        player_request_id: MediaInstallRequestId,
        target: StartupRestoreTarget,
    ) -> Result<(), PlaylistInstallAdmissionError> {
        let planned = target.install;
        self.accept_install_request(PlaylistInstallRequest {
            request_id,
            player_request_id,
            target_item_id: Some(planned.item_id),
            origin: planned.pending_origin,
            intent_revision: planned.intent_revision,
            expected_queue_revision: planned.expected_queue_revision,
            mutation: planned.mutation,
        })
    }

    /// Возвращает persisted current без выбора первого элемента при `current=None`.
    ///
    /// Если у current нет locator-а для открытия (например, у ролика коллекции сохранён
    /// только внутренний идентификатор экстрактора), строка помечается ошибкой и D22
    /// цепочка пропусков ведётся сразу — наружу выходят только открываемые target-ы.
    pub(crate) fn startup_restored_current(&mut self) -> Option<StartupRestoreTarget> {
        let item_id = self.queue.traversal_current()?.item_id();
        let install = self.planned_startup_restore_install(
            item_id,
            PlaylistInstallMutation::Reserved(
                playlist_core::ReservedQueueMutation::select_committed(item_id),
            ),
        );
        match self.build_startup_restore_target(install) {
            StartupRestoreTargetBuild::Ready(target) => Some(target),
            StartupRestoreTargetBuild::Missing => None,
            StartupRestoreTargetBuild::Refused { install, refusal } => {
                tracing::warn!(error = %refusal, "Restored current нельзя открыть отдельно");
                match self
                    .report_startup_restore_install_failed(install, Arc::from(refusal.to_string()))
                {
                    StartupRestoreFailureOutcome::OpenItem { target } => Some(target),
                    StartupRestoreFailureOutcome::Stopped { .. } => None,
                }
            }
        }
    }

    /// D22 сохраняет unavailable row и строит bounded domain traversal только при Skip.
    pub(crate) fn report_startup_restore_failure(
        &mut self,
        failed: StartupRestoreTarget,
        safe_summary: Arc<str>,
    ) -> StartupRestoreFailureOutcome {
        self.report_startup_restore_install_failed(failed.install, safe_summary)
    }

    /// Ведёт цепочку пропусков (сессия 07): первая неудача восстановленного
    /// элемента начинает цепочку «до первого воспроизведения», следующие её продолжают,
    /// а остановка закрывает её итогом для уведомления.
    fn report_startup_restore_install_failed(
        &mut self,
        failed: PlannedPlaylistInstall,
        safe_summary: Arc<str>,
    ) -> StartupRestoreFailureOutcome {
        // Reserved — это сам восстановленный current; AutomaticTraversal — уже продолжение.
        if matches!(failed.mutation, PlaylistInstallMutation::Reserved(_)) {
            self.begin_automatic_skip_chain(SkipChainStart::BeforeAnyPlayback);
        }
        self.record_automatic_skip(failed.item_id);
        let outcome = self.startup_restore_failure_outcome(failed, safe_summary);
        if let StartupRestoreFailureOutcome::Stopped { cause } = &outcome {
            self.finish_automatic_skip_chain_with_stop(*cause);
        }
        outcome
    }

    /// Решение D22 для неудачного restored target без учёта сводки пропусков.
    ///
    /// Кандидат, у которого нет locator-а для открытия, пропускается тем же циклом
    /// (не рекурсией: очередь может быть до 50 000 строк).
    fn startup_restore_failure_outcome(
        &mut self,
        mut failed: PlannedPlaylistInstall,
        mut safe_summary: Arc<str>,
    ) -> StartupRestoreFailureOutcome {
        loop {
            let (item_id, plan) = match self.next_startup_restore_step(failed, safe_summary) {
                StartupRestoreStep::OpenItem { item_id, plan } => (item_id, plan),
                StartupRestoreStep::Stopped { cause } => {
                    return StartupRestoreFailureOutcome::Stopped { cause };
                }
            };
            let install = self.planned_startup_restore_install(
                item_id,
                PlaylistInstallMutation::AutomaticTraversal(plan),
            );
            match self.build_startup_restore_target(install) {
                StartupRestoreTargetBuild::Ready(target) => {
                    return StartupRestoreFailureOutcome::OpenItem { target };
                }
                StartupRestoreTargetBuild::Missing => {
                    return StartupRestoreFailureOutcome::Stopped {
                        cause: AutomaticStopCause::StructuralInvalidation,
                    };
                }
                StartupRestoreTargetBuild::Refused { install, refusal } => {
                    tracing::warn!(error = %refusal, "Кандидат restore нельзя открыть отдельно");
                    self.record_automatic_skip(install.item_id);
                    failed = install;
                    safe_summary = Arc::from(refusal.to_string());
                }
            }
        }
    }

    /// Отмечает ошибку строки и выбирает следующий шаг D22 без построения open target.
    fn next_startup_restore_step(
        &mut self,
        failed: PlannedPlaylistInstall,
        safe_summary: Arc<str>,
    ) -> StartupRestoreStep {
        self.upsert_runtime_error(
            failed.item_id,
            PlaylistItemErrorPhase::Preparation,
            PlaylistItemErrorCategory::Unavailable,
            safe_summary,
            None,
            None,
        );
        if self.repeat_mode == RepeatMode::RepeatOne {
            return StartupRestoreStep::Stopped {
                cause: AutomaticStopCause::RepeatOneError,
            };
        }
        if self.error_behavior == PlaylistErrorBehavior::Stop {
            return StartupRestoreStep::Stopped {
                cause: AutomaticStopCause::ErrorPolicy,
            };
        }

        let traversal = match failed.mutation {
            PlaylistInstallMutation::Reserved(_) => self
                .queue
                .begin_automatic_error_traversal(AutomaticEndedIntent::new(self.repeat_mode)),
            PlaylistInstallMutation::AutomaticTraversal(plan) => {
                return match self.queue.advance_automatic_traversal_after_failure(*plan) {
                    AutomaticTraversalAdvance::OpenItem { item_id, plan } => {
                        StartupRestoreStep::OpenItem { item_id, plan }
                    }
                    AutomaticTraversalAdvance::AllFailed { attempted_count } => {
                        StartupRestoreStep::Stopped {
                            cause: AutomaticStopCause::AllCandidatesFailed { attempted_count },
                        }
                    }
                };
            }
            PlaylistInstallMutation::ManualNavigation => {
                return StartupRestoreStep::Stopped {
                    cause: AutomaticStopCause::StructuralInvalidation,
                };
            }
        };
        match traversal {
            AutomaticTraversalStart::OpenItem { item_id, plan } => {
                StartupRestoreStep::OpenItem { item_id, plan }
            }
            AutomaticTraversalStart::ReplayCurrent { .. } => StartupRestoreStep::Stopped {
                cause: AutomaticStopCause::RepeatOneError,
            },
            AutomaticTraversalStart::Stop(reason) => StartupRestoreStep::Stopped {
                cause: AutomaticStopCause::Domain(reason),
            },
        }
    }

    /// Строит target с operational locator-ом либо сообщает, почему открыть строку нельзя.
    fn build_startup_restore_target(
        &self,
        install: PlannedPlaylistInstall,
    ) -> StartupRestoreTargetBuild {
        let Some(item) = self.queue.item(install.item_id) else {
            return StartupRestoreTargetBuild::Missing;
        };
        match operational_open_locator(item) {
            Ok(open_locator) => StartupRestoreTargetBuild::Ready(StartupRestoreTarget {
                locator: item.locator().clone(),
                // Box держит target ниже порога `large_enum_variant` у enum-ов-обёрток.
                open_locator: Box::new(open_locator),
                position: StartupPosition::KeepStart,
                install,
            }),
            Err(refusal) => StartupRestoreTargetBuild::Refused { install, refusal },
        }
    }

    fn planned_startup_restore_install(
        &self,
        item_id: PlaylistItemId,
        mutation: PlaylistInstallMutation,
    ) -> PlannedPlaylistInstall {
        PlannedPlaylistInstall {
            item_id,
            playback_intent: PlaybackIntent::StartPaused,
            intent_revision: PlaybackIntentRevision::from_non_zero(self.stable_intent_revision),
            pending_origin: PendingTargetOrigin::RestoredCurrent,
            expected_queue_revision: self.queue.revision_snapshot(),
            mutation,
        }
    }
}

#[cfg(test)]
mod playback_policy_tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use playlist_core::{
        CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind,
    };

    use super::*;

    fn restored_target(position: StartupPosition) -> StartupRestoreTarget {
        let mut controller = PlaylistController::new();
        let appended = controller
            .append(vec![PlaylistItemDraft::local(
                LocalLocator::Native(PathBuf::from("startup-restore-test.mkv")),
                None,
                CachedPlaylistMetadata::new("startup restore", PlaylistMediaKind::Video),
            )])
            .expect("append startup restore fixture");
        let item_id = match appended {
            super::super::ControllerAppendOutcome::Added { item_ids, .. } => item_ids[0],
            super::super::ControllerAppendOutcome::NoItemsProvided => {
                panic!("startup restore fixture must append one row")
            }
        };
        controller
            .queue
            .set_traversal_current(item_id)
            .expect("select persisted current fixture");
        let mut target = controller
            .startup_restored_current()
            .expect("create restored-current target");
        target.set_position(position);
        target
    }

    #[test]
    fn cold_resume_autoplay_preserves_checkpoint_and_stages_start_playing_intent() {
        let checkpoint = Duration::from_secs(355);
        let mut target = restored_target(StartupPosition::Restore(checkpoint));
        let mut config = fastiplayer_config::AppConfig::default();
        config.player.start_paused = false;

        crate::startup_media::apply_restored_playback_policy(&mut target, &config);

        assert_eq!(target.position(), StartupPosition::Restore(checkpoint));
        assert_eq!(target.playback_intent(), PlaybackIntent::StartPlaying);
    }

    #[test]
    fn startup_at_zero_autoplay_keeps_exact_start_and_stages_start_playing_intent() {
        let mut target = restored_target(StartupPosition::KeepStart);
        let mut config = fastiplayer_config::AppConfig::default();
        config.player.start_paused = false;

        crate::startup_media::apply_restored_playback_policy(&mut target, &config);

        assert_eq!(target.position(), StartupPosition::KeepStart);
        assert_eq!(target.playback_intent(), PlaybackIntent::StartPlaying);
    }

    #[test]
    fn configured_pause_remains_paused_without_losing_resume_checkpoint() {
        let checkpoint = Duration::from_secs(355);
        let mut target = restored_target(StartupPosition::Restore(checkpoint));
        let mut config = fastiplayer_config::AppConfig::default();
        config.player.start_paused = true;

        crate::startup_media::apply_restored_playback_policy(&mut target, &config);

        assert_eq!(target.position(), StartupPosition::Restore(checkpoint));
        assert_eq!(target.playback_intent(), PlaybackIntent::StartPaused);
    }
}

#[cfg(test)]
#[path = "startup_restore/open_locator_tests.rs"]
mod open_locator_tests;
