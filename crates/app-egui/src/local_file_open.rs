use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};

use pollster::FutureExt as _;
use rustiplayer_config::PlayerDemuxConfig;
use tracing::{debug, warn};
use winit::window::Window;

use crate::app_wake::{
    AppWakePort, CompletionPublishError, OwnerMailboxReceiver, WakeDelivery, owner_mailbox,
};
use crate::local_media;
use crate::media_open::{PreparedLocalOpenResult, prepare_local_open};
use crate::process_shutdown::{
    FinishedThreadJoin, ProcessOwnerShutdownOutcome, ShutdownDeadline, join_finished_thread,
    join_thread_until,
};

/// Финальный результат одной фазы local picker/preparation pipeline.
pub(crate) enum LocalFileOpenResult {
    /// Пользователь закрыл dialog без выбора файла.
    Cancelled,

    /// Picker вернул target; media/path I/O ещё не начинался.
    Selected { path: PathBuf },

    /// Файл выбран и успешно подготовлен вне UI thread-а.
    Prepared {
        prepared: Box<PreparedLocalOpenResult>,
    },

    /// Файл выбран, но adapter не смог подготовить demuxer.
    PrepareFailed { path: PathBuf, error: String },

    /// Background job завершился раньше, чем отправил нормальный результат.
    JobFailed { error: String },
}

/// Один UI drain latest progress и exactly-once completion.
pub(crate) struct LocalFileOpenDrain {
    /// Последний выбранный path, для которого background job начал preparation.
    pub(crate) preparing_path: Option<PathBuf>,
    /// Финальный результат, если worker уже завершил job.
    pub(crate) completion: Option<LocalFileOpenResult>,
}

/// Typed результат возврата suspend-transferred job в новый `AppState`.
pub(crate) enum LocalFileOpenRestoreOutcome {
    /// Новый renderer owner принял exact job ownership.
    Restored,

    /// Новый owner уже содержит job; отвергнутый handle возвращён process owner-у.
    ExistingJob(Box<LocalFileOpenJob>),
}

impl LocalFileOpenDrain {
    /// `true` означает реальную видимую мутацию shell status/media state.
    pub(crate) fn has_payload(&self) -> bool {
        self.preparing_path.is_some() || self.completion.is_some()
    }
}

/// Фоновый job ровно одной фазы выбора либо подготовки локального файла.
pub(crate) struct LocalFileOpenJob {
    /// Owner mailbox хранит latest path отдельно от lossless terminal result.
    mailbox_receiver: OwnerMailboxReceiver<PathBuf, LocalFileOpenResult>,

    /// JoinHandle нужен для cleanup после финального результата.
    join_handle: Option<JoinHandle<()>>,

    /// Completion ждёт exact worker exit, чтобы UI не потерял join authority.
    pending_completion: Option<LocalFileOpenResult>,

    /// Cooperative terminal cancellation между dialog и дорогой preparation.
    cancellation_requested: Arc<AtomicBool>,

    /// Тот же cooperative state передаётся source/demux scan-ам.
    source_cancellation: source_core::CancellationToken,

    /// `true`, когда worker уже joined и повторный shutdown является no-op.
    terminal_shutdown_completed: bool,
}

impl LocalFileOpenJob {
    /// Запускает только async file dialog; выбранный target вернётся на admission boundary.
    pub(crate) fn spawn_picker(
        window: &Window,
        wake_port: AppWakePort,
    ) -> std::result::Result<Self, String> {
        let (mailbox_publisher, mailbox_receiver) = owner_mailbox(wake_port);
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let source_cancellation = source_core::CancellationToken::new();
        let worker_cancellation_requested = Arc::clone(&cancellation_requested);
        let file_dialog_future = rfd::AsyncFileDialog::new()
            .set_parent(window)
            .add_filter(
                "Supported Media",
                local_media::SUPPORTED_LOCAL_MEDIA_EXTENSIONS,
            )
            .add_filter("WebM / Matroska", &["webm", "mkv"])
            .add_filter("All Files", &["*"])
            .pick_file();

        let join_handle = thread::Builder::new()
            .name("local-file-picker".to_string())
            .spawn(move || {
                let Some(file_handle) = file_dialog_future.block_on() else {
                    publish_local_file_completion(
                        &mailbox_publisher,
                        LocalFileOpenResult::Cancelled,
                    );
                    return;
                };

                if worker_cancellation_requested.load(Ordering::Acquire) {
                    publish_local_file_completion(
                        &mailbox_publisher,
                        LocalFileOpenResult::Cancelled,
                    );
                    return;
                }

                let selected_path = file_handle.path().to_path_buf();
                publish_local_file_completion(
                    &mailbox_publisher,
                    LocalFileOpenResult::Selected {
                        path: selected_path,
                    },
                );
            })
            .map_err(|error| format!("Не удалось запустить local file picker job: {error}"))?;

        Ok(Self::from_parts(
            mailbox_receiver,
            join_handle,
            cancellation_requested,
            source_cancellation,
        ))
    }

