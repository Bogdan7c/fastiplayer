//! Shell-facing lifecycle boundary process-lifetime playlist runtime-а.
//!
//! Parent-модуль остаётся владельцем данных `PlaylistRuntime`, конструирования и media-open
//! authority. Здесь собраны только exact binding generations, mailbox admission, read-only
//! attachment и bounded shutdown projection, чтобы shell не зависел от устройства соседних owners.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use playlist_core::PlaylistQueue;

use crate::app_wake::{OwnerMailboxPublisher, WakeDelivery};
use crate::process_shutdown::{ProcessOwnerShutdownOutcome, ShutdownDeadline};

use super::{
    PlaylistController, PlaylistRuntime, PlaylistViewModel, PlaylistViewSnapshot, persistence,
    shutdown_report, startup,
};

/// Generation любого lifecycle transition runtime-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PlaylistLifecycleGeneration(pub(super) u64);

/// Generation конкретного renderer/player binding-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PlaylistBindingGeneration(pub(super) u64);

/// Exact binding token будущих AppState/controller callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistRuntimeBinding {
    pub(super) lifecycle_generation: PlaylistLifecycleGeneration,
    pub(super) binding_generation: PlaylistBindingGeneration,
}

/// Renderer-bound attachment: exact port identity плюс immutable controller view.
#[derive(Debug, Clone)]
pub(crate) struct PlaylistAppStateAttachment {
    binding: PlaylistRuntimeBinding,
    view_model: PlaylistViewModel,
}

impl PlaylistAppStateAttachment {
    /// Возвращает exact binding для correlation callbacks текущего `AppState`.
    pub(crate) const fn binding(&self) -> PlaylistRuntimeBinding {
        self.binding
    }

    /// Возвращает cheap-clone read-only snapshot без mutable доступа к controller-у.
    pub(crate) fn view_model(&self) -> PlaylistViewModel {
        self.view_model.clone()
    }

    /// Заменяет только immutable view при сохранении exact binding identity.
    pub(crate) fn replace_view_model(&mut self, view_model: PlaylistViewModel) {
        self.view_model = view_model;
    }
}

impl PlaylistRuntimeBinding {
    /// Передаёт controller-у exact player binding generation после `Installed`.
    pub(crate) const fn binding_generation(self) -> PlaylistBindingGeneration {
        self.binding_generation
    }
}

/// Почему callback нельзя применять к текущему runtime binding-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Session 11A начнёт применять этот typed rejection на callbacks.
pub(crate) enum PlaylistBindingRejection {
    /// Runtime сейчас suspended и renderer/player binding отсутствует.
    Suspended,
    /// Callback относится к предыдущей lifecycle/binding generation.
    StaleGeneration,
    /// Runtime уже начал либо завершил process shutdown.
    ShuttingDown,
}

/// Текущая lifecycle фаза process owner-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaylistRuntimeLifecycle {
    Suspended,
    Bound(PlaylistRuntimeBinding),
    ShuttingDown,
    Shutdown,
}

/// Полный typed отчёт всех process-lifetime playlist owners.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistShutdownReport {
    pub(crate) ui_interaction: ProcessOwnerShutdownOutcome,
    pub(crate) import_io: ProcessOwnerShutdownOutcome,
    pub(crate) url_import: ProcessOwnerShutdownOutcome,
    pub(crate) export_io: ProcessOwnerShutdownOutcome,
    pub(crate) prepared_next: ProcessOwnerShutdownOutcome,
    pub(crate) media_open: ProcessOwnerShutdownOutcome,
    pub(crate) startup: startup::PlaylistStartupShutdownOutcome,
    pub(crate) persistence: persistence::PlaylistPersistenceShutdownOutcome,
    pub(crate) resume_persistence: playlist_state::ResumeWorkerShutdownOutcome,
}

/// Результат terminal API с общей абсолютной границей времени.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistTerminalShutdownOutcome {
    /// Все owners терминальны, filesystem outcome успешен.
    Completed(PlaylistShutdownReport),
    /// Runtime уже полностью завершён предыдущим вызовом.
    AlreadyCompleted,
    /// Хотя бы один writable/async поток не подтверждён; lease освобождать нельзя.
    ExitRequired(PlaylistShutdownReport),
    /// Все потоки терминальны, но panic либо persistence failure сохранены типизированно.
    Failed(PlaylistShutdownReport),
}

