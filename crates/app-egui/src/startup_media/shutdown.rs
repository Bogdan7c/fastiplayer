//! Bounded process shutdown для startup media jobs.

use std::sync::atomic::Ordering;
use std::thread::JoinHandle;

use tracing::warn;

use super::{DirectMediaStartupJob, NativeHlsStartupJob, StartupMediaController, YtDlpStartupJob};
use crate::process_shutdown::{
    FinishedThreadJoin, ProcessOwnerShutdownOutcome, ShutdownDeadline, join_thread_until,
};

impl YtDlpStartupJob {
    /// Cooperative-cancel-ит resolver и bounded-join-ит finished handle.
    fn shutdown_until(&mut self, deadline: ShutdownDeadline) -> ProcessOwnerShutdownOutcome {
        self.cancellation_requested.store(true, Ordering::Release);
        self.source_cancellation.cancel();
        shutdown_single_thread(&mut self.join_handle, deadline)
    }
}

impl Drop for YtDlpStartupJob {
    fn drop(&mut self) {
        self.cancellation_requested.store(true, Ordering::Release);
        join_startup_thread_on_fail_safe_drop(&mut self.join_handle, "YtDlp startup resolver");
    }
}

impl DirectMediaStartupJob {
    /// Cooperative-cancel-ит opener и bounded-join-ит finished handle.
    fn shutdown_until(&mut self, deadline: ShutdownDeadline) -> ProcessOwnerShutdownOutcome {
        self.cancellation_requested.store(true, Ordering::Release);
        shutdown_single_thread(&mut self.join_handle, deadline)
    }
}

impl Drop for DirectMediaStartupJob {
    fn drop(&mut self) {
        self.cancellation_requested.store(true, Ordering::Release);
        join_startup_thread_on_fail_safe_drop(&mut self.join_handle, "Direct media startup opener");
    }
}

impl NativeHlsStartupJob {
    fn shutdown_until(&mut self, deadline: ShutdownDeadline) -> ProcessOwnerShutdownOutcome {
        self.cancellation_requested.store(true, Ordering::Release);
        self.source_cancellation.cancel();
        shutdown_single_thread(&mut self.join_handle, deadline)
    }
}

impl Drop for NativeHlsStartupJob {
    fn drop(&mut self) {
        self.cancellation_requested.store(true, Ordering::Release);
        // После успешного join transport token уже принадлежит переданному
        // demux runtime. Обычный Drop completed startup owner-а не имеет права
        // отменять установленное media; explicit shutdown выше по-прежнему
        // cancel-ит token до bounded join активного opener-а.
        join_startup_thread_on_fail_safe_drop(&mut self.join_handle, "Native HLS startup opener");
    }
}