    /// Запускает media preparation только для intent-а, уже прошедшего D79 admission.
    pub(crate) fn spawn_preparation(
        selected_path: PathBuf,
        demux_config: PlayerDemuxConfig,
        wake_port: AppWakePort,
    ) -> std::result::Result<Self, String> {
        let (mailbox_publisher, mailbox_receiver) = owner_mailbox(wake_port);
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let source_cancellation = source_core::CancellationToken::new();
        let worker_source_cancellation = source_cancellation.clone();
        let worker_cancellation_requested = Arc::clone(&cancellation_requested);
        let join_handle = thread::Builder::new()
            .name("local-file-preparation".to_string())
            .spawn(move || {
                if worker_cancellation_requested.load(Ordering::Acquire) {
                    publish_local_file_completion(
                        &mailbox_publisher,
                        LocalFileOpenResult::Cancelled,
                    );
                    return;
                }

                let progress_outcome = mailbox_publisher.publish_progress(selected_path.clone());
                if progress_outcome.wake_delivery == WakeDelivery::EventLoopClosed {
                    debug!("Event loop закрыт; local-file progress оставлен без wake retry");
                }

                let prepare_result = prepare_local_open(
                    &selected_path,
                    &demux_config,
                    None,
                    worker_source_cancellation,
                    || worker_cancellation_requested.load(Ordering::Acquire),
                )
                .map(|prepared| LocalFileOpenResult::Prepared {
                    prepared: Box::new(prepared),
                })
                .unwrap_or_else(|error| LocalFileOpenResult::PrepareFailed {
                    path: selected_path,
                    error: format!("{error:#}"),
                });

                if worker_cancellation_requested.load(Ordering::Acquire) {
                    publish_local_file_completion(
                        &mailbox_publisher,
                        LocalFileOpenResult::Cancelled,
                    );
                    return;
                }
                publish_local_file_completion(&mailbox_publisher, prepare_result);
            })
            .map_err(|error| format!("Не удалось запустить local file preparation job: {error}"))?;

        Ok(Self::from_parts(
            mailbox_receiver,
            join_handle,
            cancellation_requested,
            source_cancellation,
        ))
    }

    fn from_parts(
        mailbox_receiver: OwnerMailboxReceiver<PathBuf, LocalFileOpenResult>,
        join_handle: JoinHandle<()>,
        cancellation_requested: Arc<AtomicBool>,
        source_cancellation: source_core::CancellationToken,
    ) -> Self {
        Self {
            mailbox_receiver,
            join_handle: Some(join_handle),
            pending_completion: None,
            cancellation_requested,
            source_cancellation,
            terminal_shutdown_completed: false,
        }
    }

    /// Неблокирующе забирает оба независимых slot-а и никогда не ждёт join.
    pub(crate) fn drain(&mut self) -> LocalFileOpenDrain {
        let mailbox_drain = self.mailbox_receiver.drain();
        if mailbox_drain.completion.is_some() {
            self.pending_completion = mailbox_drain.completion;
        }

        let completion = match join_finished_thread(&mut self.join_handle) {
            FinishedThreadJoin::Joined => self.pending_completion.take().or_else(|| {
                Some(LocalFileOpenResult::JobFailed {
                    error: "Local file open job завершился без результата".to_string(),
                })
            }),
            FinishedThreadJoin::Panicked => {
                self.pending_completion = None;
                Some(LocalFileOpenResult::JobFailed {
                    error: "Local file open job завершился panic".to_string(),
                })
            }
            FinishedThreadJoin::AlreadyJoined if self.pending_completion.is_some() => {
                self.pending_completion.take()
            }
            FinishedThreadJoin::AlreadyJoined | FinishedThreadJoin::StillRunning => {
                if mailbox_drain.producer_disconnected_without_completion
                    && self.join_handle.is_none()
                {
                    Some(LocalFileOpenResult::JobFailed {
                        error: "Local file open job потерял terminal result".to_string(),
                    })
                } else {
                    None
                }
            }
        };

        LocalFileOpenDrain {
            preparing_path: mailbox_drain.latest_progress,
            completion,
        }
    }