/// Пока controller отсутствует, progress/completion slots несут neutral marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistOwnerProgress;

/// Neutral terminal marker playlist owner-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistOwnerCompletion;

/// Cloneable worker-side ports, принадлежащие process runtime-у.
#[derive(Clone)]
#[allow(dead_code)] // Ports удерживают process boundary независимо от renderer lifecycle.
pub(crate) struct PlaylistOwnerPorts {
    pub(super) publisher: OwnerMailboxPublisher<PlaylistOwnerProgress, PlaylistOwnerCompletion>,
    pub(super) admission_open: Arc<AtomicBool>,
}

impl PlaylistOwnerPorts {
    /// Публикует latest progress только пока shutdown gate допускает работу.
    pub(super) fn publish_progress(&self) -> bool {
        if !self.admission_open.load(Ordering::Acquire) {
            return false;
        }
        !matches!(
            self.publisher
                .publish_progress(PlaylistOwnerProgress)
                .wake_delivery(),
            WakeDelivery::EventLoopClosed
        )
    }
}

impl PlaylistRuntime {
    /// Создаёт новый exact binding после успешного AppState recreation.
    pub(crate) fn bind_resumed_app_state(&mut self) -> Option<PlaylistRuntimeBinding> {
        if matches!(
            self.lifecycle,
            PlaylistRuntimeLifecycle::ShuttingDown | PlaylistRuntimeLifecycle::Shutdown
        ) {
            return None;
        }

        self.lifecycle_generation.0 = self
            .lifecycle_generation
            .0
            .checked_add(1)
            .expect("playlist lifecycle generation overflow during bind");
        self.next_binding_generation.0 = self
            .next_binding_generation
            .0
            .checked_add(1)
            .expect("playlist binding generation overflow during bind");
        let binding = PlaylistRuntimeBinding {
            lifecycle_generation: self.lifecycle_generation,
            binding_generation: self.next_binding_generation,
        };
        self.lifecycle = PlaylistRuntimeLifecycle::Bound(binding);
        Some(binding)
    }

    /// Снимает только AppState binding; process owner, ports и load gate сохраняются.
    pub(crate) fn suspend_app_state_binding(&mut self) {
        if matches!(self.lifecycle, PlaylistRuntimeLifecycle::Bound(_)) {
            self.lifecycle_generation.0 = self
                .lifecycle_generation
                .0
                .checked_add(1)
                .expect("playlist lifecycle generation overflow during suspend");
            self.lifecycle = PlaylistRuntimeLifecycle::Suspended;
            if let Some(controller) = self.controller.as_mut() {
                controller.cancel_next_item_preload_plan();
            }
            self.prepared_next
                .cancel(player_core::MediaInstallCancellationCause::StructuralInvalidation);
            self.media_open.suspend_player_binding();
        }
    }

    /// Привязывает exact ordered player control stream нового renderer-bound AppState.
    pub(crate) fn attach_player_sender(&mut self, sender: player_core::PlayerCommandSender) {
        self.media_open.attach_player(sender);
    }

    /// Проверяет exact generation до применения callback-а.
    pub(crate) fn validate_binding(
        &self,
        binding: PlaylistRuntimeBinding,
    ) -> Result<(), PlaylistBindingRejection> {
        match self.lifecycle {
            PlaylistRuntimeLifecycle::Bound(current) if current == binding => Ok(()),
            PlaylistRuntimeLifecycle::Bound(_) => Err(PlaylistBindingRejection::StaleGeneration),
            PlaylistRuntimeLifecycle::Suspended => Err(PlaylistBindingRejection::Suspended),
            PlaylistRuntimeLifecycle::ShuttingDown | PlaylistRuntimeLifecycle::Shutdown => {
                Err(PlaylistBindingRejection::ShuttingDown)
            }
        }
    }

    /// Неблокирующе опустошает mailbox owners в прежнем lossless порядке.
    pub(crate) fn drain_owner_mailbox(&mut self) -> bool {
        let owner_changed = self.owner_receiver.drain().has_payload();
        let media_open_changed = self.media_open.drain();
        let dialog_changed = self.drain_playlist_file_dialog();
        let import_changed = self.drain_playlist_import_job();
        let url_import_changed = self.drain_playlist_url_import_job();
        let export_changed = self.drain_playlist_export_job();
        owner_changed
            || media_open_changed
            || dialog_changed
            || import_changed
            || url_import_changed
            || export_changed
    }

