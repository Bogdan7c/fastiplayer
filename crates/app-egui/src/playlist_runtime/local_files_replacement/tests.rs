use std::path::PathBuf;

use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, MAX_PLAYLIST_ITEMS, PlaylistItemDraft, PlaylistMediaKind,
};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::controller::{
    ControllerAppendOutcome, ControllerPlayItemOutcome, PlaylistController,
};
use crate::playlist_runtime::replacement_confirmation::{
    AdmittedQueueReplacementIntent, InAppQueueReplacementAdmission, InAppQueueReplacementIntent,
    PlaylistConfirmationAction, QueueReplacementConfirmationDecision,
};
use crate::playlist_runtime::{PlaylistConfirmationApplyOutcome, RuntimeRowPlayOutcome};

fn runtime_without_load_decision() -> PlaylistRuntime {
    let wake = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    PlaylistRuntime::new_with_config(wake, fastiplayer_config::PlaylistConfig::default())
}

fn runtime_with_queue(existing: &[&str]) -> PlaylistRuntime {
    let mut runtime = runtime_without_load_decision();
    runtime.controller.install(PlaylistController::new());
    if !existing.is_empty() {
        let drafts = existing
            .iter()
            .map(|name| {
                PlaylistItemDraft::local(
                    LocalLocator::Native(PathBuf::from(name)),
                    None,
                    CachedPlaylistMetadata::new(*name, PlaylistMediaKind::Video),
                )
            })
            .collect();
        assert!(matches!(
            runtime.controller.append(drafts).expect("existing rows"),
            ControllerAppendOutcome::Added { .. }
        ));
    }
    runtime
}

/// Нативные пути очереди в её порядке.
fn queue_paths(runtime: &PlaylistRuntime) -> Vec<PathBuf> {
    let controller = runtime.controller.as_ref().expect("controller installed");
    controller
        .queue()
        .iter_playable_items()
        .map(|item| {
            item.locator()
                .as_local()
                .and_then(LocalLocator::expose_native_path_for_open)
                .expect("local row")
                .to_path_buf()
        })
        .collect()
}

fn paths(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

#[test]
fn replacing_with_files_gives_exact_queue_in_drop_order_and_first_row_starts_playing() {
    let mut runtime = runtime_with_queue(&["/old/one.mkv", "/old/two.mkv"]);

    let outcome = runtime.replace_queue_with_local_files(paths(&["/c.mkv", "/a.mkv", "/b.mkv"]));

    let LocalFilesReplacementOutcome::Replaced {
        first_item,
        file_count,
    } = outcome
    else {
        panic!("replacement must succeed, got {outcome:?}");
    };
    assert_eq!(file_count, 3);
    assert_eq!(
        queue_paths(&runtime),
        paths(&["/c.mkv", "/a.mkv", "/b.mkv"])
    );
    let first_in_queue = runtime
        .controller
        .as_ref()
        .and_then(|controller| controller.queue().iter_playable_ids().next())
        .expect("queue has first row");
    assert_eq!(
        first_item, first_in_queue,
        "играть надо именно первый файл броска"
    );

    // Первый файл действительно запускает установку media обычным Row Play.
    assert!(matches!(
        runtime.play_playlist_row(first_item),
        RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::StartInstall { .. })
    ));
}

#[test]
fn non_utf8_and_spaced_paths_are_stored_without_distortion() {
    let mut runtime = runtime_with_queue(&[]);
    #[cfg(unix)]
    let odd_path = {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(
            b"/data/bad\xFFname x.mkv".to_vec(),
        ))
    };
    #[cfg(not(unix))]
    let odd_path = PathBuf::from("/data/name with spaces.mkv");

    let outcome =
        runtime.replace_queue_with_local_files(vec![odd_path.clone(), PathBuf::from("/b.mkv")]);

    assert!(matches!(
        outcome,
        LocalFilesReplacementOutcome::Replaced { file_count: 2, .. }
    ));
    assert_eq!(
        queue_paths(&runtime),
        vec![odd_path, PathBuf::from("/b.mkv")]
    );
}

#[test]
fn rejections_leave_existing_queue_untouched() {
    let mut runtime = runtime_with_queue(&["/old.mkv"]);

    assert_eq!(
        runtime.replace_queue_with_local_files(Vec::new()),
        LocalFilesReplacementOutcome::NoFilesProvided
    );
    let too_many = (0..=MAX_PLAYLIST_ITEMS)
        .map(|index| PathBuf::from(format!("/f{index}.mkv")))
        .collect();
    assert_eq!(
        runtime.replace_queue_with_local_files(too_many),
        LocalFilesReplacementOutcome::TooManyFiles
    );

    assert_eq!(queue_paths(&runtime), paths(&["/old.mkv"]));
}

#[test]
fn before_load_decision_replacement_is_refused_and_reported_as_pending() {
    let mut runtime = runtime_without_load_decision();
    assert!(runtime.queue_load_decision_is_pending());

    let outcome = runtime.replace_queue_with_local_files(paths(&["/a.mkv", "/b.mkv"]));

    assert_eq!(outcome, LocalFilesReplacementOutcome::LoadDecisionPending);
    assert!(
        runtime.queue_load_decision_is_pending(),
        "отказ не создаёт очередь"
    );
}