    /// Закрывает admission и cooperative-cancel-ит job перед bounded join.
    pub(crate) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> ProcessOwnerShutdownOutcome {
        if self.terminal_shutdown_completed {
            return ProcessOwnerShutdownOutcome::AlreadyCompleted;
        }

        self.cancellation_requested.store(true, Ordering::Release);
        self.source_cancellation.cancel();

        match join_thread_until(&mut self.join_handle, deadline) {
            FinishedThreadJoin::AlreadyJoined | FinishedThreadJoin::Joined => {
                self.terminal_shutdown_completed = true;
                ProcessOwnerShutdownOutcome::Completed
            }
            FinishedThreadJoin::StillRunning => {
                ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 1 }
            }
            FinishedThreadJoin::Panicked => {
                self.terminal_shutdown_completed = true;
                ProcessOwnerShutdownOutcome::ThreadPanicked {
                    panicked_threads: 1,
                    pending_threads: 0,
                }
            }
        }
    }
}

impl Drop for LocalFileOpenJob {
    fn drop(&mut self) {
        self.cancellation_requested.store(true, Ordering::Release);
        self.source_cancellation.cancel();
        let Some(join_handle) = self.join_handle.take() else {
            return;
        };

        // Это fail-safe, а не process shutdown path. AppShell обязан передать job
        // через suspend boundary и вызвать bounded shutdown до terminal Drop.
        if join_handle.join().is_err() {
            warn!("Local file open job panic обнаружен во время fail-safe Drop");
        }
    }
}

/// Публикует terminal без blocking send; повторный terminal является invariant error.
fn publish_local_file_completion(
    publisher: &crate::app_wake::OwnerMailboxPublisher<PathBuf, LocalFileOpenResult>,
    completion: LocalFileOpenResult,
) {
    match publisher.publish_completion(completion) {
        Ok(WakeDelivery::EventLoopClosed) => {
            debug!("Event loop закрыт; local-file terminal оставлен без wake retry");
        }
        Ok(WakeDelivery::Armed | WakeDelivery::Coalesced) => {}
        Err(CompletionPublishError::AlreadyPublished) => {
            warn!("Local file open job попытался опубликовать второй terminal result");
        }
    }
}

/// Форматирует pending overlay после выбора файла.
pub(crate) fn preparing_local_file_message(path: &std::path::Path) -> String {
    let safe_label = crate::playlist_runtime::safe_local_open_label(path);
    format!("Подготовка media-файла: {safe_label}")
}