    /// Cheap-clone read-only snapshot для renderer-bound AppState/playlist UI port-а.
    #[allow(
        dead_code,
        reason = "read-only AppState attachment lands with playlist UI"
    )]
    pub(crate) fn playlist_view_snapshot(&self) -> Arc<PlaylistViewSnapshot> {
        static EMPTY_VIEW: LazyLock<Arc<PlaylistViewSnapshot>> =
            LazyLock::new(|| Arc::new(PlaylistViewSnapshot::initial(&PlaylistQueue::new())));
        self.controller
            .as_ref()
            .map(PlaylistController::view_snapshot)
            .unwrap_or_else(|| Arc::clone(&EMPTY_VIEW))
    }

    /// Создаёт renderer-bound attachment только для текущего exact binding-а.
    pub(crate) fn app_state_attachment(
        &self,
        binding: PlaylistRuntimeBinding,
    ) -> Result<PlaylistAppStateAttachment, PlaylistBindingRejection> {
        self.validate_binding(binding)?;
        Ok(PlaylistAppStateAttachment {
            binding,
            view_model: self.playlist_view_model(),
        })
    }

    /// Controller остаётся process owner-ом; mutable facade нужен orchestration layer-у.
    #[allow(
        dead_code,
        reason = "controller facade is consumed by Session 11B orchestration"
    )]
    pub(crate) fn playlist_controller(&self) -> Option<&PlaylistController> {
        self.controller.as_ref()
    }

    /// Закрывает admission и последовательно завершает owners в одном общем бюджете.
    ///
    /// Порядок намеренный: сначала отменяется player/media preparation, затем
    /// read/quarantine startup job, последним выполняется committed-only state flush.
    pub(crate) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> PlaylistTerminalShutdownOutcome {
        if matches!(self.lifecycle, PlaylistRuntimeLifecycle::Shutdown) {
            return PlaylistTerminalShutdownOutcome::AlreadyCompleted;
        }

        self.admission_open.store(false, Ordering::Release);
        self.lifecycle = PlaylistRuntimeLifecycle::ShuttingDown;
        self.removal_undo = None;
        self.supersede_playlist_import_flow();
        self.replacement_confirmation.cancel();
        if let Some(controller) = self.controller.as_mut() {
            controller.release_detached_tombstone_for_shutdown();
        }
        self.discovery.begin_shutdown();

        let ui_interaction = self.ui_interaction.shutdown_until(deadline);
        let import_io = self.import_io.shutdown_until(deadline);
        let url_import = self.url_import.shutdown_until(deadline);
        self.export_io.cancel_active();
        let export_io = self.export_io.shutdown_until(deadline);
        let prepared_next = self.prepared_next.shutdown_until(deadline);
        let media_open = self.media_open.shutdown_until(deadline);
        let startup = self.startup.shutdown_until(deadline);
        let persistence = self
            .persistence
            .shutdown_until(self.controller.as_ref(), deadline);
        let resume_persistence = self.resume_persistence.shutdown_until(deadline);
        let report = PlaylistShutdownReport {
            ui_interaction,
            import_io,
            url_import,
            export_io,
            prepared_next,
            media_open,
            startup,
            persistence,
            resume_persistence,
        };

        if report.requires_process_exit() {
            return PlaylistTerminalShutdownOutcome::ExitRequired(report);
        }
        self.lifecycle = PlaylistRuntimeLifecycle::Shutdown;
        if report.has_terminal_failure() {
            PlaylistTerminalShutdownOutcome::Failed(report)
        } else {
            PlaylistTerminalShutdownOutcome::Completed(report)
        }
    }

    #[cfg(test)]
    pub(super) fn owner_ports(&self) -> PlaylistOwnerPorts {
        self.owner_ports.clone()
    }

    #[cfg(test)]
    pub(super) fn load_gate(&self) -> super::PlaylistLoadGateState {
        self.load_gate
    }
}