#[test]
fn nonempty_queue_asks_confirmation_and_changes_only_after_confirm() {
    let mut runtime = runtime_with_queue(&["/old.mkv"]);

    let admission = runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_files(paths(&[
            "/a.mkv", "/b.mkv",
        ])))
        .expect("admission");
    assert!(matches!(
        admission,
        InAppQueueReplacementAdmission::AwaitingConfirmation
    ));
    assert_eq!(
        queue_paths(&runtime),
        paths(&["/old.mkv"]),
        "до ответа очередь не меняется"
    );
    let model = runtime
        .pending_playlist_confirmation()
        .expect("pending confirmation");
    assert_eq!(model.safe_label(), "2 файла");
    assert!(model.reasons().queue_replacement());

    let outcome = runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
        intent_id: model.intent_id(),
        decision: QueueReplacementConfirmationDecision::Confirm,
    });
    let PlaylistConfirmationApplyOutcome::QueueReplacementConfirmed(
        AdmittedQueueReplacementIntent::LocalFiles(files),
    ) = outcome
    else {
        panic!("confirm must hand the files to the replacement owner, got {outcome:?}");
    };
    assert_eq!(
        queue_paths(&runtime),
        paths(&["/old.mkv"]),
        "Confirm сам очередь не меняет"
    );

    assert!(matches!(
        runtime.replace_queue_with_local_files(files.into_paths()),
        LocalFilesReplacementOutcome::Replaced { file_count: 2, .. }
    ));
    assert_eq!(queue_paths(&runtime), paths(&["/a.mkv", "/b.mkv"]));
}

#[test]
fn cancel_keeps_queue_and_clears_the_confirmation() {
    let mut runtime = runtime_with_queue(&["/old.mkv"]);
    runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_files(paths(&[
            "/a.mkv", "/b.mkv",
        ])))
        .expect("admission");
    let model = runtime.pending_playlist_confirmation().expect("pending");

    let outcome = runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
        intent_id: model.intent_id(),
        decision: QueueReplacementConfirmationDecision::Cancel,
    });

    assert!(matches!(
        outcome,
        PlaylistConfirmationApplyOutcome::Cancelled
    ));
    assert!(runtime.pending_playlist_confirmation().is_none());
    assert_eq!(queue_paths(&runtime), paths(&["/old.mkv"]));
}

#[test]
fn empty_queue_admits_files_immediately_without_confirmation() {
    let mut runtime = runtime_with_queue(&[]);

    let admission = runtime
        .admit_in_app_queue_replacement(InAppQueueReplacementIntent::local_files(paths(&[
            "/a.mkv", "/b.mkv",
        ])))
        .expect("admission");

    let InAppQueueReplacementAdmission::StartNow(AdmittedQueueReplacementIntent::LocalFiles(files)) =
        admission
    else {
        panic!("empty queue must start immediately, got {admission:?}");
    };
    assert_eq!(files.into_paths(), paths(&["/a.mkv", "/b.mkv"]));
    assert!(runtime.pending_playlist_confirmation().is_none());
}

/// Прогоняет реальный Manual Add (probe настоящих WAV) до добавления `expected_len` строк.
fn drain_manual_add_until_queue_len(runtime: &mut PlaylistRuntime, expected_len: usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while queue_paths(runtime).len() < expected_len {
        runtime.drain_playlist_discovery();
        assert!(
            std::time::Instant::now() < deadline,
            "manual add must append all files"
        );
        std::thread::yield_now();
    }
}

/// Два реальных WAV `a.wav`, `b.wav` во временной папке.
fn two_real_wavs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let directory = tempfile::tempdir().expect("temp dir");
    let wav_bytes = crate::media_open::local::tests::pcm_wav_bytes();
    let first = directory.path().join("a.wav");
    let second = directory.path().join("b.wav");
    std::fs::write(&first, &wav_bytes).expect("first wav");
    std::fs::write(&second, &wav_bytes).expect("second wav");
    (directory, first, second)
}

#[test]
fn dropping_real_files_on_playlist_appends_them_in_drop_order_and_keeps_old_rows() {
    let (_directory, first, second) = two_real_wavs();
    let mut runtime = runtime_with_queue(&["/old.mkv"]);

    runtime
        .start_manual_file_add_in_given_order(vec![second.clone(), first.clone()])
        .expect("manual add must start");
    drain_manual_add_until_queue_len(&mut runtime, 3);

    // Порядок броска (b, a) сохранён: решение владельца 10.
    assert_eq!(
        queue_paths(&runtime),
        vec![PathBuf::from("/old.mkv"), second, first]
    );
}

#[test]
fn add_files_dialog_manual_add_keeps_natural_sort() {
    let (_directory, first, second) = two_real_wavs();
    let mut runtime = runtime_with_queue(&["/old.mkv"]);

    runtime
        .start_manual_file_add(vec![second.clone(), first.clone()])
        .expect("manual add must start");
    drain_manual_add_until_queue_len(&mut runtime, 3);

    assert_eq!(
        queue_paths(&runtime),
        vec![PathBuf::from("/old.mkv"), first, second]
    );
}
