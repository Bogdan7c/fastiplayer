//! Process-lifetime owner экспорта immutable playlist snapshot-а.
//!
//! UI выбирает scope и format до native save dialog. Worker сначала выполняет
//! pure S10 preflight/serialization и только затем передаёт готовые bytes
//! neutral S04 atomic writer-у. Queue, selection и dirty revisions не мутируются.

mod cue_availability;

use std::fmt;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};

use playlist_core::{PlaylistEntryId, SecretUrlLocator, ServiceDurableReopenPayload};
use playlist_io::{
    PlaylistExportDocumentTarget, PlaylistExportFormat, PlaylistExportLocatorPolicy,
    PlaylistExportLocatorRejection, PlaylistExportScope, PlaylistExportSecretClassification,
    PlaylistExportSnapshot, PlaylistExportWarning, PortablePlaylistExportUrl,
    PortableUrlSecretClassification, preflight_playlist_export,
};
use pollster::FutureExt as _;
use winit::window::Window;

use crate::app_wake::{AppWakePort, CompletionPublishError, OwnerMailboxReceiver, owner_mailbox};
use crate::media_open::SafeMediaLabel;
use crate::process_shutdown::{
    FinishedThreadJoin, ProcessOwnerShutdownOutcome, ShutdownDeadline, join_finished_thread,
    join_thread_until,
};
use crate::url_service_adapter::{StartupUrlClassification, classify_playlist_url};

use super::PlaylistRuntime;

pub(super) use cue_availability::CueExportAvailabilityCache;

/// Пользовательский scope фиксируется до открытия save dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaylistExportScopeIntent {
    /// Экспортировать все canonical top-level entries.
    FullPlaylist,
    /// Экспортировать top-level entries, затронутые текущим selection.
    SelectedEntries,
}

/// Полный typed intent toolbar menu без позиционных bool-параметров.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlaylistExportRequest {
    /// Canonical queue scope.
    pub(crate) scope: PlaylistExportScopeIntent,
    /// Явно выбранный document format.
    pub(crate) format: PlaylistExportFormat,
}

/// Save dialog является explicit user intent разрешить replacement выбранного target-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlaylistExportOverwriteIntent {
    /// Native save dialog уже выполнил platform overwrite acknowledgement.
    ReplaceTargetSelectedBySaveDialog,
}

/// Exact prepared payload после pure preflight; Debug не раскрывает path или bytes.
pub(super) struct PlaylistExportConfirmationContinuation {
    generation: u64,
    target_path: PathBuf,
    document_bytes: Vec<u8>,
    overwrite_intent: PlaylistExportOverwriteIntent,
    flattened_compound_groups: bool,
}

impl fmt::Debug for PlaylistExportConfirmationContinuation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistExportConfirmationContinuation")
            .field("generation", &self.generation)
            .field("target_path", &"<redacted>")
            .field("document_bytes", &"<redacted>")
            .field("overwrite_intent", &self.overwrite_intent)
            .field("flattened_compound_groups", &self.flattened_compound_groups)
            .finish()
    }
}

/// Safe terminal failure vocabulary не переносит raw locator/path в UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlaylistExportJobError {
    InvalidTarget,
    LocatorIneligible,
    AtomicWriteFailed,
    WorkerPanicked,
}

/// Результат одного dialog/preflight/write либо confirmed writer job-а.
#[derive(Debug)]
enum PlaylistExportJobCompletion {
    Cancelled {
        generation: u64,
    },
    AwaitingSensitiveConfirmation {
        generation: u64,
        locator_count: usize,
        continuation: PlaylistExportConfirmationContinuation,
    },
    Written {
        generation: u64,
        durability: PlaylistExportDurability,
        flattened_compound_groups: bool,
    },
    Failed {
        generation: u64,
        error: PlaylistExportJobError,
    },
}

impl PlaylistExportJobCompletion {
    const fn generation(&self) -> u64 {
        match self {
            Self::Cancelled { generation }
            | Self::AwaitingSensitiveConfirmation { generation, .. }
            | Self::Written { generation, .. }
            | Self::Failed { generation, .. } => *generation,
        }
    }
}

/// App-facing durability distinction сохраняет post-rename uncertainty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlaylistExportDurability {
    Durable,
    ReplacedDurabilityUnconfirmed,
}