impl PlaylistShutdownReport {
    /// Любой незавершённый thread owner требует немедленного process exit при удержанном lease.
    pub(super) fn requires_process_exit(self) -> bool {
        matches!(
            self.ui_interaction,
            ProcessOwnerShutdownOutcome::TimedOut { .. }
                | ProcessOwnerShutdownOutcome::ThreadPanicked {
                    pending_threads: 1..,
                    ..
                }
        ) || matches!(
            self.import_io,
            ProcessOwnerShutdownOutcome::TimedOut { .. }
                | ProcessOwnerShutdownOutcome::ThreadPanicked {
                    pending_threads: 1..,
                    ..
                }
        ) || matches!(
            self.url_import,
            ProcessOwnerShutdownOutcome::TimedOut { .. }
                | ProcessOwnerShutdownOutcome::ThreadPanicked {
                    pending_threads: 1..,
                    ..
                }
        ) || matches!(
            self.export_io,
            ProcessOwnerShutdownOutcome::TimedOut { .. }
                | ProcessOwnerShutdownOutcome::ThreadPanicked {
                    pending_threads: 1..,
                    ..
                }
        ) || matches!(
            self.prepared_next,
            ProcessOwnerShutdownOutcome::TimedOut { .. }
                | ProcessOwnerShutdownOutcome::ThreadPanicked {
                    pending_threads: 1..,
                    ..
                }
        ) || matches!(
            self.media_open,
            ProcessOwnerShutdownOutcome::TimedOut { .. }
                | ProcessOwnerShutdownOutcome::ThreadPanicked {
                    pending_threads: 1..,
                    ..
                }
        ) || matches!(
            self.startup,
            startup::PlaylistStartupShutdownOutcome::TimedOut
        ) || matches!(
            self.persistence,
            persistence::PlaylistPersistenceShutdownOutcome::TimedOut { .. }
        ) || matches!(
            self.resume_persistence,
            playlist_state::ResumeWorkerShutdownOutcome::TimedOut
        )
    }

    /// Отличает завершившийся с ошибкой owner от незавершённого владельца процесса.
    fn has_terminal_failure(self) -> bool {
        let ui_failed = matches!(
            self.ui_interaction,
            ProcessOwnerShutdownOutcome::ThreadPanicked { .. }
        );
        let import_failed = matches!(
            self.import_io,
            ProcessOwnerShutdownOutcome::ThreadPanicked { .. }
        );
        let url_import_failed = matches!(
            self.url_import,
            ProcessOwnerShutdownOutcome::ThreadPanicked { .. }
        );
        let export_failed = matches!(
            self.export_io,
            ProcessOwnerShutdownOutcome::ThreadPanicked { .. }
        );
        let media_failed = matches!(
            self.media_open,
            ProcessOwnerShutdownOutcome::ThreadPanicked { .. }
        );
        let prepared_next_failed = matches!(
            self.prepared_next,
            ProcessOwnerShutdownOutcome::ThreadPanicked { .. }
        );
        let startup_failed = shutdown_report::startup_failed(self.startup);
        let persistence_failed = match self.persistence {
            persistence::PlaylistPersistenceShutdownOutcome::WriterUnavailable { .. }
            | persistence::PlaylistPersistenceShutdownOutcome::SnapshotCaptureFailed { .. }
            | persistence::PlaylistPersistenceShutdownOutcome::ThreadPanicked(_) => true,
            persistence::PlaylistPersistenceShutdownOutcome::Completed(completion) => {
                shutdown_report::shutdown_persistence_failed(completion.persistence)
            }
            persistence::PlaylistPersistenceShutdownOutcome::CompletedWithoutWorker { .. }
            | persistence::PlaylistPersistenceShutdownOutcome::AlreadyCompleted
            | persistence::PlaylistPersistenceShutdownOutcome::TimedOut { .. } => false,
        };
        let resume_persistence_failed = match self.resume_persistence {
            playlist_state::ResumeWorkerShutdownOutcome::Completed {
                final_report: Some(report),
            } => !matches!(report.outcome, playlist_state::AtomicWriteOutcome::Durable),
            playlist_state::ResumeWorkerShutdownOutcome::WorkerUnavailable => true,
            playlist_state::ResumeWorkerShutdownOutcome::Completed { final_report: None }
            | playlist_state::ResumeWorkerShutdownOutcome::AlreadyCompleted
            | playlist_state::ResumeWorkerShutdownOutcome::TimedOut => false,
        };
        ui_failed
            || import_failed
            || url_import_failed
            || export_failed
            || prepared_next_failed
            || media_failed
            || startup_failed
            || persistence_failed
            || resume_persistence_failed
    }
}
