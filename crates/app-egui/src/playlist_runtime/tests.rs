use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::app_wake::{AppWakeEvent, AppWakeOwner, WakeEmitter};
use crate::process_shutdown::{ProcessOwnerShutdownOutcome, ShutdownDeadline};

use super::*;

struct CountingEmitter(AtomicUsize);

impl WakeEmitter for CountingEmitter {
    fn emit(&self, _event: AppWakeEvent) -> Result<(), ()> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

fn runtime() -> PlaylistRuntime {
    let emitter = Arc::new(CountingEmitter(AtomicUsize::new(0)));
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::new(AppWakeOwner::PlaylistRuntime, emitter));
    runtime.resolve_missing_state_for_test();
    runtime
}

#[test]
fn suspend_resume_preserves_runtime_and_rejects_stale_binding() {
    let mut runtime = runtime();
    let initial_view = runtime.playlist_view_snapshot();
    let first = runtime.bind_resumed_app_state().unwrap();
    assert_eq!(runtime.validate_binding(first), Ok(()));

    runtime.suspend_app_state_binding();
    assert_eq!(
        runtime.validate_binding(first),
        Err(PlaylistBindingRejection::Suspended)
    );

    let second = runtime.bind_resumed_app_state().unwrap();
    assert_ne!(second, first);
    assert_eq!(
        runtime.validate_binding(first),
        Err(PlaylistBindingRejection::StaleGeneration)
    );
    assert_eq!(runtime.validate_binding(second), Ok(()));
    assert_eq!(
        runtime.load_gate(),
        PlaylistLoadGateState::Open(PlaylistLineagePersistence::Persistent)
    );

    let attachment = runtime
        .app_state_attachment(second)
        .expect("current binding attachment");
    assert_eq!(attachment.binding(), second);
    assert_eq!(attachment.view_model().revision(), initial_view.revision());
    assert!(matches!(
        runtime.app_state_attachment(first),
        Err(PlaylistBindingRejection::StaleGeneration)
    ));
}

#[test]
fn owner_ports_survive_suspend_and_keep_mailbox_reachable() {
    let mut runtime = runtime();
    let ports = runtime.owner_ports();
    runtime.bind_resumed_app_state().unwrap();
    runtime.suspend_app_state_binding();

    assert!(ports.publish_progress());
    assert!(runtime.drain_owner_mailbox());
    assert!(!runtime.drain_owner_mailbox());
}

#[test]
fn inline_url_draft_survives_views_and_confirmation_is_render_only() {
    let mut runtime = runtime();
    runtime.open_playlist_url_editor();
    runtime.update_playlist_url_draft("не URL с token=secret".to_string());

    assert!(runtime.submit_playlist_url_draft(&fastiplayer_config::YtDlpConfig::default()));
    let invalid = runtime.playlist_interaction_model();
    assert!(invalid.url_editor_open);
    assert_eq!(invalid.url_text, "не URL с token=secret");
    assert_eq!(
        invalid.url_safe_error.as_ref().map(|error| error.message()),
        Some("Введите корректный http(s) URL")
    );

    let sensitive = "https://user:password@media.example.test/video.mp4?token=secret";
    runtime.update_playlist_url_draft(sensitive.to_string());
    assert!(runtime.submit_playlist_url_draft(&fastiplayer_config::YtDlpConfig::default()));
    let confirmation = runtime
        .pending_playlist_confirmation()
        .expect("sensitive URL должен ждать typed decision");
    assert_eq!(runtime.playlist_interaction_model().url_text, sensitive);
    assert!(!format!("{confirmation:?}").contains("token=secret"));

    assert!(runtime.submit_playlist_url_draft(&fastiplayer_config::YtDlpConfig::default()));
    let stale_outcome = runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
        intent_id: confirmation.intent_id(),
        decision: QueueReplacementConfirmationDecision::Confirm,
    });
    runtime.finish_url_draft_after_confirmation(&stale_outcome);
    assert_eq!(runtime.playlist_interaction_model().url_text, sensitive);

    let current_confirmation = runtime
        .pending_playlist_confirmation()
        .expect("новый exact intent должен остаться pending");
    let outcome = runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
        intent_id: current_confirmation.intent_id(),
        decision: QueueReplacementConfirmationDecision::Confirm,
    });
    runtime.finish_url_draft_after_confirmation(&outcome);

    let finished = runtime.playlist_interaction_model();
    assert!(!finished.url_editor_open);
    assert!(finished.url_text.is_empty());
    assert_eq!(runtime.controller.queue().top_level_entry_count(), 1);
}

#[test]
fn shutdown_is_bounded_idempotent_and_closes_admission() {
    let mut runtime = runtime();
    let ports = runtime.owner_ports();
    let deadline = ShutdownDeadline::after(Duration::from_secs(1));

    assert!(matches!(
        runtime.shutdown_until(deadline),
        PlaylistTerminalShutdownOutcome::Completed(_)
    ));
    assert!(!ports.publish_progress());
    assert_eq!(
        runtime.shutdown_until(deadline),
        PlaylistTerminalShutdownOutcome::AlreadyCompleted
    );
}

#[test]
fn media_open_timeout_requires_process_exit_without_collapsing_owner_outcomes() {
    let mut runtime = runtime();
    let ports = runtime.owner_ports();
    let report = PlaylistShutdownReport {
        media_open: ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 1 },
        prepared_next: ProcessOwnerShutdownOutcome::Completed,
        ui_interaction: ProcessOwnerShutdownOutcome::Completed,
        import_io: ProcessOwnerShutdownOutcome::Completed,
        url_import: ProcessOwnerShutdownOutcome::Completed,
        export_io: ProcessOwnerShutdownOutcome::Completed,
        startup: startup::PlaylistStartupShutdownOutcome::Completed,
        persistence: persistence::PlaylistPersistenceShutdownOutcome::CompletedWithoutWorker {
            save_block: None,
        },
        resume_persistence: playlist_state::ResumeWorkerShutdownOutcome::AlreadyCompleted,
    };

    assert!(report.requires_process_exit());
    assert_eq!(
        report.media_open,
        ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 1 }
    );

    runtime.admission_open.store(false, Ordering::Release);
    runtime.lifecycle = PlaylistRuntimeLifecycle::ShuttingDown;
    assert!(!ports.publish_progress());
    assert!(runtime.bind_resumed_app_state().is_none());
}
