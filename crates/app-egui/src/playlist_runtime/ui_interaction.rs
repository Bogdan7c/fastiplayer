//! Process-lifetime состояние форм Playlist и async multi-file picker.
//!
//! Модуль хранит только UI draft/lifecycle и безопасные bounded сообщения. Политика
//! URL, queue mutations, probing и sorting остаются у существующих owners.

use std::fmt;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};

use pollster::FutureExt as _;
use winit::window::Window;

use playlist_core::PlaylistItemId;

use crate::app_wake::{AppWakePort, CompletionPublishError, OwnerMailboxReceiver, owner_mailbox};
use crate::local_media;
use crate::process_shutdown::{
    FinishedThreadJoin, ProcessOwnerShutdownOutcome, ShutdownDeadline, join_finished_thread,
    join_thread_until,
};

use super::PlaylistRuntime;
use super::discovery::{ManualAddCompletion, ManualAddTerminalOutcome, PlaylistDiscoveryStatus};
use super::view::PlaylistStructuralActionAvailability;

/// Безопасная ошибка формы: введённый locator сюда никогда не копируется.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PlaylistUrlDraftError(Arc<str>);

impl PlaylistUrlDraftError {
    pub(crate) fn new(message: impl Into<Arc<str>>) -> Self {
        Self(message.into())
    }

    pub(crate) fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PlaylistUrlDraftError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PlaylistUrlDraftError")
            .field(&self.0)
            .finish()
    }
}

/// D48 form state принадлежит process runtime и намеренно не сериализуется.
#[derive(Default)]
pub(crate) struct PlaylistUrlDraftState {
    text: String,
    open: bool,
    request_focus: bool,
    safe_error: Option<PlaylistUrlDraftError>,
}

impl fmt::Debug for PlaylistUrlDraftState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistUrlDraftState")
            .field("text", &"<redacted>")
            .field("open", &self.open)
            .field("request_focus", &self.request_focus)
            .field("safe_error", &self.safe_error)
            .finish()
    }
}

impl PlaylistUrlDraftState {
    pub(crate) fn open(&mut self) {
        self.open = true;
        self.request_focus = true;
    }

    pub(crate) fn replace_text(&mut self, text: String) {
        self.text = text;
        self.safe_error = None;
    }

    pub(crate) fn cancel(&mut self) -> bool {
        if !self.open {
            return false;
        }
        self.text.clear();
        self.safe_error = None;
        self.open = false;
        self.request_focus = false;
        true
    }

    pub(crate) fn finish_success(&mut self) {
        self.text.clear();
        self.safe_error = None;
        self.open = false;
        self.request_focus = false;
    }

    pub(crate) fn set_safe_error(&mut self, error: PlaylistUrlDraftError) {
        self.safe_error = Some(error);
        self.open = true;
        self.request_focus = true;
    }

    pub(crate) fn acknowledge_focus_request(&mut self) {
        self.request_focus = false;
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) const fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) const fn requests_focus(&self) -> bool {
        self.request_focus
    }

    pub(crate) fn safe_error(&self) -> Option<&PlaylistUrlDraftError> {
        self.safe_error.as_ref()
    }
}

/// Lossless result picker-а; paths остаются только в process memory.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PlaylistFileDialogCompletion {
    Cancelled,
    Selected(Vec<PathBuf>),
    Failed,
}

/// Один async multi-file dialog. UI thread только создаёт future и drain-ит mailbox.
pub(crate) struct PlaylistFileDialogJob {
    mailbox: OwnerMailboxReceiver<(), PlaylistFileDialogCompletion>,
    join_handle: Option<JoinHandle<()>>,
    pending_completion: Option<PlaylistFileDialogCompletion>,
    cancellation_requested: Arc<AtomicBool>,
    terminal_delivered: bool,
}

