//! Process-lifetime I/O owner локального импорта плейлистов.
//!
//! UI публикует только typed intent. Этот модуль открывает single-file dialog,
//! выполняет bounded `playlist-io` expansion в worker thread и возвращает
//! source-neutral draft владельцу S08 transaction.

use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};

use pollster::FutureExt as _;
use winit::window::Window;

use playlist_core::PlaylistImportEntryDraft;
use playlist_io::{
    CueDocumentSource, CueParseRequest, CueParserLimits, LocalPlaylistExpansionCancellation,
    LocalPlaylistExpansionLimits, LocalPlaylistExpansionRequest, M3uParserLimits, XspfParserLimits,
    expand_local_playlist, parse_cue_document,
};

use crate::app_wake::{AppWakePort, CompletionPublishError, OwnerMailboxReceiver, owner_mailbox};
use crate::process_shutdown::{
    FinishedThreadJoin, ProcessOwnerShutdownOutcome, ShutdownDeadline, join_finished_thread,
    join_thread_until,
};

use super::PlaylistRuntime;
use super::import_transaction::{PlaylistImportDraft, PlaylistImportIntent};

mod materializer;

#[cfg(test)]
mod supersede_cancel_contract;

/// Terminal result одного picker+parse job-а.
#[derive(Debug)]
enum PlaylistImportJobCompletion {
    /// Пользователь закрыл dialog без выбора.
    Cancelled,
    /// Bounded parse/expansion завершён и готов к S08 staging.
    Parsed {
        intent: PlaylistImportIntent,
        draft: PlaylistImportDraft,
    },
    /// Ошибка безопасно классифицирована без path/URL payload.
    Failed(PlaylistImportJobError),
}

/// Safe error vocabulary не переносит raw filesystem details в UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlaylistImportJobError {
    /// Root extension не относится к текущему S09 format set.
    UnsupportedRootFormat,
    /// Root document не удалось прочитать либо проверить.
    SourceRejected,
    /// Worker thread завершился panic-ом.
    WorkerPanicked,
}

/// Один async single-root import job с exactly-once terminal publication.
struct PlaylistImportJob {
    mailbox: OwnerMailboxReceiver<(), PlaylistImportJobCompletion>,
    join_handle: Option<JoinHandle<()>>,
    pending_completion: Option<PlaylistImportJobCompletion>,
    cancellation_requested: Arc<AtomicBool>,
    expansion_cancellation: LocalPlaylistExpansionCancellation,
    terminal_delivered: bool,
}

impl PlaylistImportJob {
    /// Создаёт native dialog, но не блокирует renderer/UI thread.
    fn spawn(
        window: &Window,
        wake_port: AppWakePort,
        intent: PlaylistImportIntent,
    ) -> Result<Self, String> {
        let dialog = rfd::AsyncFileDialog::new()
            .set_parent(window)
            .set_title(import_dialog_title(intent))
            .add_filter(
                "Плейлисты M3U / M3U8 / XSPF / CUE",
                &["m3u", "m3u8", "xspf", "cue"],
            )
            .add_filter("M3U / M3U8", &["m3u", "m3u8"])
            .add_filter("XSPF", &["xspf"])
            .add_filter("CUE", &["cue"])
            .pick_file();
        Self::spawn_runner(
            wake_port,
            "playlist-import-picker",
            move |worker_cancel, expansion_cancellation| {
                let Some(handle) = dialog.block_on() else {
                    return PlaylistImportJobCompletion::Cancelled;
                };
                if worker_cancel.load(Ordering::Acquire) {
                    return PlaylistImportJobCompletion::Cancelled;
                }
                parse_selected_root(handle.path(), intent, &expansion_cancellation)
            },
        )
    }

    /// Запускает тот же authoritative parser для trusted CLI/desktop path без dialog-а.
    fn spawn_path(
        root_path: PathBuf,
        wake_port: AppWakePort,
        intent: PlaylistImportIntent,
    ) -> Result<Self, String> {
        Self::spawn_runner(
            wake_port,
            "playlist-startup-import",
            move |worker_cancel, expansion_cancellation| {
                if worker_cancel.load(Ordering::Acquire) {
                    return PlaylistImportJobCompletion::Cancelled;
                }
                parse_selected_root(&root_path, intent, &expansion_cancellation)
            },
        )
    }

