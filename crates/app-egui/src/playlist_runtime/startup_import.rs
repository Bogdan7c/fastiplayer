//! Trusted startup/desktop playlist import orchestration.
//!
//! Модуль не парсит документы и не выделяет ID. Он удерживает ID-less draft до
//! allocator gate, принимает exact receipt единственного `StartupReplace` commit-а
//! и только после него строит plan первого source-order queue item.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use playlist_core::{PlaylistItemId, QueueRevisionSnapshot, ReservedQueueMutation};

use super::PlaylistRuntime;
use super::controller::{PlannedPlaylistInstall, PlaylistInstallMutation, StablePlaybackIntent};
use super::identity::{PendingTargetOrigin, TransportActionOrigin};
use super::import_transaction::{PlaylistImportDraft, PlaylistImportIntent};

/// Exact queue identity, опубликованная только после successful `StartupReplace`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StartupPlaylistCommitReceipt {
    /// Первый playable Item ID в source order committed prefix-а.
    first_item_id: PlaylistItemId,
    /// Queue revision защищает от user mutation между commit и strong-open start.
    expected_queue_revision: QueueRevisionSnapshot,
}

/// Terminal startup import result, который exactly once забирает startup controller.
#[derive(Debug)]
pub(crate) enum StartupPlaylistImportTerminal {
    /// Queue уже committed; receipt разрешает открыть только exact первый item.
    Committed(StartupPlaylistCommitReceipt),
    /// Empty accepted prefix не мутировал queue и не выделял allocator IDs.
    Empty,
    /// Parser/materializer/controller завершил trusted import безопасной ошибкой.
    Failed(String),
    /// Preview или background parse был явно отменён/superseded.
    Cancelled,
}

/// Typed failure построения post-commit first-item plan-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupPlaylistPlanError {
    /// Controller недоступен после startup load decision.
    ControllerUnavailable,
    /// Queue изменилась после commit receipt-а.
    Superseded,
    /// Stable playback intent revision исчерпана.
    IntentRevisionExhausted,
}

impl std::fmt::Display for StartupPlaylistPlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ControllerUnavailable => formatter.write_str("playlist controller недоступен"),
            Self::Superseded => formatter.write_str("startup playlist commit уже superseded"),
            Self::IntentRevisionExhausted => {
                formatter.write_str("исчерпана revision startup playback intent")
            }
        }
    }
}

impl std::error::Error for StartupPlaylistPlanError {}

/// Process-lifetime bridge между import I/O, S08 transaction и startup controller.
#[derive(Default)]
pub(super) struct StartupPlaylistImportState {
    /// Active trusted startup import отличает его terminal от interactive import-а.
    active: bool,
    /// Parsed ID-less draft ждёт allocator gate, не занимая queue IDs заранее.
    held_draft: Option<PlaylistImportDraft>,
    /// Exactly-once terminal mailbox внутри serialized runtime owner-а.
    terminal: Option<StartupPlaylistImportTerminal>,
}

impl StartupPlaylistImportState {
    /// Начинает новую trusted generation после отмены прежнего import flow.
    fn begin(&mut self) {
        self.active = true;
        self.held_draft = None;
        self.terminal = None;
    }

    /// Supersede отнимает apply authority у parser/preview/receipt.
    pub(super) fn supersede(&mut self) {
        self.active = false;
        self.held_draft = None;
        self.terminal = None;
    }

    /// Возвращает, принадлежит ли текущий import trusted startup flow.
    pub(super) const fn is_active(&self) -> bool {
        self.active
    }

    /// Удерживает готовый draft до появления canonical queue owner-а.
    pub(super) fn hold_draft(&mut self, draft: PlaylistImportDraft) {
        if self.active {
            self.held_draft = Some(draft);
        }
    }

    /// Передаёт held draft staging boundary ровно один раз.
    pub(super) fn take_held_draft(&mut self) -> Option<PlaylistImportDraft> {
        self.active.then(|| self.held_draft.take()).flatten()
    }

    /// Публикует terminal только активной trusted generation.
    pub(super) fn finish(&mut self, terminal: StartupPlaylistImportTerminal) {
        if self.active {
            self.active = false;
            self.held_draft = None;
            self.terminal = Some(terminal);
        }
    }