impl PlaylistFileDialogJob {
    pub(crate) fn spawn(window: &Window, wake_port: AppWakePort) -> Result<Self, String> {
        let dialog = rfd::AsyncFileDialog::new()
            .set_parent(window)
            .set_title("Добавить файлы в плейлист")
            .add_filter(
                "Supported Media",
                local_media::SUPPORTED_LOCAL_MEDIA_EXTENSIONS,
            )
            .add_filter("WebM / Matroska", &["webm", "mkv"])
            .add_filter("All Files", &["*"])
            .pick_files();
        Self::spawn_runner(
            wake_port,
            "playlist-file-picker",
            move |worker_cancel| match dialog.block_on() {
                Some(handles) if !worker_cancel.load(Ordering::Acquire) => {
                    PlaylistFileDialogCompletion::Selected(
                        handles
                            .into_iter()
                            .map(|handle| handle.path().to_path_buf())
                            .collect(),
                    )
                }
                Some(_) | None => PlaylistFileDialogCompletion::Cancelled,
            },
        )
    }

    /// Общий runner сохраняет production mailbox/thread semantics и даёт hermetic test fixture.
    fn spawn_runner(
        wake_port: AppWakePort,
        thread_name: &str,
        runner: impl FnOnce(Arc<AtomicBool>) -> PlaylistFileDialogCompletion + Send + 'static,
    ) -> Result<Self, String> {
        let (publisher, mailbox) = owner_mailbox(wake_port);
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancellation_requested);
        let join_handle = thread::Builder::new()
            .name(thread_name.to_string())
            .spawn(move || {
                let completion = runner(worker_cancel);
                if let Err(error) = publisher.publish_completion(completion) {
                    let CompletionPublishError::AlreadyPublished = error;
                    tracing::error!(
                        "Multi-file dialog попытался опубликовать второй terminal result"
                    );
                }
            })
            .map_err(|error| format!("Не удалось запустить multi-file dialog: {error}"))?;
        Ok(Self {
            mailbox,
            join_handle: Some(join_handle),
            pending_completion: None,
            cancellation_requested,
            terminal_delivered: false,
        })
    }

    #[cfg(test)]
    fn spawn_test(
        wake_port: AppWakePort,
        completion: PlaylistFileDialogCompletion,
    ) -> Result<Self, String> {
        Self::spawn_runner(wake_port, "playlist-file-picker-test", move |_| completion)
    }

    pub(crate) fn drain(&mut self) -> Option<PlaylistFileDialogCompletion> {
        if self.terminal_delivered {
            return None;
        }
        let mailbox = self.mailbox.drain();
        if let Some(completion) = mailbox.completion {
            self.pending_completion = Some(completion);
            // Completion публикуется последней операцией worker-а. Join здесь не
            // требует периодического redraw/poll и не может ждать dialog I/O.
            let join_outcome = self.join_handle.take().map(JoinHandle::join).transpose();
            let completion = match join_outcome {
                Ok(_) => self.pending_completion.take(),
                Err(_) => {
                    self.pending_completion = None;
                    Some(PlaylistFileDialogCompletion::Failed)
                }
            };
            self.terminal_delivered = completion.is_some();
            return completion;
        }
        let completion = match join_finished_thread(&mut self.join_handle) {
            FinishedThreadJoin::Joined | FinishedThreadJoin::AlreadyJoined => self
                .pending_completion
                .take()
                .or(Some(PlaylistFileDialogCompletion::Failed)),
            FinishedThreadJoin::Panicked => {
                self.pending_completion = None;
                Some(PlaylistFileDialogCompletion::Failed)
            }
            FinishedThreadJoin::StillRunning => None,
        };
        self.terminal_delivered = completion.is_some();
        completion
    }

    pub(crate) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> ProcessOwnerShutdownOutcome {
        self.cancellation_requested.store(true, Ordering::Release);
        match join_thread_until(&mut self.join_handle, deadline) {
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
}

impl Drop for PlaylistFileDialogJob {
    fn drop(&mut self) {
        self.cancellation_requested.store(true, Ordering::Release);
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

/// Process owner формы и picker-а. Safe feedback не содержит paths/URL.
pub(crate) struct PlaylistUiInteractionOwner {
    wake_port: AppWakePort,
    url_draft: PlaylistUrlDraftState,
    file_dialog: Option<PlaylistFileDialogJob>,
    safe_feedback: Option<PlaylistSafeFeedback>,
    next_safe_feedback_generation: u64,
}

impl PlaylistUiInteractionOwner {
    pub(crate) fn new(wake_port: AppWakePort) -> Self {
        Self {
            wake_port,
            url_draft: PlaylistUrlDraftState::default(),
            file_dialog: None,
            safe_feedback: None,
            next_safe_feedback_generation: 1,
        }
    }

    pub(crate) fn url_draft(&self) -> &PlaylistUrlDraftState {
        &self.url_draft
    }

    pub(crate) fn url_draft_mut(&mut self) -> &mut PlaylistUrlDraftState {
        &mut self.url_draft
    }

    pub(crate) fn safe_feedback(&self) -> Option<&PlaylistSafeFeedback> {
        self.safe_feedback.as_ref()
    }

    pub(crate) fn set_safe_feedback(&mut self, message: impl Into<Arc<str>>) {
        // Поколение является identity события; текст остаётся только presentation payload.
        let generation = PlaylistSafeFeedbackGeneration(self.next_safe_feedback_generation);
        // Ноль зарезервирован, поэтому после теоретического wrap начинаем новый цикл с единицы.
        self.next_safe_feedback_generation = self.next_safe_feedback_generation.wrapping_add(1);
        if self.next_safe_feedback_generation == 0 {
            self.next_safe_feedback_generation = 1;
        }
        self.safe_feedback = Some(PlaylistSafeFeedback {
            generation,
            message: message.into(),
        });
    }

    pub(crate) fn start_file_dialog(&mut self, window: &Window) -> Result<bool, String> {
        if self.file_dialog.is_some() {
            return Ok(false);
        }
        self.file_dialog = Some(PlaylistFileDialogJob::spawn(
            window,
            self.wake_port.clone(),
        )?);
        self.safe_feedback = None;
        Ok(true)
    }

    #[cfg(test)]
    fn install_file_dialog_for_test(&mut self, job: PlaylistFileDialogJob) -> bool {
        if self.file_dialog.is_some() {
            return false;
        }
        self.file_dialog = Some(job);
        true
    }

    pub(crate) fn dialog_is_open(&self) -> bool {
        self.file_dialog.is_some()
    }

    pub(crate) fn drain_file_dialog(&mut self) -> Option<PlaylistFileDialogCompletion> {
        let completion = self.file_dialog.as_mut()?.drain()?;
        self.file_dialog = None;
        Some(completion)
    }

    pub(crate) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> ProcessOwnerShutdownOutcome {
        let Some(job) = self.file_dialog.as_mut() else {
            return ProcessOwnerShutdownOutcome::Completed;
        };
        let outcome = job.shutdown_until(deadline);
        if !matches!(outcome, ProcessOwnerShutdownOutcome::TimedOut { .. }) {
            self.file_dialog = None;
        }
        outcome
    }
}

/// Одноразовый explicit scroll/focus target D80.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistGoCurrentTarget {
    Row(PlaylistItemId),
    Tombstone,
}

/// Toolbar нужен только typed факт конфликтующей foreground-операции.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlaylistActiveOperation {
    MetadataSort,
    ManualAdd,
    SiblingDiscovery,
}

/// Opaque identity terminal Manual Add не раскрывает executor handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistManualAddEventId(pub(crate) u64);

/// Privacy-safe accounting частичного Manual Add форматируется только в presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaylistManualAddWarning {
    pub(crate) event_id: PlaylistManualAddEventId,
    pub(crate) kind: PlaylistManualAddWarningKind,
    pub(crate) requested: usize,
    pub(crate) added: usize,
    pub(crate) unsupported_container: usize,
    pub(crate) no_audio_video_tracks: usize,
    pub(crate) probe_failed: usize,
    pub(crate) capacity_rejected: usize,
}

