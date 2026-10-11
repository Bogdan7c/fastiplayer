//! Stop во время стартовой подготовки: отмена токеном, молчаливый итог, никаких fallback-ов.
//!
//! Подготовка эмулируется настоящим job-ом с синтетическим worker-ом: он «висит», пока тест
//! не отпустит его, и затем ведёт себя либо как настоящий cooperative worker (видит флаг
//! отмены и выходит без результата), либо как опоздавший (уже прошёл проверку флага и
//! публикует итог). Оба случая обязаны закончиться без ошибки и без установки.

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::super::*;
use crate::media_open::{PreparationCancelCause, PreparationCancelOutcome};

/// Как worker ведёт себя после того, как тест его отпустил.
#[derive(Clone, Copy)]
enum WorkerAfterRelease {
    /// Видит флаг отмены и выходит, ничего не публикуя (поведение настоящего yt-dlp job-а).
    HonorsCancellation,
    /// Флаг не проверяет и публикует ошибку подготовки (опоздавший итог).
    PublishesFailureAnyway,
}

struct HangingPreparation {
    controller: StartupMediaController,
    release: Arc<AtomicBool>,
}

/// Controller с «зависшей» CLI-подготовкой yt-dlp: target CLI, фаза `Preparing`.
fn controller_with_hanging_cli_preparation(after: WorkerAfterRelease) -> HangingPreparation {
    let wake_port = AppWakePort::disconnected(AppWakeOwner::StartupMedia);
    let (result_publisher, result_receiver) = owner_mailbox(wake_port.clone());
    let release = Arc::new(AtomicBool::new(false));
    let cancellation_requested = Arc::new(AtomicBool::new(false));
    let worker_release = Arc::clone(&release);
    let join_handle: JoinHandle<()> = std::thread::spawn(move || {
        while !worker_release.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        match after {
            // Настоящий worker видит флаг и выходит, ничего не публикуя.
            WorkerAfterRelease::HonorsCancellation => {}
            WorkerAfterRelease::PublishesFailureAnyway => {
                let _ = result_publisher.publish_completion(Err(
                    StartupWebPreparationFailure::unclassified("сеть оборвалась"),
                ));
            }
        }
    });
    let mut orchestration = StartupMediaOrchestration::new(true);
    orchestration.begin_target(StartupMediaTarget::CliReplacement);
    let controller = StartupMediaController {
        wake_port,
        initial_media: None,
        yt_dlp_startup_job: Some(YtDlpStartupJob {
            source_locator: service_ytdlp::parse_yt_dlp_media_locator(
                "https://www.youtube.com/watch?v=test",
            )
            .expect("test locator должен проходить service parse"),
            pending_message: "Подготовка YtDlp stream...".to_string(),
            result_receiver,
            join_handle: Some(join_handle),
            pending_result: None,
            cancellation_requested,
            source_cancellation: source_core::CancellationToken::new(),
        }),
        direct_media_startup_job: None,
        native_hls_startup_job: None,
        native_dash_startup_job: None,
        native_hds_startup_job: None,
        native_smooth_startup_job: None,
        local_startup_job: None,
        startup_playlist_pending: false,
        orchestration,
        cli_url_target_draft: None,
        startup_config: None,
        system_capabilities: None,
        startup_error: None,
        web_display_host: None,
        terminal_shutdown_started: false,
        terminal_shutdown_completed: false,
        preparation_stopped_by_user: false,
    };
    HangingPreparation {
        controller,
        release,
    }
}