impl StartupMediaController {
    /// Закрывает admission и bounded-завершает все принадлежащие startup jobs.
    pub(crate) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> ProcessOwnerShutdownOutcome {
        if self.terminal_shutdown_completed {
            return ProcessOwnerShutdownOutcome::AlreadyCompleted;
        }
        self.terminal_shutdown_started = true;
        self.orchestration.phase = super::StartupMediaPhase::Shutdown;
        self.orchestration.prepared = None;
        self.startup_playlist_pending = false;

        // Все owners сначала теряют admission/apply authority и получают cancel,
        // только после этого общий deadline расходуется на bounded join.
        if let Some(job) = self.yt_dlp_startup_job.as_ref() {
            job.cancellation_requested.store(true, Ordering::Release);
        }
        if let Some(job) = self.direct_media_startup_job.as_ref() {
            job.cancellation_requested.store(true, Ordering::Release);
        }
        if let Some(job) = self.native_hls_startup_job.as_ref() {
            job.cancellation_requested.store(true, Ordering::Release);
            job.source_cancellation.cancel();
        }

        let mut panicked_threads = 0;
        let mut pending_threads = 0;
        if let Some(job) = self.yt_dlp_startup_job.as_mut() {
            accumulate_shutdown_outcome(
                job.shutdown_until(deadline),
                &mut panicked_threads,
                &mut pending_threads,
            );
            if job.join_handle.is_none() {
                self.yt_dlp_startup_job = None;
            }
        }
        if let Some(job) = self.direct_media_startup_job.as_mut() {
            accumulate_shutdown_outcome(
                job.shutdown_until(deadline),
                &mut panicked_threads,
                &mut pending_threads,
            );
            if job.join_handle.is_none() {
                self.direct_media_startup_job = None;
            }
        }
        if let Some(job) = self.native_hls_startup_job.as_mut() {
            accumulate_shutdown_outcome(
                job.shutdown_until(deadline),
                &mut panicked_threads,
                &mut pending_threads,
            );
            if job.join_handle.is_none() {
                self.native_hls_startup_job = None;
            }
        }
        if let Some(job) = self.local_startup_job.as_mut() {
            let outcome = job.shutdown_until(deadline);
            let completed = matches!(
                outcome,
                ProcessOwnerShutdownOutcome::Completed
                    | ProcessOwnerShutdownOutcome::AlreadyCompleted
                    | ProcessOwnerShutdownOutcome::ThreadPanicked {
                        pending_threads: 0,
                        ..
                    }
            );
            accumulate_shutdown_outcome(outcome, &mut panicked_threads, &mut pending_threads);
            if completed {
                self.local_startup_job = None;
            }
        }

        if pending_threads > 0 {
            if panicked_threads > 0 {
                return ProcessOwnerShutdownOutcome::ThreadPanicked {
                    panicked_threads,
                    pending_threads,
                };
            }
            return ProcessOwnerShutdownOutcome::TimedOut { pending_threads };
        }

        self.terminal_shutdown_completed = true;
        if panicked_threads > 0 {
            ProcessOwnerShutdownOutcome::ThreadPanicked {
                panicked_threads,
                pending_threads: 0,
            }
        } else {
            ProcessOwnerShutdownOutcome::Completed
        }
    }

    /// Проверяет single-startup-job invariant и terminal admission.
    pub(super) fn startup_job_admission_error(&self) -> Option<String> {
        if self.terminal_shutdown_started {
            return Some("Startup media shutdown уже начат; новый job запрещён".to_string());
        }
        if self.yt_dlp_startup_job.is_some()
            || self.direct_media_startup_job.is_some()
            || self.native_hls_startup_job.is_some()
            || self.local_startup_job.is_some()
        {
            return Some(
                "Startup media job уже выполняется; параллельный запуск запрещён".to_string(),
            );
        }
        None
    }
}

/// Bounded shutdown одного startup thread-а без преждевременного blocking join.
fn shutdown_single_thread(
    join_handle: &mut Option<JoinHandle<()>>,
    deadline: ShutdownDeadline,
) -> ProcessOwnerShutdownOutcome {
    match join_thread_until(join_handle, deadline) {
        FinishedThreadJoin::AlreadyJoined | FinishedThreadJoin::Joined => {
            ProcessOwnerShutdownOutcome::Completed
        }
        FinishedThreadJoin::StillRunning => {
            ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 1 }
        }
        FinishedThreadJoin::Panicked => ProcessOwnerShutdownOutcome::ThreadPanicked {
            panicked_threads: 1,
            pending_threads: 0,
        },
    }
}

/// Агрегирует typed outcome без сведения panic/timeout в общий bool.
fn accumulate_shutdown_outcome(
    outcome: ProcessOwnerShutdownOutcome,
    panicked_threads: &mut usize,
    pending_threads: &mut usize,
) {
    match outcome {
        ProcessOwnerShutdownOutcome::Completed | ProcessOwnerShutdownOutcome::AlreadyCompleted => {}
        ProcessOwnerShutdownOutcome::TimedOut {
            pending_threads: pending,
        } => *pending_threads += pending,
        ProcessOwnerShutdownOutcome::ThreadPanicked {
            panicked_threads: panicked,
            pending_threads: pending,
        } => {
            *panicked_threads += panicked;
            *pending_threads += pending;
        }
    }
}

/// Последний safety net: explicit shutdown обязан убрать handle до Drop.
fn join_startup_thread_on_fail_safe_drop(
    join_handle: &mut Option<JoinHandle<()>>,
    owner_name: &str,
) {
    let Some(join_handle) = join_handle.take() else {
        return;
    };
    if join_handle.join().is_err() {
        warn!(
            owner = owner_name,
            "Startup media thread panic обнаружен во время fail-safe Drop"
        );
    }
}