/// Presentation различает partial accounting и owner-level terminal failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistManualAddWarningKind {
    PartialResult,
    Failed,
}

impl PlaylistManualAddWarning {
    /// Успешный полный batch не является проблемным уведомлением.
    fn from_completion(completion: &ManualAddCompletion) -> Option<Self> {
        let has_rejections = completion.added < completion.requested
            || completion.unsupported_container > 0
            || completion.no_audio_video_tracks > 0
            || completion.probe_failed > 0
            || completion.capacity_rejected > 0;
        let kind = match completion.outcome {
            ManualAddTerminalOutcome::Cancelled
            | ManualAddTerminalOutcome::SupersededQueueGeneration => return None,
            ManualAddTerminalOutcome::Appended
            | ManualAddTerminalOutcome::NoSuccessfulItems
            | ManualAddTerminalOutcome::NoCapacity => {
                if !has_rejections {
                    return None;
                }
                PlaylistManualAddWarningKind::PartialResult
            }
            ManualAddTerminalOutcome::ExecutorDisconnected
            | ManualAddTerminalOutcome::CommitRejected => PlaylistManualAddWarningKind::Failed,
        };
        Some(Self {
            event_id: PlaylistManualAddEventId(completion.job_id.get()),
            kind,
            requested: completion.requested,
            added: completion.added,
            unsupported_container: completion.unsupported_container,
            no_audio_video_tracks: completion.no_audio_video_tracks,
            probe_failed: completion.probe_failed,
            capacity_rejected: completion.capacity_rejected,
        })
    }
}