/// Форматирует shell-level ошибку подготовки локального файла.
pub(crate) fn local_file_prepare_error_message(path: &std::path::Path, error: &str) -> String {
    let safe_label = crate::playlist_runtime::safe_local_open_label(path);
    format!("Ошибка открытия media-файла {safe_label}: {error}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::Duration;

    use super::{
        LocalFileOpenDrain, LocalFileOpenJob, LocalFileOpenResult,
        local_file_prepare_error_message, preparing_local_file_message,
    };
    use crate::app_wake::{AppWakeOwner, AppWakePort, owner_mailbox};
    use crate::process_shutdown::{ProcessOwnerShutdownOutcome, ShutdownDeadline};

    /// Создаёт focused job без открытия platform dialog-а.
    fn test_job(join_handle: std::thread::JoinHandle<()>) -> LocalFileOpenJob {
        let (_publisher, mailbox_receiver) =
            owner_mailbox(AppWakePort::disconnected(AppWakeOwner::LocalFileOpen));
        LocalFileOpenJob {
            mailbox_receiver,
            join_handle: Some(join_handle),
            pending_completion: None,
            cancellation_requested: Arc::new(AtomicBool::new(false)),
            source_cancellation: source_core::CancellationToken::new(),
            terminal_shutdown_completed: false,
        }
    }

    /// Проверяет, что terminal cancel считается видимой exactly-once мутацией.
    #[test]
    fn cancelled_result_is_visible_completion_without_media() {
        let drain = LocalFileOpenDrain {
            preparing_path: None,
            completion: Some(LocalFileOpenResult::Cancelled),
        };

        assert!(drain.has_payload());
        assert!(matches!(
            drain.completion,
            Some(LocalFileOpenResult::Cancelled)
        ));
    }

    /// Проверяет, что prepare error не показывает ни filename, ни parent path.
    #[test]
    fn prepare_error_uses_generic_label_without_selected_path() {
        let path = PathBuf::from("/tmp/broken-media.webm");
        let error_message = local_file_prepare_error_message(&path, "demux failed");

        assert!(error_message.contains("локальный media-файл"));
        assert!(!error_message.contains("broken-media.webm"));
        assert!(!error_message.contains("/tmp/"));
        assert!(error_message.contains("demux failed"));
    }

    /// Проверяет pending-текст без раскрытия native/foreign path units.
    #[test]
    fn preparing_message_uses_generic_label_without_path_units() {
        let path = PathBuf::from("/tmp/clip.mkv");

        assert_eq!(
            preparing_local_file_message(&path),
            "Подготовка media-файла: локальный media-файл"
        );
    }

    /// Пустой queued wake event не должен объявлять видимую мутацию.
    #[test]
    fn empty_drain_is_not_visible_mutation() {
        let drain = LocalFileOpenDrain {
            preparing_path: None,
            completion: None,
        };

        assert!(!drain.has_payload());
    }

    #[test]
    fn shutdown_timeout_retains_handle_and_later_reaps_it() {
        let release = Arc::new(shutdown_observation::ObservedRelease::new());
        let worker_release = Arc::clone(&release);
        let mut job = test_job(std::thread::spawn(move || {
            while !worker_release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
        }));
        let source_cancellation = job.source_cancellation.clone();

        assert_eq!(
            job.shutdown_until(ShutdownDeadline::after(Duration::from_millis(1))),
            ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 1 }
        );
        assert!(job.join_handle.is_some());
        assert!(source_cancellation.is_cancelled());

        release.store(true, Ordering::Release);
        assert_eq!(
            job.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::Completed
        );
        assert_eq!(
            job.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::AlreadyCompleted
        );
    }

    #[test]
    fn shutdown_reports_worker_panic() {
        let mut job = test_job(std::thread::spawn(|| panic!("expected local job panic")));

        assert_eq!(
            job.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::ThreadPanicked {
                panicked_threads: 1,
                pending_threads: 0,
            }
        );
    }

    #[test]
    fn suspend_transfer_preserves_join_authority() {
        let mut renderer_owner = Some(test_job(std::thread::spawn(|| {})));
        let mut process_owner = renderer_owner.take().expect("suspend должен передать job");

        assert!(renderer_owner.is_none());
        assert!(process_owner.join_handle.is_some());
        assert_eq!(
            process_owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::Completed
        );
    }

    #[test]
    fn fail_safe_drop_waits_instead_of_reporting_detached_success() {
        let release = Arc::new(AtomicBool::new(false));
        let worker_release = Arc::clone(&release);
        let job = test_job(std::thread::spawn(move || {
            while !worker_release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
        }));
        let (drop_finished_sender, drop_finished_receiver) = mpsc::channel();
        let dropper = std::thread::spawn(move || {
            drop(job);
            drop_finished_sender
                .send(())
                .expect("test receiver должен оставаться жив");
        });

        assert!(
            drop_finished_receiver
                .recv_timeout(Duration::from_millis(5))
                .is_err(),
            "Drop не должен detach-ить ещё работающий thread"
        );
        release.store(true, Ordering::Release);
        drop_finished_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("Drop должен завершиться после worker-а");
        dropper.join().expect("dropper не должен panic");
    }

    mod shutdown_observation;
}