/// Один owned worker с exactly-once terminal mailbox.
struct PlaylistExportJob {
    generation: u64,
    mailbox: OwnerMailboxReceiver<(), PlaylistExportJobCompletion>,
    join_handle: Option<JoinHandle<()>>,
    pending_completion: Option<PlaylistExportJobCompletion>,
    cancellation_requested: Arc<AtomicBool>,
    terminal_delivered: bool,
}

impl PlaylistExportJob {
    /// Создаёт save dialog после того, как scope/format уже зафиксированы.
    fn spawn_dialog(
        window: &Window,
        wake_port: AppWakePort,
        generation: u64,
        request: PlaylistExportRequest,
        snapshot: PlaylistExportSnapshot,
    ) -> Result<Self, String> {
        let (filter_label, extension, title) = export_dialog_presentation(request);
        let suggested_file_name = format!("playlist.{extension}");
        let dialog = rfd::AsyncFileDialog::new()
            .set_parent(window)
            .set_title(title)
            .set_file_name(suggested_file_name)
            .add_filter(filter_label, &[extension])
            .save_file();
        Self::spawn_runner(
            wake_port,
            generation,
            "playlist-export-picker",
            move |cancelled| {
                let Some(handle) = dialog.block_on() else {
                    return PlaylistExportJobCompletion::Cancelled { generation };
                };
                if cancelled.load(Ordering::Acquire) {
                    return PlaylistExportJobCompletion::Cancelled { generation };
                }
                prepare_and_write_export(
                    generation,
                    request.format,
                    snapshot,
                    handle.path().to_path_buf(),
                    PlaylistExportOverwriteIntent::ReplaceTargetSelectedBySaveDialog,
                    cancelled.as_ref(),
                )
            },
        )
    }

    /// Confirmed sensitive payload пропускает dialog/preflight и выполняет только write.
    fn spawn_confirmed_writer(
        wake_port: AppWakePort,
        continuation: PlaylistExportConfirmationContinuation,
    ) -> Result<Self, String> {
        let generation = continuation.generation;
        Self::spawn_runner(
            wake_port,
            generation,
            "playlist-export-writer",
            move |cancelled| {
                if cancelled.load(Ordering::Acquire) {
                    return PlaylistExportJobCompletion::Cancelled {
                        generation: continuation.generation,
                    };
                }
                write_prepared_export(continuation)
            },
        )
    }