/// Монотонное поколение отделяет повтор одинакового safe текста от старого snapshot-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistSafeFeedbackGeneration(pub(crate) u64);

/// Safe feedback хранит текст как payload, но никогда не использует его как identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaylistSafeFeedback {
    pub(crate) generation: PlaylistSafeFeedbackGeneration,
    pub(crate) message: Arc<str>,
}

/// Immutable interaction snapshot одного frame-а. Raw URL не реализует `Debug`.
#[derive(Clone)]
pub(crate) struct PlaylistInteractionModel {
    pub(crate) structural_action_availability: PlaylistStructuralActionAvailability,
    pub(crate) item_count: usize,
    pub(crate) selected_item_count: usize,
    pub(crate) cue_full_export_availability: playlist_io::PlaylistExportAvailability,
    pub(crate) cue_selected_export_availability: playlist_io::PlaylistExportAvailability,
    pub(crate) url_editor_open: bool,
    pub(crate) url_text: String,
    pub(crate) url_request_focus: bool,
    pub(crate) url_safe_error: Option<PlaylistUrlDraftError>,
    pub(crate) file_dialog_open: bool,
    pub(crate) import_dialog_open: bool,
    pub(crate) export_dialog_open: bool,
    pub(crate) active_operation: Option<PlaylistActiveOperation>,
    pub(crate) manual_add_warning: Option<PlaylistManualAddWarning>,
    pub(crate) safe_feedback: Option<PlaylistSafeFeedback>,
    pub(crate) go_current_target: Option<PlaylistGoCurrentTarget>,
}

impl Default for PlaylistInteractionModel {
    fn default() -> Self {
        Self {
            structural_action_availability: PlaylistStructuralActionAvailability::Available,
            item_count: 0,
            selected_item_count: 0,
            cue_full_export_availability: playlist_io::PlaylistExportAvailability::Disabled(
                playlist_io::CueExportScopeIneligibility::EmptyScope,
            ),
            cue_selected_export_availability: playlist_io::PlaylistExportAvailability::Disabled(
                playlist_io::CueExportScopeIneligibility::EmptyScope,
            ),
            url_editor_open: false,
            url_text: String::new(),
            url_request_focus: false,
            url_safe_error: None,
            file_dialog_open: false,
            import_dialog_open: false,
            export_dialog_open: false,
            active_operation: None,
            manual_add_warning: None,
            safe_feedback: None,
            go_current_target: None,
        }
    }
}