    /// Exactly-once consumer terminal receipt-а.
    fn take_terminal(&mut self) -> Option<StartupPlaylistImportTerminal> {
        self.terminal.take()
    }
}

impl PlaylistRuntime {
    /// Запускает authoritative parser для exact CLI/desktop path без native dialog.
    pub(crate) fn start_startup_playlist_import(
        &mut self,
        root_path: PathBuf,
    ) -> Result<bool, String> {
        if !self.admission_open.load(Ordering::Acquire) {
            return Err("Приложение уже завершает startup import".to_owned());
        }
        if self.import_io.is_open() {
            return Ok(false);
        }

        let root_path = if root_path.is_absolute() {
            root_path
        } else {
            std::env::current_dir()
                .map_err(|_| "Не удалось разрешить relative startup playlist path".to_owned())?
                .join(root_path)
        };
        self.supersede_playlist_import_flow();
        self.replacement_confirmation.cancel();
        self.startup_import.begin();
        match self
            .import_io
            .start_path(root_path, PlaylistImportIntent::StartupReplace)
        {
            Ok(started) => Ok(started),
            Err(error) => {
                self.startup_import
                    .finish(StartupPlaylistImportTerminal::Failed(error.clone()));
                Err(error)
            }
        }
    }

    /// Отменяет parser/preview authority при D65 supersede или terminal startup lifecycle.
    pub(crate) fn cancel_startup_playlist_import(&mut self) {
        self.import_io.cancel_active();
        self.import_transaction.cancel();
        self.startup_import
            .finish(StartupPlaylistImportTerminal::Cancelled);
    }

    /// Забирает exact post-commit terminal без чтения queue internals снаружи runtime-а.
    pub(crate) fn take_startup_playlist_import_terminal(
        &mut self,
    ) -> Option<StartupPlaylistImportTerminal> {
        self.startup_import.take_terminal()
    }

    /// После commit receipt-а строит единственный first-item plan без sibling/next scan.
    pub(crate) fn plan_startup_playlist_first_install(
        &mut self,
        receipt: StartupPlaylistCommitReceipt,
        autoplay: bool,
    ) -> Result<PlannedPlaylistInstall, StartupPlaylistPlanError> {
        let controller = self
            .controller
            .as_mut()
            .ok_or(StartupPlaylistPlanError::ControllerUnavailable)?;
        if controller.queue().revision_snapshot() != receipt.expected_queue_revision
            || controller.queue().iter_playable_ids().next() != Some(receipt.first_item_id)
        {
            return Err(StartupPlaylistPlanError::Superseded);
        }

        let stable_intent = if autoplay {
            StablePlaybackIntent::Playing
        } else {
            StablePlaybackIntent::Paused
        };
        let intent_dispatch = controller
            .record_stable_transport_intent(stable_intent, TransportActionOrigin::Startup)
            .ok_or(StartupPlaylistPlanError::IntentRevisionExhausted)?;
        Ok(PlannedPlaylistInstall {
            item_id: receipt.first_item_id,
            playback_intent: intent_dispatch.intent,
            intent_revision: intent_dispatch.revision,
            pending_origin: PendingTargetOrigin::ExplicitOpen,
            expected_queue_revision: receipt.expected_queue_revision,
            mutation: PlaylistInstallMutation::Reserved(ReservedQueueMutation::select_committed(
                receipt.first_item_id,
            )),
        })
    }

    /// Commit owner публикует first-item receipt из canonical queue revision.
    pub(super) fn finish_startup_playlist_commit(&mut self, first_item_id: Option<PlaylistItemId>) {
        let terminal = match first_item_id {
            Some(first_item_id) => {
                let Some(controller) = self.controller.as_ref() else {
                    self.startup_import
                        .finish(StartupPlaylistImportTerminal::Failed(
                            "playlist controller исчез после startup commit".to_owned(),
                        ));
                    return;
                };
                StartupPlaylistImportTerminal::Committed(StartupPlaylistCommitReceipt {
                    first_item_id,
                    expected_queue_revision: controller.queue().revision_snapshot(),
                })
            }
            None => StartupPlaylistImportTerminal::Empty,
        };
        self.startup_import.finish(terminal);
    }