    /// Общий thread/mailbox runner удерживает единый lifecycle для picker и writer.
    fn spawn_runner(
        wake_port: AppWakePort,
        generation: u64,
        thread_name: &str,
        runner: impl FnOnce(Arc<AtomicBool>) -> PlaylistExportJobCompletion + Send + 'static,
    ) -> Result<Self, String> {
        let (publisher, mailbox) = owner_mailbox(wake_port);
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation_requested);
        let join_handle = thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                let completion = runner(worker_cancellation);
                if let Err(error) = publisher.publish_completion(completion) {
                    let CompletionPublishError::AlreadyPublished = error;
                    tracing::error!("Playlist export job опубликовал второй terminal result");
                }
            })
            .map_err(|error| format!("Не удалось запустить экспорт плейлиста: {error}"))?;
        Ok(Self {
            generation,
            mailbox,
            join_handle: Some(join_handle),
            pending_completion: None,
            cancellation_requested,
            terminal_delivered: false,
        })
    }

    /// Неблокирующе забирает terminal и отличает worker panic.
    fn drain(&mut self) -> Option<PlaylistExportJobCompletion> {
        if self.terminal_delivered {
            return None;
        }
        let mailbox = self.mailbox.drain();
        if let Some(completion) = mailbox.completion {
            let generation = completion.generation();
            self.pending_completion = Some(completion);
            let join_outcome = self.join_handle.take().map(JoinHandle::join).transpose();
            let completion = match join_outcome {
                Ok(_) => self.pending_completion.take(),
                Err(_) => Some(PlaylistExportJobCompletion::Failed {
                    generation,
                    error: PlaylistExportJobError::WorkerPanicked,
                }),
            };
            self.terminal_delivered = completion.is_some();
            return completion;
        }
        let completion = match join_finished_thread(&mut self.join_handle) {
            FinishedThreadJoin::Joined | FinishedThreadJoin::AlreadyJoined => {
                self.pending_completion.take()
            }
            FinishedThreadJoin::Panicked => Some(PlaylistExportJobCompletion::Failed {
                generation: self.generation,
                error: PlaylistExportJobError::WorkerPanicked,
            }),
            FinishedThreadJoin::StillRunning => None,
        };
        self.terminal_delivered = completion.is_some();
        completion
    }

    /// Shutdown отменяет дальнейшие filesystem stages и bounded-join-ит worker.
    pub(super) fn shutdown_until(
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

/// Process-lifetime owner допускает максимум один dialog/preflight/write job.
pub(super) struct PlaylistExportIoOwner {
    wake_port: AppWakePort,
    generation: u64,
    job: Option<PlaylistExportJob>,
}

impl PlaylistExportIoOwner {
    pub(super) fn new(wake_port: AppWakePort) -> Self {
        Self {
            wake_port,
            generation: 0,
            job: None,
        }
    }

    pub(super) const fn is_open(&self) -> bool {
        self.job.is_some()
    }

    /// Новый job получает generation только вместе с успешным thread spawn.
    fn start(
        &mut self,
        window: &Window,
        request: PlaylistExportRequest,
        snapshot: PlaylistExportSnapshot,
    ) -> Result<bool, String> {
        if self.job.is_some() {
            return Ok(false);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| "Исчерпан generation экспорта плейлиста".to_owned())?;
        let job = PlaylistExportJob::spawn_dialog(
            window,
            self.wake_port.clone(),
            generation,
            request,
            snapshot,
        )?;
        self.generation = generation;
        self.job = Some(job);
        Ok(true)
    }

    /// Matching confirmation может запустить только writer той же generation.
    fn start_confirmed(
        &mut self,
        continuation: PlaylistExportConfirmationContinuation,
    ) -> Result<bool, String> {
        if self.job.is_some() || continuation.generation != self.generation {
            return Ok(false);
        }
        self.job = Some(PlaylistExportJob::spawn_confirmed_writer(
            self.wake_port.clone(),
            continuation,
        )?);
        Ok(true)
    }

    /// Cancel marker revalidate-ится на serialized drain boundary.
    pub(super) fn cancel_active(&mut self) {
        if let Some(job) = self.job.as_mut() {
            job.cancellation_requested.store(true, Ordering::Release);
        }
    }

    fn drain(&mut self) -> Option<PlaylistExportJobCompletion> {
        let job = self.job.as_mut()?;
        let completion = job.drain()?;
        let completion = if job.cancellation_requested.load(Ordering::Acquire) {
            PlaylistExportJobCompletion::Cancelled {
                generation: completion.generation(),
            }
        } else {
            completion
        };
        self.job = None;
        (completion.generation() == self.generation).then_some(completion)
    }

    pub(super) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> ProcessOwnerShutdownOutcome {
        let Some(job) = self.job.as_mut() else {
            return ProcessOwnerShutdownOutcome::Completed;
        };
        let outcome = job.shutdown_until(deadline);
        if !matches!(outcome, ProcessOwnerShutdownOutcome::TimedOut { .. }) {
            self.job = None;
        }
        outcome
    }
}

/// App composition policy повторно использует тот же URL service registry.
struct AppPlaylistExportLocatorPolicy;

impl PlaylistExportLocatorPolicy for AppPlaylistExportLocatorPolicy {
    fn preflight_url(
        &self,
        locator: &SecretUrlLocator,
    ) -> Result<PortablePlaylistExportUrl, PlaylistExportLocatorRejection> {
        let classification = classify_playlist_url(locator);
        let StartupUrlClassification::Supported(service_locator) = classification else {
            return Err(PlaylistExportLocatorRejection::OwnerPolicyRejected);
        };
        let secret_classification = if service_locator.requires_sensitive_export_acknowledgement() {
            PortableUrlSecretClassification::SensitiveDurableIdentity
        } else {
            PortableUrlSecretClassification::Public
        };
        PortablePlaylistExportUrl::new(
            locator.expose_secret_for_persistence(),
            secret_classification,
        )
        .map_err(|_| PlaylistExportLocatorRejection::NonPortableIdentity)
    }

    fn preflight_service(
        &self,
        _locator: &ServiceDurableReopenPayload,
    ) -> Result<PortablePlaylistExportUrl, PlaylistExportLocatorRejection> {
        // Stable extracted-child payload mapping появится у его service owner-а в S19.
        Err(PlaylistExportLocatorRejection::ServiceOwnerUnavailable)
    }
}

/// Выполняет все fallible semantic checks до первого filesystem mutation.
fn prepare_and_write_export(
    generation: u64,
    format: PlaylistExportFormat,
    snapshot: PlaylistExportSnapshot,
    target_path: PathBuf,
    overwrite_intent: PlaylistExportOverwriteIntent,
    cancellation_requested: &AtomicBool,
) -> PlaylistExportJobCompletion {
    let target = match PlaylistExportDocumentTarget::local_file(target_path.clone()) {
        Ok(target) => target,
        Err(_) => {
            return PlaylistExportJobCompletion::Failed {
                generation,
                error: PlaylistExportJobError::InvalidTarget,
            };
        }
    };
    let prepared = match preflight_playlist_export(
        &snapshot,
        format,
        &target,
        &AppPlaylistExportLocatorPolicy,
    ) {
        Ok(prepared) => prepared,
        Err(_) => {
            return PlaylistExportJobCompletion::Failed {
                generation,
                error: PlaylistExportJobError::LocatorIneligible,
            };
        }
    };
    let flattened_compound_groups = prepared.warnings().iter().any(|warning| {
        matches!(
            warning,
            PlaylistExportWarning::CompoundGroupingFlattened { .. }
        )
    });
    let secret_classification = prepared.secret_classification();
    let serialized = prepared.serialize();
    let document_bytes = serialized.into_bytes();
    let continuation = PlaylistExportConfirmationContinuation {
        generation,
        target_path,
        document_bytes,
        overwrite_intent,
        flattened_compound_groups,
    };
    match secret_classification {
        PlaylistExportSecretClassification::NoSensitiveLocators => {
            if cancellation_requested.load(Ordering::Acquire) {
                PlaylistExportJobCompletion::Cancelled { generation }
            } else {
                write_prepared_export(continuation)
            }
        }
        PlaylistExportSecretClassification::SensitiveDurableLocators { locator_count } => {
            PlaylistExportJobCompletion::AwaitingSensitiveConfirmation {
                generation,
                locator_count,
                continuation,
            }
        }
    }
}

/// Единственная функция, которой разрешено вызвать S04 target mutation.
fn write_prepared_export(
    continuation: PlaylistExportConfirmationContinuation,
) -> PlaylistExportJobCompletion {
    write_prepared_export_with(continuation, atomic_file_store::replace_file_atomically)
}

/// Инъекция atomic boundary позволяет focused tests проверить все stage outcomes.
fn write_prepared_export_with(
    continuation: PlaylistExportConfirmationContinuation,
    writer: impl FnOnce(&std::path::Path, &[u8]) -> atomic_file_store::AtomicFileWriteOutcome,
) -> PlaylistExportJobCompletion {
    let PlaylistExportConfirmationContinuation {
        generation,
        target_path,
        document_bytes,
        overwrite_intent: PlaylistExportOverwriteIntent::ReplaceTargetSelectedBySaveDialog,
        flattened_compound_groups,
    } = continuation;
    match writer(&target_path, &document_bytes) {
        atomic_file_store::AtomicFileWriteOutcome::Durable => {
            PlaylistExportJobCompletion::Written {
                generation,
                durability: PlaylistExportDurability::Durable,
                flattened_compound_groups,
            }
        }
        atomic_file_store::AtomicFileWriteOutcome::ReplacedDurabilityUnconfirmed(_) => {
            PlaylistExportJobCompletion::Written {
                generation,
                durability: PlaylistExportDurability::ReplacedDurabilityUnconfirmed,
                flattened_compound_groups,
            }
        }
        atomic_file_store::AtomicFileWriteOutcome::NotReplaced(_) => {
            PlaylistExportJobCompletion::Failed {
                generation,
                error: PlaylistExportJobError::AtomicWriteFailed,
            }
        }
    }
}

/// Presentation не определяет format: format уже хранится в typed request.
fn export_dialog_presentation(
    request: PlaylistExportRequest,
) -> (&'static str, &'static str, &'static str) {
    match request.format {
        PlaylistExportFormat::M3u8 => ("Плейлист M3U8", "m3u8", "Экспортировать плейлист в M3U8"),
        PlaylistExportFormat::Xspf => ("Плейлист XSPF", "xspf", "Экспортировать плейлист в XSPF"),
        PlaylistExportFormat::Cue => ("CUE sheet", "cue", "Экспортировать треки в CUE"),
    }
}

impl PlaylistRuntime {
    /// Снимает immutable canonical scope и только затем открывает save dialog.
    pub(crate) fn start_playlist_export(
        &mut self,
        window: &Window,
        request: PlaylistExportRequest,
    ) -> bool {
        if !self.admission_open.load(Ordering::Acquire) || self.export_io.is_open() {
            return false;
        }
        let Some(controller) = self.controller.as_ref() else {
            return false;
        };
        let queue = controller.queue();
        if queue.is_empty() {
            return false;
        }
        let selected_entry_ids = if request.scope == PlaylistExportScopeIntent::SelectedEntries {
            let mut entry_ids = selected_export_entry_ids(controller);
            if entry_ids.is_empty() {
                return false;
            }
            Some(std::mem::take(&mut entry_ids))
        } else {
            None
        };
        let scope = selected_entry_ids
            .as_deref()
            .map_or(PlaylistExportScope::Full, PlaylistExportScope::Selected);
        let snapshot = match PlaylistExportSnapshot::capture(queue, scope) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(?error, "Не удалось снять immutable export snapshot");
                self.set_playlist_safe_feedback("Не удалось подготовить выбранный scope экспорта");
                return true;
            }
        };
        self.replacement_confirmation.cancel();
        match self.export_io.start(window, request, snapshot) {
            Ok(started) => started,
            Err(error) => {
                tracing::warn!(%error, "Не удалось запустить export job");
                self.set_playlist_safe_feedback("Не удалось открыть экспорт плейлиста");
                true
            }
        }
    }

    /// Matching generalized confirmation запускает только prepared atomic writer.
    pub(super) fn confirm_playlist_export(
        &mut self,
        continuation: PlaylistExportConfirmationContinuation,
    ) -> bool {
        match self.export_io.start_confirmed(continuation) {
            Ok(started) => started,
            Err(error) => {
                tracing::warn!(%error, "Не удалось запустить confirmed export writer");
                self.set_playlist_safe_feedback("Не удалось продолжить экспорт плейлиста");
                true
            }
        }
    }

    /// Serialized owner drain stage-ит confirmation либо safe terminal feedback.
    pub(in crate::playlist_runtime) fn drain_playlist_export_job(&mut self) -> bool {
        let Some(completion) = self.export_io.drain() else {
            return false;
        };
        match completion {
            PlaylistExportJobCompletion::Cancelled { .. } => false,
            PlaylistExportJobCompletion::AwaitingSensitiveConfirmation {
                locator_count,
                continuation,
                ..
            } => {
                let safe_label = SafeMediaLabel::from_service_safe_label("экспорт плейлиста");
                if let Err(error) = self.replacement_confirmation.replace_with_export(
                    safe_label,
                    locator_count,
                    continuation,
                ) {
                    tracing::warn!(?error, "Не удалось открыть confirmation экспорта");
                    self.set_playlist_safe_feedback("Не удалось подтвердить безопасный экспорт");
                }
                true
            }
            PlaylistExportJobCompletion::Written {
                durability,
                flattened_compound_groups,
                ..
            } => {
                if durability == PlaylistExportDurability::ReplacedDurabilityUnconfirmed {
                    self.set_playlist_safe_feedback(
                        "Файл заменён, но файловая система не подтвердила долговечность записи",
                    );
                } else if flattened_compound_groups {
                    self.set_playlist_safe_feedback(
                        "M3U8 сохранён: составные группы представлены отдельными строками",
                    );
                }
                true
            }
            PlaylistExportJobCompletion::Failed { error, .. } => {
                let safe_message = match error {
                    PlaylistExportJobError::InvalidTarget => {
                        "Выбранный путь нельзя использовать для экспорта"
                    }
                    PlaylistExportJobError::LocatorIneligible => {
                        "Плейлист содержит источник, который нельзя безопасно экспортировать"
                    }
                    PlaylistExportJobError::AtomicWriteFailed => {
                        "Не удалось атомарно сохранить файл плейлиста"
                    }
                    PlaylistExportJobError::WorkerPanicked => {
                        "Фоновый экспорт плейлиста аварийно завершился"
                    }
                };
                self.set_playlist_safe_feedback(safe_message);
                true
            }
        }
    }
}

/// Возвращает canonical top-level export scope без part-to-group inference.
pub(super) fn selected_export_entry_ids(
    controller: &super::controller::PlaylistController,
) -> Vec<PlaylistEntryId> {
    let queue = controller.queue();
    let selection = controller.view_snapshot();
    queue
        .iter_top_level_entry_ids()
        .filter(|entry_id| selection.selection().is_selected(*entry_id))
        .collect()
}

#[cfg(test)]
mod tests;