impl PlaylistRuntime {
    /// UI получает snapshot, но никогда mutable form/controller ownership.
    pub(crate) fn playlist_interaction_model(&self) -> PlaylistInteractionModel {
        let controller = self.controller.as_ref();
        let jobs = self.playlist_discovery_jobs_read_model();
        let sort = self.metadata_sort_read_model();
        let active_operation = if sort.active_job.is_some() {
            Some(PlaylistActiveOperation::MetadataSort)
        } else if jobs.active_manual_jobs > 0 {
            Some(PlaylistActiveOperation::ManualAdd)
        } else {
            match self.playlist_discovery_status() {
                PlaylistDiscoveryStatus::Enumerating { .. }
                | PlaylistDiscoveryStatus::Probing { .. } => {
                    Some(PlaylistActiveOperation::SiblingDiscovery)
                }
                _ => None,
            }
        };
        let manual_add_warning = jobs
            .latest_manual_completion
            .as_ref()
            .and_then(PlaylistManualAddWarning::from_completion);
        let go_current_target = controller.and_then(|controller| {
            controller
                .active_media()
                .and_then(|active| active.item_id())
                .map(PlaylistGoCurrentTarget::Row)
                .or_else(|| {
                    controller
                        .view_snapshot()
                        .has_active_tombstone()
                        .then_some(PlaylistGoCurrentTarget::Tombstone)
                })
        });
        let draft = self.ui_interaction.url_draft();
        let (cue_full_export_availability, cue_selected_export_availability) =
            self.cue_export_availabilities();
        PlaylistInteractionModel {
            structural_action_availability: controller.map_or(
                PlaylistStructuralActionAvailability::Unavailable,
                |controller| controller.view_snapshot().structural_action_availability(),
            ),
            item_count: controller
                .map_or(0, |controller| controller.queue().top_level_entry_count()),
            selected_item_count: controller.map_or(0, |controller| {
                controller.view_snapshot().selection().selected_count()
            }),
            cue_full_export_availability,
            cue_selected_export_availability,
            url_editor_open: draft.is_open(),
            url_text: draft.text().to_string(),
            url_request_focus: draft.requests_focus(),
            url_safe_error: draft.safe_error().cloned(),
            file_dialog_open: self.ui_interaction.dialog_is_open(),
            import_dialog_open: self.import_io.is_open(),
            export_dialog_open: self.export_io.is_open(),
            active_operation,
            manual_add_warning,
            safe_feedback: self.ui_interaction.safe_feedback().cloned(),
            go_current_target,
        }
    }

    pub(crate) fn start_playlist_file_dialog(&mut self, window: &Window) -> bool {
        match self.ui_interaction.start_file_dialog(window) {
            Ok(changed) => changed,
            Err(_) => {
                self.ui_interaction
                    .set_safe_feedback("Не удалось открыть диалог выбора файлов");
                true
            }
        }
    }

    pub(crate) fn open_playlist_url_editor(&mut self) {
        self.ui_interaction.url_draft_mut().open();
    }

    pub(crate) fn update_playlist_url_draft(&mut self, text: String) {
        self.ui_interaction.url_draft_mut().replace_text(text);
    }

    pub(crate) fn acknowledge_playlist_url_focus(&mut self) {
        self.ui_interaction
            .url_draft_mut()
            .acknowledge_focus_request();
    }

    pub(crate) fn cancel_playlist_url_editor(&mut self) -> bool {
        self.ui_interaction.url_draft_mut().cancel()
    }

    /// Pure URL classifier/controller boundary выполняется после egui render.
    pub(crate) fn submit_playlist_url_draft(
        &mut self,
        yt_dlp_config: &rustiplayer_config::YtDlpConfig,
    ) -> bool {
        let input = self.ui_interaction.url_draft().text().to_string();
        match self.append_playlist_url(&input, yt_dlp_config) {
            Ok(super::UrlAppendActionOutcome::Appended { .. }) => {
                self.ui_interaction.url_draft_mut().finish_success();
            }
            Ok(super::UrlAppendActionOutcome::AwaitingSensitivePersistenceDecision) => {}
            Ok(super::UrlAppendActionOutcome::DeferredUntilStartupInstallResolution) => {
                self.ui_interaction.url_draft_mut().finish_success();
            }
            Ok(super::UrlAppendActionOutcome::ResolvingTopology) => {
                self.ui_interaction.url_draft_mut().finish_success();
            }
            Ok(super::UrlAppendActionOutcome::NoCapacity) => self
                .ui_interaction
                .url_draft_mut()
                .set_safe_error(PlaylistUrlDraftError::new("Плейлист заполнен")),
            Err(error) => {
                let safe_message: Arc<str> = match error {
                    super::UrlAppendValidationError::NotUrl => {
                        "Введите корректный http(s) URL".into()
                    }
                    super::UrlAppendValidationError::Unsupported { safe_error } => {
                        Arc::from(safe_error)
                    }
                    super::UrlAppendValidationError::RuntimeShuttingDown => {
                        "Приложение завершает работу".into()
                    }
                    super::UrlAppendValidationError::LoadDecisionPending => {
                        "Дождитесь загрузки плейлиста".into()
                    }
                    super::UrlAppendValidationError::LocatorMapping
                    | super::UrlAppendValidationError::MetadataMapping
                    | super::UrlAppendValidationError::ConfirmationIdentityExhausted
                    | super::UrlAppendValidationError::TopologyGenerationExhausted
                    | super::UrlAppendValidationError::TopologyWorkerUnavailable
                    | super::UrlAppendValidationError::CommitRejected => {
                        "Не удалось добавить URL".into()
                    }
                };
                self.ui_interaction
                    .url_draft_mut()
                    .set_safe_error(PlaylistUrlDraftError::new(safe_message));
            }
        }
        true
    }