    /// Ошибка startup parser/staging/commit становится typed terminal без raw path payload.
    pub(super) fn fail_startup_playlist_import(&mut self, safe_message: impl Into<String>) {
        self.startup_import
            .finish(StartupPlaylistImportTerminal::Failed(safe_message.into()));
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use playlist_core::{
        CachedPlaylistMetadata, DurableReopenLocator, LocalLocator, MAX_PLAYLIST_ITEMS,
        PlaylistCompoundImportDraft, PlaylistImportAvailability, PlaylistImportEntryDraft,
        PlaylistImportProvenance, PlaylistImportSourceKind, PlaylistMediaKind,
        PlaylistSingleImportDraft,
    };

    use super::*;
    use crate::app_wake::{AppWakeOwner, AppWakePort};
    use crate::playlist_runtime::controller::{ControllerImportCommitOutcome, PlaylistController};
    use crate::playlist_runtime::import_transaction::{
        PlaylistImportContinueOutcome, PlaylistImportDraft, PlaylistImportIssue,
        PlaylistImportIssueKind,
    };

    fn runtime() -> PlaylistRuntime {
        let wake = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
        let mut runtime =
            PlaylistRuntime::new_with_config(wake, rustiplayer_config::PlaylistConfig::default());
        runtime.controller.install(PlaylistController::new());
        runtime
    }

    fn metadata(label: &str) -> CachedPlaylistMetadata {
        CachedPlaylistMetadata::new(label, PlaylistMediaKind::Unknown)
    }

    fn provenance(root: DurableReopenLocator) -> PlaylistImportProvenance {
        PlaylistImportProvenance::new(root, PlaylistImportSourceKind::M3u, None)
    }

    fn single(label: &str) -> PlaylistSingleImportDraft {
        let locator = DurableReopenLocator::local(LocalLocator::Native(PathBuf::from(format!(
            "/{label}.mkv"
        ))));
        PlaylistSingleImportDraft::new(
            locator.clone(),
            metadata(label),
            None,
            Vec::new(),
            provenance(locator),
            PlaylistImportAvailability::Available,
        )
        .expect("focused startup single")
    }

    fn compound(label: &str, part_count: usize) -> PlaylistImportEntryDraft {
        let root = DurableReopenLocator::local(LocalLocator::Native(PathBuf::from(format!(
            "/{label}.collection"
        ))));
        let parts = (0..part_count)
            .map(|index| single(&format!("{label}-{index}")))
            .collect();
        PlaylistImportEntryDraft::Compound(
            PlaylistCompoundImportDraft::new(
                root.clone(),
                metadata(label),
                provenance(root),
                parts,
            )
            .expect("focused startup compound"),
        )
    }

    fn begin_trusted_transaction(runtime: &mut PlaylistRuntime) {
        runtime.startup_import.begin();
    }

    #[test]
    fn startup_commit_precedes_first_plan_and_accounts_item_and_group_allocators_exactly() {
        let mut runtime = runtime();
        begin_trusted_transaction(&mut runtime);
        let initial_item_allocator = runtime.controller.queue().next_item_id_snapshot();
        let initial_group_allocator = runtime.controller.queue().next_compound_group_id_snapshot();
        let preview = runtime
            .stage_playlist_import(
                PlaylistImportIntent::StartupReplace,
                PlaylistImportDraft::new(
                    vec![
                        PlaylistImportEntryDraft::Single(single("first")),
                        compound("group", 2),
                        PlaylistImportEntryDraft::Single(single("last")),
                    ],
                    Vec::new(),
                    None,
                    0,
                ),
            )
            .expect("startup preview");

        assert_eq!(runtime.controller.queue().retained_item_count(), 0);
        assert_eq!(
            runtime.controller.queue().next_item_id_snapshot(),
            initial_item_allocator
        );
        assert_eq!(
            runtime.controller.queue().next_compound_group_id_snapshot(),
            initial_group_allocator
        );

        let outcome = runtime.continue_playlist_import(preview.preview_id());
        let PlaylistImportContinueOutcome::Committed(ControllerImportCommitOutcome::Committed {
            allocated,
            ..
        }) = outcome
        else {
            panic!("trusted startup must commit without a second confirmation");
        };
        assert_eq!(allocated.top_level_entry_count(), 3);
        assert_eq!(allocated.retained_item_count(), 4);
        assert!(runtime.pending_playlist_confirmation().is_none());
        assert_eq!(runtime.controller.queue().retained_item_count(), 4);
        assert_eq!(
            runtime
                .controller
                .queue()
                .next_item_id_snapshot()
                .expose_value_for_persistence(),
            5
        );
        assert_eq!(
            runtime
                .controller
                .queue()
                .next_compound_group_id_snapshot()
                .expose_value_for_persistence(),
            2
        );
        assert!(runtime.controller.queue().traversal_current().is_none());

        let StartupPlaylistImportTerminal::Committed(receipt) = runtime
            .take_startup_playlist_import_terminal()
            .expect("post-commit receipt")
        else {
            panic!("non-empty startup commit must publish first-item receipt");
        };
        let plan = runtime
            .plan_startup_playlist_first_install(receipt, false)
            .expect("first source-order plan");
        assert_eq!(
            Some(plan.item_id),
            runtime.controller.queue().iter_playable_ids().next()
        );
        assert!(runtime.controller.queue().traversal_current().is_none());
    }

    #[test]
    fn empty_and_partial_startup_imports_preserve_explicit_decision_and_zero_allocation() {
        let mut empty_runtime = runtime();
        begin_trusted_transaction(&mut empty_runtime);
        let initial_item_allocator = empty_runtime.controller.queue().next_item_id_snapshot();
        let initial_group_allocator = empty_runtime
            .controller
            .queue()
            .next_compound_group_id_snapshot();
        let empty_preview = empty_runtime
            .stage_playlist_import(
                PlaylistImportIntent::StartupReplace,
                PlaylistImportDraft::new(Vec::new(), Vec::new(), None, 0),
            )
            .expect("empty startup preview");
        assert!(matches!(
            empty_runtime.continue_playlist_import(empty_preview.preview_id()),
            PlaylistImportContinueOutcome::Committed(
                ControllerImportCommitOutcome::NoEntriesProvided
            )
        ));
        assert!(matches!(
            empty_runtime.take_startup_playlist_import_terminal(),
            Some(StartupPlaylistImportTerminal::Empty)
        ));
        assert_eq!(
            empty_runtime.controller.queue().next_item_id_snapshot(),
            initial_item_allocator
        );
        assert_eq!(
            empty_runtime
                .controller
                .queue()
                .next_compound_group_id_snapshot(),
            initial_group_allocator
        );

        let mut partial_runtime = runtime();
        begin_trusted_transaction(&mut partial_runtime);
        let partial_preview = partial_runtime
            .stage_playlist_import(
                PlaylistImportIntent::StartupReplace,
                PlaylistImportDraft::new(
                    vec![PlaylistImportEntryDraft::Single(single("accepted"))],
                    vec![PlaylistImportIssue::new(
                        PlaylistImportIssueKind::SourceRejectedEntry,
                    )],
                    None,
                    1,
                ),
            )
            .expect("partial startup preview");
        assert!(partial_preview.requires_partial_decision());
        assert_eq!(partial_runtime.controller.queue().retained_item_count(), 0);
        assert!(
            partial_runtime
                .take_startup_playlist_import_terminal()
                .is_none()
        );
        assert!(matches!(
            partial_runtime.continue_playlist_import(partial_preview.preview_id()),
            PlaylistImportContinueOutcome::Committed(_)
        ));
        assert!(partial_runtime.pending_playlist_confirmation().is_none());
    }

    #[test]
    fn startup_capacity_commits_exact_capped_prefix_only_after_continue() {
        let mut runtime = runtime();
        begin_trusted_transaction(&mut runtime);
        let repeated = single("capacity");
        let entries = (0..=MAX_PLAYLIST_ITEMS)
            .map(|_| PlaylistImportEntryDraft::Single(repeated.clone()))
            .collect();
        let preview = runtime
            .stage_playlist_import(
                PlaylistImportIntent::StartupReplace,
                PlaylistImportDraft::new(entries, Vec::new(), None, 0),
            )
            .expect("capacity preview");

        assert!(preview.requires_partial_decision());
        assert_eq!(runtime.controller.queue().retained_item_count(), 0);
        assert!(matches!(
            runtime.continue_playlist_import(preview.preview_id()),
            PlaylistImportContinueOutcome::Committed(_)
        ));
        assert_eq!(
            runtime.controller.queue().retained_item_count(),
            MAX_PLAYLIST_ITEMS
        );
        assert!(matches!(
            runtime.take_startup_playlist_import_terminal(),
            Some(StartupPlaylistImportTerminal::Committed(_))
        ));
    }

    #[test]
    fn competing_structural_intent_supersedes_held_startup_draft_and_receipt() {
        let mut runtime = runtime();
        begin_trusted_transaction(&mut runtime);
        runtime.startup_import.hold_draft(PlaylistImportDraft::new(
            vec![PlaylistImportEntryDraft::Single(single("late"))],
            Vec::new(),
            None,
            0,
        ));

        runtime.supersede_playlist_import_flow();

        assert!(!runtime.startup_import.is_active());
        assert!(runtime.startup_import.take_held_draft().is_none());
        assert!(runtime.take_startup_playlist_import_terminal().is_none());
        assert_eq!(runtime.controller.queue().retained_item_count(), 0);
    }

    #[test]
    fn each_startup_format_uses_startup_replace_and_cue_keeps_first_track_window() {
        let directory = tempfile::tempdir().expect("startup playlist directory");
        let fixtures = [
            (
                "queue.m3u",
                "#EXTM3U\n#EXTINF:12,Track\nsong.mp3\n".to_owned(),
                false,
            ),
            (
                "queue.m3u8",
                "#EXTM3U\n#EXTINF:12,Track\nsong.mp3\n".to_owned(),
                false,
            ),
            (
                "queue.xspf",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<playlist version="1" xmlns="http://xspf.org/ns/0/">
  <trackList><track><location>file:///tmp/song.mp3</location></track></trackList>
</playlist>"#
                    .to_owned(),
                false,
            ),
            (
                "album.cue",
                "FILE \"album.flac\" FLAC\n  TRACK 01 AUDIO\n    INDEX 01 00:10:00\n  TRACK 02 AUDIO\n    INDEX 01 01:00:00\n"
                    .to_owned(),
                true,
            ),
        ];

        for (file_name, contents, expects_window) in fixtures {
            let path = directory.path().join(file_name);
            fs::write(&path, contents).expect("write startup fixture");
            let mut runtime = runtime();
            assert!(
                runtime
                    .start_startup_playlist_import(path)
                    .expect("start exact startup path")
            );
            let deadline = Instant::now() + Duration::from_secs(2);
            let preview = loop {
                runtime.drain_owner_mailbox();
                if let Some(preview) = runtime.pending_playlist_import_preview() {
                    break preview;
                }
                assert!(
                    Instant::now() < deadline,
                    "startup parser timeout: {file_name}"
                );
                std::thread::yield_now();
            };
            assert_eq!(preview.intent(), PlaylistImportIntent::StartupReplace);
            assert!(matches!(
                runtime.continue_playlist_import(preview.preview_id()),
                PlaylistImportContinueOutcome::Committed(_)
            ));
            let StartupPlaylistImportTerminal::Committed(receipt) = runtime
                .take_startup_playlist_import_terminal()
                .expect("format commit receipt")
            else {
                panic!("{file_name} must commit at least one entry");
            };
            let plan = runtime
                .plan_startup_playlist_first_install(receipt, false)
                .expect("source-order first plan");
            let open_intent = runtime
                .media_open_intent_for_planned_install(&plan)
                .expect("first committed locator/window");
            assert_eq!(open_intent.playback_window().is_some(), expects_window);

            // Unstaged failure marks only exact first row; no second plan/scan is produced.
            runtime.report_unstaged_playlist_navigation_failure(plan.item_id);
            assert!(runtime.take_startup_playlist_import_terminal().is_none());
            assert!(runtime.controller.queue().traversal_current().is_none());
        }
    }
}