/// Ждёт, пока controller освободит слот job-а (worker реально вышел).
fn drain_until_slots_free(controller: &mut StartupMediaController) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while controller.has_pending_startup_job() {
        controller.discard_stopped_preparation_jobs();
        assert!(Instant::now() < deadline, "job не освободил слот за 5 с");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn stop_cancels_hanging_preparation_and_late_failure_stays_silent() {
    let HangingPreparation {
        mut controller,
        release,
    } = controller_with_hanging_cli_preparation(WorkerAfterRelease::PublishesFailureAnyway);
    assert_eq!(controller.orchestration.phase, StartupMediaPhase::Preparing);

    let outcome = controller.cancel_pending_preparation(PreparationCancelCause::UserStop);

    assert_eq!(outcome, PreparationCancelOutcome::Cancelled);
    let job = controller
        .yt_dlp_startup_job
        .as_ref()
        .expect("слот держится до реального выхода worker-а, без join на UI-потоке");
    assert!(
        job.cancellation_requested.load(Ordering::Acquire),
        "флаг отмены поднят"
    );
    assert!(
        job.source_cancellation.is_cancelled(),
        "токен сети/yt-dlp прерван"
    );
    assert_eq!(controller.orchestration.phase, StartupMediaPhase::Idle);
    assert!(
        !controller.orchestration.has_pending_work(),
        "запланированный CLI-старт больше не ждёт результата"
    );
    assert!(
        !controller.discard_stopped_preparation_jobs(),
        "пока worker висит, слот не освобождается"
    );

    release.store(true, Ordering::Release);
    drain_until_slots_free(&mut controller);

    assert!(
        controller.startup_error.is_none(),
        "опоздавшая ошибка не показана"
    );
    assert!(
        !controller.orchestration.cli_failed,
        "CLI-цель не помечена проваленной"
    );
    assert!(
        controller.orchestration.prepared.is_none(),
        "ничего не готово к установке"
    );
    assert!(
        controller.orchestration.pending_install.is_none(),
        "установка не начиналась"
    );
    assert!(
        controller.orchestration.restored_fallback_started,
        "restored fallback запрещён: очередь не перебирается"
    );
    assert_eq!(controller.orchestration.phase, StartupMediaPhase::Idle);
    assert_eq!(controller.pending_message(), None);
}

#[test]
fn stop_with_cooperative_worker_does_not_report_missing_result_as_error() {
    // Настоящий worker после отмены выходит, ничего не опубликовав; без Stop это
    // «завершился без результата» и ошибка. После Stop это не ошибка.
    let HangingPreparation {
        mut controller,
        release,
    } = controller_with_hanging_cli_preparation(WorkerAfterRelease::HonorsCancellation);

    assert_eq!(
        controller.cancel_pending_preparation(PreparationCancelCause::UserStop),
        PreparationCancelOutcome::Cancelled
    );
    release.store(true, Ordering::Release);
    drain_until_slots_free(&mut controller);

    assert!(controller.startup_error.is_none());
    assert!(!controller.has_pending_startup_job());
    assert!(!controller.orchestration.cli_failed);
}

#[test]
fn repeated_stop_after_cancellation_is_nothing_pending_for_freed_slots() {
    let HangingPreparation {
        mut controller,
        release,
    } = controller_with_hanging_cli_preparation(WorkerAfterRelease::HonorsCancellation);
    controller.cancel_pending_preparation(PreparationCancelCause::UserStop);
    release.store(true, Ordering::Release);
    drain_until_slots_free(&mut controller);

    assert_eq!(
        controller.cancel_pending_preparation(PreparationCancelCause::UserStop),
        PreparationCancelOutcome::NothingPending,
        "второй Stop ничего не находит"
    );
}

#[test]
fn stop_without_preparation_changes_nothing() {
    let wake_port = AppWakePort::disconnected(AppWakeOwner::StartupMedia);
    let mut controller = StartupMediaController::with_wake_port(None, None, wake_port);

    let outcome = controller.cancel_pending_preparation(PreparationCancelCause::UserStop);

    assert_eq!(outcome, PreparationCancelOutcome::NothingPending);
    assert!(!controller.preparation_stopped_by_user);
    assert!(
        !controller.orchestration.restored_fallback_started,
        "Stop без подготовки не запрещает будущий restore"
    );
    assert_eq!(
        controller.orchestration.phase,
        StartupMediaPhase::WaitingForRuntime
    );
}