    pub(crate) fn finish_url_draft_after_confirmation(
        &mut self,
        outcome: &super::PlaylistConfirmationApplyOutcome,
    ) {
        match outcome {
            super::PlaylistConfirmationApplyOutcome::UrlAppended { .. }
            | super::PlaylistConfirmationApplyOutcome::DeferredUntilStartupInstallResolution => {
                self.ui_interaction.url_draft_mut().finish_success();
            }
            super::PlaylistConfirmationApplyOutcome::UrlNoCapacity => self
                .ui_interaction
                .url_draft_mut()
                .set_safe_error(PlaylistUrlDraftError::new("Плейлист заполнен")),
            super::PlaylistConfirmationApplyOutcome::CommitRejected => self
                .ui_interaction
                .url_draft_mut()
                .set_safe_error(PlaylistUrlDraftError::new("Не удалось добавить URL")),
            super::PlaylistConfirmationApplyOutcome::Cancelled
            | super::PlaylistConfirmationApplyOutcome::Stale
            | super::PlaylistConfirmationApplyOutcome::Import(_)
            | super::PlaylistConfirmationApplyOutcome::ExportWriterStarted
            | super::PlaylistConfirmationApplyOutcome::QueueReplacementConfirmed(_) => {}
        }
    }

    pub(crate) fn set_playlist_safe_feedback(&mut self, message: impl Into<Arc<str>>) {
        self.ui_interaction.set_safe_feedback(message);
    }

    pub(in crate::playlist_runtime) fn drain_playlist_file_dialog(&mut self) -> bool {
        let Some(completion) = self.ui_interaction.drain_file_dialog() else {
            return false;
        };
        match manual_add_paths_from_dialog_completion(completion) {
            Ok(None) => {}
            Ok(Some(paths)) => {
                if self.start_manual_file_add(paths).is_err() {
                    self.ui_interaction
                        .set_safe_feedback("Не удалось начать добавление файлов");
                }
            }
            Err(()) => self
                .ui_interaction
                .set_safe_feedback("Диалог выбора файлов завершился с ошибкой"),
        }
        true
    }
}