    /// Общий runner сохраняет production mailbox/thread semantics и упрощает tests.
    fn spawn_runner(
        wake_port: AppWakePort,
        thread_name: &str,
        runner: impl FnOnce(
            Arc<AtomicBool>,
            LocalPlaylistExpansionCancellation,
        ) -> PlaylistImportJobCompletion
        + Send
        + 'static,
    ) -> Result<Self, String> {
        let (publisher, mailbox) = owner_mailbox(wake_port);
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancellation_requested);
        let expansion_cancellation = LocalPlaylistExpansionCancellation::new();
        let worker_expansion_cancellation = expansion_cancellation.clone();
        let join_handle = thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                let completion = runner(worker_cancel, worker_expansion_cancellation);
                if let Err(error) = publisher.publish_completion(completion) {
                    let CompletionPublishError::AlreadyPublished = error;
                    tracing::error!(
                        "Playlist import job попытался опубликовать второй terminal result"
                    );
                }
            })
            .map_err(|error| format!("Не удалось запустить импорт плейлиста: {error}"))?;
        Ok(Self {
            mailbox,
            join_handle: Some(join_handle),
            pending_completion: None,
            cancellation_requested,
            expansion_cancellation,
            terminal_delivered: false,
        })
    }

    /// Неблокирующе забирает ровно один terminal result.
    fn drain(&mut self) -> Option<PlaylistImportJobCompletion> {
        if self.terminal_delivered {
            return None;
        }
        let mailbox = self.mailbox.drain();
        if let Some(completion) = mailbox.completion {
            self.pending_completion = Some(completion);
            let join_outcome = self.join_handle.take().map(JoinHandle::join).transpose();
            let completion = match join_outcome {
                Ok(_) => self.pending_completion.take(),
                Err(_) => {
                    self.pending_completion = None;
                    Some(PlaylistImportJobCompletion::Failed(
                        PlaylistImportJobError::WorkerPanicked,
                    ))
                }
            };
            self.terminal_delivered = completion.is_some();
            return completion;
        }
        let completion = match join_finished_thread(&mut self.join_handle) {
            FinishedThreadJoin::Joined | FinishedThreadJoin::AlreadyJoined => self
                .pending_completion
                .take()
                .or(Some(PlaylistImportJobCompletion::Failed(
                    PlaylistImportJobError::WorkerPanicked,
                ))),
            FinishedThreadJoin::Panicked => {
                self.pending_completion = None;
                Some(PlaylistImportJobCompletion::Failed(
                    PlaylistImportJobError::WorkerPanicked,
                ))
            }
            FinishedThreadJoin::StillRunning => None,
        };
        self.terminal_delivered = completion.is_some();
        completion
    }

    /// Cooperative parser cancellation сохраняет общий shutdown deadline.
    fn shutdown_until(&mut self, deadline: ShutdownDeadline) -> ProcessOwnerShutdownOutcome {
        self.cancellation_requested.store(true, Ordering::Release);
        self.expansion_cancellation.cancel();
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

/// Process-lifetime owner не допускает два native import dialog одновременно.
pub(super) struct PlaylistImportIoOwner {
    wake_port: AppWakePort,
    job: Option<PlaylistImportJob>,
}

impl PlaylistImportIoOwner {
    pub(super) fn new(wake_port: AppWakePort) -> Self {
        Self {
            wake_port,
            job: None,
        }
    }

    pub(super) const fn is_open(&self) -> bool {
        self.job.is_some()
    }

    /// Superseding runtime intent инвалидирует и dialog, и parser completion.
    pub(super) fn cancel_active(&mut self) {
        let Some(job) = self.job.as_mut() else {
            return;
        };
        job.cancellation_requested.store(true, Ordering::Release);
        job.expansion_cancellation.cancel();
    }

    pub(super) fn start(
        &mut self,
        window: &Window,
        intent: PlaylistImportIntent,
    ) -> Result<bool, String> {
        if self.job.is_some() {
            return Ok(false);
        }
        self.job = Some(PlaylistImportJob::spawn(
            window,
            self.wake_port.clone(),
            intent,
        )?);
        Ok(true)
    }

    /// Принимает exact startup path, сохраняя single-job и wake/thread semantics owner-а.
    pub(super) fn start_path(
        &mut self,
        root_path: PathBuf,
        intent: PlaylistImportIntent,
    ) -> Result<bool, String> {
        if self.job.is_some() {
            return Ok(false);
        }
        self.job = Some(PlaylistImportJob::spawn_path(
            root_path,
            self.wake_port.clone(),
            intent,
        )?);
        Ok(true)
    }

    fn drain(&mut self) -> Option<PlaylistImportJobCompletion> {
        let job = self.job.as_mut()?;
        let completion = job.drain()?;
        // Supersede может прийти уже после публикации worker result, но до owner drain.
        // Поэтому authoritative cancellation marker проверяется здесь, на serialized boundary.
        let completion = if job.cancellation_requested.load(Ordering::Acquire) {
            PlaylistImportJobCompletion::Cancelled
        } else {
            completion
        };
        self.job = None;
        Some(completion)
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

impl PlaylistRuntime {
    /// Общая supersede boundary не позволяет late parser completion воскресить preview.
    pub(in crate::playlist_runtime) fn supersede_playlist_import_flow(&mut self) {
        self.import_io.cancel_active();
        self.cancel_playlist_url_import();
        self.import_transaction.cancel();
        self.startup_import.supersede();
    }

    /// Запускает explicit append/replace import после post-render action drain.
    pub(crate) fn start_playlist_import_dialog(
        &mut self,
        window: &Window,
        intent: PlaylistImportIntent,
    ) -> bool {
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return false;
        }
        // Повторный Start не должен отменять уже открытый picker из-за дубля UI action.
        if self.import_io.is_open() {
            return false;
        }
        self.supersede_startup_media_apply();
        self.supersede_playlist_import_flow();
        self.replacement_confirmation.cancel();
        match self.import_io.start(window, intent) {
            Ok(started) => started,
            Err(error) => {
                tracing::warn!(%error, "Не удалось открыть dialog импорта плейлиста");
                self.set_playlist_safe_feedback("Не удалось открыть файл плейлиста");
                true
            }
        }
    }

    /// Применяет worker completion только на serialized runtime owner turn.
    pub(in crate::playlist_runtime) fn drain_playlist_import_job(&mut self) -> bool {
        let mut changed = self.stage_held_startup_playlist_draft();
        let Some(completion) = self.import_io.drain() else {
            return changed;
        };
        match completion {
            PlaylistImportJobCompletion::Cancelled => {
                if self.startup_import.is_active() {
                    self.startup_import
                        .finish(super::startup_import::StartupPlaylistImportTerminal::Cancelled);
                    changed = true;
                }
                changed
            }
            PlaylistImportJobCompletion::Parsed { intent, draft } => {
                if intent == PlaylistImportIntent::StartupReplace
                    && self.controller.as_ref().is_none()
                {
                    self.startup_import.hold_draft(draft);
                    return true;
                }
                if let Err(error) = self.stage_playlist_import(intent, draft) {
                    tracing::warn!(?error, "Playlist import preview не прошёл S08 staging");
                    if intent == PlaylistImportIntent::StartupReplace {
                        self.fail_startup_playlist_import(
                            "Startup playlist preview не прошёл allocator gate",
                        );
                    }
                    self.set_playlist_safe_feedback(
                        "Импорт устарел или сейчас недоступен; выберите файл ещё раз",
                    );
                }
                true
            }
            PlaylistImportJobCompletion::Failed(error) => {
                let safe_message = match error {
                    PlaylistImportJobError::UnsupportedRootFormat => {
                        "Выберите плейлист M3U, M3U8, XSPF или CUE"
                    }
                    PlaylistImportJobError::SourceRejected => {
                        "Файл не прошёл проверку формата плейлиста"
                    }
                    PlaylistImportJobError::WorkerPanicked => {
                        "Не удалось обработать файл плейлиста"
                    }
                };
                if self.startup_import.is_active() {
                    self.fail_startup_playlist_import(safe_message);
                }
                self.set_playlist_safe_feedback(safe_message);
                true
            }
        }
    }

    /// Stages parser result только после canonical allocator/load decision.
    fn stage_held_startup_playlist_draft(&mut self) -> bool {
        if self.controller.as_ref().is_none() {
            return false;
        }
        let Some(draft) = self.startup_import.take_held_draft() else {
            return false;
        };
        if let Err(error) = self.stage_playlist_import(PlaylistImportIntent::StartupReplace, draft)
        {
            tracing::warn!(?error, "Held startup playlist draft не прошёл S08 staging");
            self.fail_startup_playlist_import(
                "Startup playlist preview устарел до открытия allocator gate",
            );
        }
        true
    }
}

/// Заголовок объясняет destructive intent до native выбора файла.
const fn import_dialog_title(intent: PlaylistImportIntent) -> &'static str {
    match intent {
        PlaylistImportIntent::AppendToQueue => "Добавить плейлист",
        PlaylistImportIntent::ReplaceQueue => "Открыть как новый плейлист",
        PlaylistImportIntent::StartupReplace => "Открыть плейлист",
    }
}

/// Выполняет S05/S06/S07 validation и строит S08 source-neutral draft.
fn parse_selected_root(
    root_path: &Path,
    intent: PlaylistImportIntent,
    cancellation: &LocalPlaylistExpansionCancellation,
) -> PlaylistImportJobCompletion {
    if root_path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cue"))
    {
        return parse_selected_cue_root(root_path, intent, cancellation);
    }
    let request = LocalPlaylistExpansionRequest::new(
        root_path.to_path_buf(),
        LocalPlaylistExpansionLimits::default(),
        M3uParserLimits::default(),
        XspfParserLimits::default(),
        cancellation,
    );
    let expansion = match expand_local_playlist(request) {
        Ok(expansion) => expansion,
        Err(playlist_io::LocalPlaylistExpansionStartError::UnsupportedRootFormat) => {
            return PlaylistImportJobCompletion::Failed(
                PlaylistImportJobError::UnsupportedRootFormat,
            );
        }
        Err(playlist_io::LocalPlaylistExpansionStartError::RootPathMustBeAbsolute) => {
            return PlaylistImportJobCompletion::Failed(PlaylistImportJobError::SourceRejected);
        }
    };
    if expansion.summary().cancelled() {
        return PlaylistImportJobCompletion::Cancelled;
    }

    let draft = materializer::materialize_expansion(&expansion);
    PlaylistImportJobCompletion::Parsed { intent, draft }
}

/// Читает один CUE root строго в пределах S12 byte budget и materialize-ит Singles.
fn parse_selected_cue_root(
    root_path: &Path,
    intent: PlaylistImportIntent,
    cancellation: &LocalPlaylistExpansionCancellation,
) -> PlaylistImportJobCompletion {
    if !root_path.is_absolute() {
        return PlaylistImportJobCompletion::Failed(PlaylistImportJobError::SourceRejected);
    }
    if cancellation.is_cancelled() {
        return PlaylistImportJobCompletion::Cancelled;
    }
    let limits = CueParserLimits::default();
    let maximum_plus_sentinel = match limits.max_document_bytes().checked_add(1) {
        Some(limit) => limit,
        None => return PlaylistImportJobCompletion::Failed(PlaylistImportJobError::SourceRejected),
    };
    let read_limit = match u64::try_from(maximum_plus_sentinel) {
        Ok(limit) => limit,
        Err(_) => {
            return PlaylistImportJobCompletion::Failed(PlaylistImportJobError::SourceRejected);
        }
    };
    let file = match File::open(root_path) {
        Ok(file) => file,
        Err(_) => {
            return PlaylistImportJobCompletion::Failed(PlaylistImportJobError::SourceRejected);
        }
    };
    let mut document_bytes = Vec::with_capacity(limits.max_document_bytes());
    if file
        .take(read_limit)
        .read_to_end(&mut document_bytes)
        .is_err()
        || document_bytes.len() > limits.max_document_bytes()
    {
        return PlaylistImportJobCompletion::Failed(PlaylistImportJobError::SourceRejected);
    }
    if cancellation.is_cancelled() {
        return PlaylistImportJobCompletion::Cancelled;
    }
    let document = match parse_cue_document(CueParseRequest::new(
        &document_bytes,
        CueDocumentSource::local(root_path.to_path_buf()),
        limits,
    )) {
        Ok(document) => document,
        Err(_) => {
            return PlaylistImportJobCompletion::Failed(PlaylistImportJobError::SourceRejected);
        }
    };
    let entries = document
        .tracks()
        .iter()
        .map(|track| PlaylistImportEntryDraft::Single(track.import_draft().clone()))
        .collect();
    let draft = PlaylistImportDraft::new(entries, Vec::new(), None, 0);
    PlaylistImportJobCompletion::Parsed { intent, draft }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::app_wake::AppWakeOwner;

    #[test]
    fn m3u_xspf_and_cue_roots_use_authoritative_content_parsers() {
        let directory = tempfile::tempdir().expect("temporary import directory");
        let m3u_path = directory.path().join("queue.m3u8");
        fs::write(&m3u_path, "#EXTM3U\n#EXTINF:12,Трек\nsong.mp3\n").expect("write M3U fixture");
        let xspf_path = directory.path().join("queue.xspf");
        fs::write(
            &xspf_path,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<playlist version="1" xmlns="http://xspf.org/ns/0/">
  <trackList>
    <track>
      <location>file:///tmp/song.mp3</location>
      <title>Трек XSPF</title>
    </track>
  </trackList>
</playlist>"#,
        )
        .expect("write XSPF fixture");
        let cue_path = directory.path().join("album.cue");
        fs::write(
            &cue_path,
            "FILE \"album.flac\" FLAC\n\
             TRACK 01 AUDIO\nINDEX 01 00:00:00\n\
             TRACK 02 AUDIO\nINDEX 01 01:00:00\n",
        )
        .expect("write CUE fixture");

        for (path, intent, expected_entries) in [
            (&m3u_path, PlaylistImportIntent::AppendToQueue, 1),
            (&xspf_path, PlaylistImportIntent::ReplaceQueue, 1),
            (&cue_path, PlaylistImportIntent::AppendToQueue, 2),
        ] {
            let completion =
                parse_selected_root(path, intent, &LocalPlaylistExpansionCancellation::new());
            let PlaylistImportJobCompletion::Parsed {
                intent: actual_intent,
                draft,
            } = completion
            else {
                panic!("valid playlist root must produce staged draft");
            };
            let (entries, issues, truncated, _) = draft.test_summary();
            assert_eq!(actual_intent, intent);
            assert_eq!(entries, expected_entries);
            assert_eq!(issues, 0);
            assert!(!truncated);
        }
    }

    #[test]
    fn extension_filter_never_overrides_content_validation() {
        let directory = tempfile::tempdir().expect("temporary import directory");
        let invalid_xspf = directory.path().join("not-really-a-playlist.xspf");
        fs::write(&invalid_xspf, b"not XML").expect("write invalid XSPF fixture");

        let completion = parse_selected_root(
            &invalid_xspf,
            PlaylistImportIntent::AppendToQueue,
            &LocalPlaylistExpansionCancellation::new(),
        );
        let PlaylistImportJobCompletion::Parsed { draft, .. } = completion else {
            panic!("content failure must remain a typed partial preview");
        };
        let (entries, issues, _, _) = draft.test_summary();
        assert_eq!(entries, 0);
        assert!(issues > 0);

        let invalid_cue = directory.path().join("not-really-a-cue.cue");
        fs::write(&invalid_cue, b"#EXTM3U\nsong.flac\n").expect("write mismatched CUE fixture");
        let cue_completion = parse_selected_root(
            &invalid_cue,
            PlaylistImportIntent::AppendToQueue,
            &LocalPlaylistExpansionCancellation::new(),
        );
        assert!(matches!(
            cue_completion,
            PlaylistImportJobCompletion::Failed(PlaylistImportJobError::SourceRejected)
        ));
    }

    #[test]
    fn unsupported_root_format_is_rejected_before_filesystem_traversal() {
        let directory = tempfile::tempdir().expect("temporary import directory");
        let unsupported_path = directory.path().join("future.pls");
        fs::write(&unsupported_path, b"[playlist]").expect("write unsupported fixture");

        let completion = parse_selected_root(
            &unsupported_path,
            PlaylistImportIntent::AppendToQueue,
            &LocalPlaylistExpansionCancellation::new(),
        );
        assert!(matches!(
            completion,
            PlaylistImportJobCompletion::Failed(PlaylistImportJobError::UnsupportedRootFormat)
        ));
    }

    #[test]
    fn dialog_contract_lists_current_formats_and_selects_one_root() {
        let source = include_str!("import_io.rs");
        let dialog_start = source.find("fn spawn(").expect("dialog spawn source");
        let dialog_end = source[dialog_start..]
            .find("Self::spawn_runner")
            .map(|offset| dialog_start + offset)
            .expect("dialog runner call");
        let dialog_source = &source[dialog_start..dialog_end];

        assert!(dialog_source.contains(r#"&["m3u", "m3u8", "xspf", "cue"]"#));
        assert!(dialog_source.contains(".pick_file()"));
        assert!(!dialog_source.contains(".pick_files()"));
        assert!(dialog_source.contains(r#".add_filter("CUE", &["cue"])"#));
    }

    #[test]
    fn supersede_suppresses_completion_published_before_owner_drain() {
        let wake_port = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
        let job = PlaylistImportJob::spawn_runner(
            wake_port.clone(),
            "playlist-import-published-supersede-test",
            move |_worker_cancel, _expansion_cancellation| {
                PlaylistImportJobCompletion::Failed(PlaylistImportJobError::SourceRejected)
            },
        )
        .expect("spawn test import job");
        let mut owner = PlaylistImportIoOwner {
            wake_port,
            job: Some(job),
        };
        while !owner
            .job
            .as_ref()
            .and_then(|job| job.join_handle.as_ref())
            .is_some_and(JoinHandle::is_finished)
        {
            std::thread::yield_now();
        }

        owner.cancel_active();
        let completion = owner.drain().expect("published terminal completion");

        assert!(matches!(completion, PlaylistImportJobCompletion::Cancelled));
        assert!(!owner.is_open());
    }
}