/// Pure terminal mapping: cancel/empty selection не создают queue action.
fn manual_add_paths_from_dialog_completion(
    completion: PlaylistFileDialogCompletion,
) -> Result<Option<Vec<PathBuf>>, ()> {
    match completion {
        PlaylistFileDialogCompletion::Cancelled => Ok(None),
        PlaylistFileDialogCompletion::Selected(paths) if paths.is_empty() => Ok(None),
        PlaylistFileDialogCompletion::Selected(paths) => Ok(Some(paths)),
        PlaylistFileDialogCompletion::Failed => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::Duration;

    use crate::app_wake::{AppWakeEvent, AppWakeOwner, WakeEmitter};

    use super::*;

    struct ChannelWakeEmitter(Sender<AppWakeEvent>);

    impl WakeEmitter for ChannelWakeEmitter {
        fn emit(&self, event: AppWakeEvent) -> Result<(), ()> {
            self.0.send(event).map_err(|_| ())
        }
    }

    fn test_dialog_job(
        completion: PlaylistFileDialogCompletion,
    ) -> (PlaylistFileDialogJob, Receiver<AppWakeEvent>) {
        let (wake_sender, wake_receiver) = mpsc::channel();
        let wake_port = AppWakePort::new(
            AppWakeOwner::PlaylistRuntime,
            Arc::new(ChannelWakeEmitter(wake_sender)),
        );
        let job = PlaylistFileDialogJob::spawn_test(wake_port, completion)
            .expect("hermetic dialog runner должен запуститься");
        (job, wake_receiver)
    }

    fn wait_for_dialog_wake(receiver: &Receiver<AppWakeEvent>) {
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("terminal dialog result должен разбудить owner");
    }

    #[test]
    fn url_draft_debug_never_exposes_sensitive_text() {
        let mut draft = PlaylistUrlDraftState::default();
        draft.open();
        draft.replace_text("https://user:secret@example.test/video.mp4?token=raw".to_string());
        let debug = format!("{draft:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("token"));
    }

    #[test]
    fn url_draft_hide_lifecycle_requires_explicit_cancel_or_success() {
        let mut draft = PlaylistUrlDraftState::default();
        draft.open();
        draft.replace_text("https://example.test/video.mp4".to_string());
        assert_eq!(draft.text(), "https://example.test/video.mp4");
        assert!(draft.is_open());
        assert!(draft.cancel());
        assert!(!draft.cancel());
        assert!(draft.text().is_empty());
    }

    #[test]
    fn url_draft_edit_clears_only_safe_error_and_focus_ack_keeps_text() {
        let mut draft = PlaylistUrlDraftState::default();
        draft.open();
        draft.replace_text("https://example.test/first.mp4?token=secret".to_string());
        draft.set_safe_error(PlaylistUrlDraftError::new("Безопасная ошибка"));
        draft.acknowledge_focus_request();

        assert!(!draft.requests_focus());
        assert!(draft.safe_error().is_some());
        assert!(draft.text().contains("token=secret"));

        draft.replace_text("https://example.test/second.mp4".to_string());
        assert!(draft.safe_error().is_none());
        assert!(draft.is_open());
    }

    #[test]
    fn dialog_cancel_maps_to_no_manual_add_and_is_delivered_once() {
        let (mut job, wake_receiver) = test_dialog_job(PlaylistFileDialogCompletion::Cancelled);
        wait_for_dialog_wake(&wake_receiver);

        let completion = job.drain().expect("Cancel terminal должен быть lossless");
        assert_eq!(
            manual_add_paths_from_dialog_completion(completion),
            Ok(None)
        );
        assert!(job.drain().is_none(), "terminal нельзя доставить повторно");
    }

    #[test]
    fn dialog_selected_paths_keep_exact_one_result_handoff() {
        let expected_paths = vec![PathBuf::from("alpha.mkv"), PathBuf::from("beta.mp3")];
        let (mut job, wake_receiver) = test_dialog_job(PlaylistFileDialogCompletion::Selected(
            expected_paths.clone(),
        ));
        wait_for_dialog_wake(&wake_receiver);

        let completion = job
            .drain()
            .expect("Selected terminal должен быть доставлен");
        assert_eq!(
            manual_add_paths_from_dialog_completion(completion),
            Ok(Some(expected_paths))
        );
        assert!(
            job.drain().is_none(),
            "result handoff должен быть exactly once"
        );
    }

    #[test]
    fn active_dialog_rejects_duplicate_job_without_replacing_first_terminal() {
        let (owner_wake_sender, _owner_wake_receiver) = mpsc::channel();
        let owner_wake_port = AppWakePort::new(
            AppWakeOwner::PlaylistRuntime,
            Arc::new(ChannelWakeEmitter(owner_wake_sender)),
        );
        let mut owner = PlaylistUiInteractionOwner::new(owner_wake_port);
        let (first, first_wake_receiver) = test_dialog_job(PlaylistFileDialogCompletion::Cancelled);
        wait_for_dialog_wake(&first_wake_receiver);
        assert!(owner.install_file_dialog_for_test(first));

        let (duplicate, duplicate_wake_receiver) = test_dialog_job(
            PlaylistFileDialogCompletion::Selected(vec![PathBuf::from("unexpected.mkv")]),
        );
        wait_for_dialog_wake(&duplicate_wake_receiver);
        assert!(!owner.install_file_dialog_for_test(duplicate));

        assert_eq!(
            owner.drain_file_dialog(),
            Some(PlaylistFileDialogCompletion::Cancelled)
        );
        assert!(owner.drain_file_dialog().is_none());
    }
}
