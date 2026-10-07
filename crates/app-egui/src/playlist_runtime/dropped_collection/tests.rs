use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use playlist_core::{CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::DroppedCollectionTruncation;
use crate::playlist_runtime::controller::{ControllerPlayItemOutcome, PlaylistController};
use crate::playlist_runtime::{
    AdmittedQueueReplacementIntent, InAppQueueReplacementAdmission, InAppQueueReplacementIntent,
    LocalFilesReplacementOutcome, PlaylistConfirmationAction, PlaylistConfirmationApplyOutcome,
    QueueReplacementConfirmationDecision, RuntimeRowPlayOutcome,
};

fn limits(max_files: usize, max_depth: usize) -> DroppedCollectionLimits {
    DroppedCollectionLimits {
        max_files: NonZeroUsize::new(max_files).expect("non-zero"),
        max_subfolder_depth: max_depth,
    }
}

fn touch(root: &Path, relative: &str) -> PathBuf {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent dirs");
    }
    fs::write(&path, b"x").expect("fixture file");
    path
}

fn collect(
    entries: &[DroppedCollectionEntry],
    limits: DroppedCollectionLimits,
) -> CollectedDroppedFiles {
    collect_dropped_files(
        entries,
        DroppedCollectionDestination::ReplaceQueue,
        limits,
        &CancellationToken::new(),
    )
    .expect("walk is not cancelled")
}

#[test]
fn folders_expand_in_place_between_dropped_files_in_drop_order() {
    let root = tempfile::tempdir().expect("dir");
    let first_file = touch(root.path(), "z first.mkv");
    let album = root.path().join("album");
    let track2 = touch(&album, "track 10.flac");
    let track1 = touch(&album, "track 2.flac");
    let nested = touch(&album, "disc2/bonus.mp3");
    touch(&album, "cover.jpg");
    touch(&album, ".hidden.mp3");
    let last_file = touch(root.path(), "a last.mkv");

    let collected = collect(
        &[
            DroppedCollectionEntry::File(first_file.clone()),
            DroppedCollectionEntry::Folder(album),
            DroppedCollectionEntry::File(last_file.clone()),
        ],
        limits(100, 8),
    );

    assert_eq!(
        collected.files,
        vec![first_file, track1, track2, nested, last_file],
        "файл, затем папка на своём месте (natural, файлы раньше подпапок, без cover/hidden), затем файл"
    );
    assert_eq!(collected.truncation, None);
    assert_eq!(
        collected.source.map(|source| source.first_folder_name),
        Some("album".to_owned())
    );
}

#[test]
fn file_limit_is_shared_across_the_whole_drop_and_reported() {
    let root = tempfile::tempdir().expect("dir");
    let single = touch(root.path(), "single.mkv");
    let folder = root.path().join("f");
    let a = touch(&folder, "a.mkv");
    touch(&folder, "b.mkv");
    touch(&folder, "c.mkv");

    let collected = collect(
        &[
            DroppedCollectionEntry::File(single.clone()),
            DroppedCollectionEntry::Folder(folder),
        ],
        limits(2, 8),
    );

    assert_eq!(collected.files, vec![single, a]);
    assert_eq!(
        collected.truncation,
        Some(DroppedCollectionTruncation::FileLimit)
    );
}

#[test]
fn exhausted_budget_with_empty_next_folder_is_not_reported_as_truncation() {
    let root = tempfile::tempdir().expect("dir");
    let only = touch(root.path(), "only.mkv");
    let empty_folder = root.path().join("empty");
    fs::create_dir(&empty_folder).expect("empty folder");

    let collected = collect(
        &[
            DroppedCollectionEntry::File(only.clone()),
            DroppedCollectionEntry::Folder(empty_folder),
        ],
        limits(1, 8),
    );

    assert_eq!(collected.files, vec![only]);
    assert_eq!(collected.truncation, None);
}

#[test]
fn folder_without_media_gives_empty_result_and_unreadable_root_is_counted() {
    let root = tempfile::tempdir().expect("dir");
    touch(root.path(), "docs/readme.txt");
    let missing = root.path().join("vanished");

    let collected = collect(
        &[
            DroppedCollectionEntry::Folder(root.path().join("docs")),
            DroppedCollectionEntry::Folder(missing),
        ],
        limits(10, 8),
    );

    assert!(collected.files.is_empty());
    assert_eq!(collected.unreadable_count, 1);
}

#[test]
fn depth_limit_is_reported_separately_from_file_limit() {
    let root = tempfile::tempdir().expect("dir");
    let top = touch(root.path(), "top.mkv");
    touch(root.path(), "sub/deep.mkv");

    let collected = collect(
        &[DroppedCollectionEntry::Folder(root.path().to_path_buf())],
        limits(10, 0),
    );

    assert_eq!(collected.files, vec![top]);
    assert_eq!(
        collected.truncation,
        Some(DroppedCollectionTruncation::DepthLimit)
    );
}

#[test]
fn cancelled_token_yields_no_partial_result() {
    let root = tempfile::tempdir().expect("dir");
    touch(root.path(), "a.mkv");
    let token = CancellationToken::new();
    token.cancel();

    let result = collect_dropped_files(
        &[DroppedCollectionEntry::Folder(root.path().to_path_buf())],
        DroppedCollectionDestination::AppendToQueue,
        limits(10, 8),
        &token,
    );

    assert_eq!(result.err(), Some(DroppedCollectionCancelled));
}

fn runtime_with_queue(existing: &[&str]) -> PlaylistRuntime {
    let wake = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let mut runtime = PlaylistRuntime::new(wake);
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
        runtime.controller.append(drafts).expect("existing rows");
    }
    runtime
}

fn queue_paths(runtime: &PlaylistRuntime) -> Vec<PathBuf> {
    runtime
        .controller
        .as_ref()
        .expect("controller")
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

/// Ждёт завершения фонового обхода; зависание — точный провал теста.
fn wait_for_walk(runtime: &mut PlaylistRuntime) -> DroppedCollectionWalkCompletion {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(completion) = runtime.take_dropped_collection_walk_completion() {
            return completion;
        }
        assert!(Instant::now() < deadline, "обход не завершился за 5 секунд");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn collected_from(completion: DroppedCollectionWalkCompletion) -> CollectedDroppedFiles {
    match completion {
        DroppedCollectionWalkCompletion::Collected(collected) => collected,
        other => panic!("ожидался собранный результат, получено {other:?}"),
    }
}

fn music_folder() -> (tempfile::TempDir, PathBuf, Vec<PathBuf>) {
    let root = tempfile::tempdir().expect("dir");
    let folder = root.path().join("Музыка");
    let expected = vec![
        touch(&folder, "01 intro.mp3"),
        touch(&folder, "02 song.mp3"),
        touch(&folder, "live/03 bonus.flac"),
    ];
    touch(&folder, "notes.txt");
    (root, folder, expected)
}

#[test]
fn background_walk_runs_off_caller_and_busy_until_result_is_taken() {
    let (_root, folder, expected) = music_folder();
    let mut runtime = runtime_with_queue(&[]);

    runtime
        .start_dropped_collection_walk(
            vec![DroppedCollectionEntry::Folder(folder.clone())],
            DroppedCollectionDestination::ReplaceQueue,
        )
        .expect("walk starts");

    assert!(runtime.has_dropped_collection_walk_in_flight());
    assert_eq!(
        runtime.start_dropped_collection_walk(
            vec![DroppedCollectionEntry::Folder(folder)],
            DroppedCollectionDestination::AppendToQueue,
        ),
        Err(DroppedCollectionWalkStartError::AlreadyInFlight)
    );
    let collected = collected_from(wait_for_walk(&mut runtime));
    assert_eq!(collected.files, expected);
    assert!(!runtime.has_dropped_collection_walk_in_flight());
    assert!(
        runtime.take_dropped_collection_walk_completion().is_none(),
        "результат отдаётся ровно один раз"
    );
}

#[test]
fn folder_walk_then_confirmation_shows_count_and_changes_queue_only_after_confirm() {
    let (_root, folder, expected) = music_folder();
    let mut runtime = runtime_with_queue(&["/old/one.mkv", "/old/two.mkv"]);
    runtime
        .start_dropped_collection_walk(
            vec![DroppedCollectionEntry::Folder(folder)],
            DroppedCollectionDestination::ReplaceQueue,
        )
        .expect("walk starts");
    let collected = collected_from(wait_for_walk(&mut runtime));
    let source = collected.source.expect("folder source");

    let admission = runtime
        .admit_in_app_queue_replacement(
            InAppQueueReplacementIntent::local_files_from_dropped_collection(
                collected.files,
                &source,
                collected.truncation,
            ),
        )
        .expect("admission");

    assert!(matches!(
        admission,
        InAppQueueReplacementAdmission::AwaitingConfirmation
    ));
    assert_eq!(
        queue_paths(&runtime),
        vec![PathBuf::from("/old/one.mkv"), PathBuf::from("/old/two.mkv")],
        "очередь не меняется до Confirm"
    );
    let model = runtime
        .pending_playlist_confirmation()
        .expect("pending confirmation");
    assert_eq!(model.safe_label(), "3 файла из папки „Музыка“");

    let outcome = runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
        intent_id: model.intent_id(),
        decision: QueueReplacementConfirmationDecision::Confirm,
    });
    let PlaylistConfirmationApplyOutcome::QueueReplacementConfirmed(
        AdmittedQueueReplacementIntent::LocalFiles(files),
    ) = outcome
    else {
        panic!("Confirm должен вернуть набор файлов, получено {outcome:?}");
    };
    let LocalFilesReplacementOutcome::Replaced { first_item, .. } =
        runtime.replace_queue_with_local_files(files.into_paths())
    else {
        panic!("замена после Confirm должна пройти");
    };
    assert_eq!(queue_paths(&runtime), expected);
    assert!(matches!(
        runtime.play_playlist_row(first_item),
        RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::StartInstall { .. })
    ));
}

#[test]
fn cancelled_folder_confirmation_leaves_queue_untouched_and_starts_no_walk() {
    let (_root, folder, _expected) = music_folder();
    let mut runtime = runtime_with_queue(&["/old.mkv"]);
    runtime
        .start_dropped_collection_walk(
            vec![DroppedCollectionEntry::Folder(folder)],
            DroppedCollectionDestination::ReplaceQueue,
        )
        .expect("walk starts");
    let collected = collected_from(wait_for_walk(&mut runtime));
    let source = collected.source.expect("folder source");
    runtime
        .admit_in_app_queue_replacement(
            InAppQueueReplacementIntent::local_files_from_dropped_collection(
                collected.files,
                &source,
                collected.truncation,
            ),
        )
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
    assert_eq!(queue_paths(&runtime), vec![PathBuf::from("/old.mkv")]);
    assert!(!runtime.has_dropped_collection_walk_in_flight());
}

#[test]
fn shutdown_stops_the_walk_and_closes_admission() {
    let (_root, folder, _expected) = music_folder();
    let mut runtime = runtime_with_queue(&[]);
    runtime
        .start_dropped_collection_walk(
            vec![DroppedCollectionEntry::Folder(folder.clone())],
            DroppedCollectionDestination::ReplaceQueue,
        )
        .expect("walk starts");

    runtime.shutdown_until(crate::process_shutdown::ShutdownDeadline::after(
        Duration::from_secs(2),
    ));

    assert!(!runtime.has_dropped_collection_walk_in_flight());
    assert_eq!(
        runtime.start_dropped_collection_walk(
            vec![DroppedCollectionEntry::Folder(folder)],
            DroppedCollectionDestination::ReplaceQueue,
        ),
        Err(DroppedCollectionWalkStartError::RuntimeClosed)
    );
}

#[test]
fn limits_come_from_committed_playlist_config() {
    let wake = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let config = fastiplayer_config::PlaylistConfig {
        dropped_folder_max_files: 7,
        dropped_folder_max_depth: 3,
        ..fastiplayer_config::PlaylistConfig::default()
    };
    let runtime = PlaylistRuntime::new_with_config(wake, config);

    assert_eq!(runtime.dropped_collection_limits(), limits(7, 3));
}

/// Явные открытия, которые обязаны вытеснить идущий обход папки.
type ExplicitOpenBoundary = fn(&mut PlaylistRuntime);

fn explicit_open_boundaries() -> Vec<(&'static str, ExplicitOpenBoundary)> {
    vec![
        ("Row Play", |runtime| {
            runtime.supersede_queue_replacement_confirmation_for_row_play();
        }),
        ("главный Open / новый файл", |runtime| {
            let intent = InAppQueueReplacementIntent::local_files(vec![PathBuf::from("/new.mkv")]);
            runtime
                .admit_in_app_queue_replacement(intent)
                .expect("admission");
        }),
        ("структурная замена / Clear", |runtime| {
            runtime.cancel_queue_replacement_confirmation_for_structural_replacement();
        }),
    ]
}

#[test]
fn explicit_open_boundary_discards_in_flight_folder_walk_without_confirmation() {
    for (boundary_name, trigger_boundary) in explicit_open_boundaries() {
        let (_root, folder, _expected) = music_folder();
        let mut runtime = runtime_with_queue(&["/old.mkv"]);
        runtime
            .start_dropped_collection_walk(
                vec![DroppedCollectionEntry::Folder(folder)],
                DroppedCollectionDestination::ReplaceQueue,
            )
            .expect("walk starts");

        trigger_boundary(&mut runtime);

        // Результат устаревшего обхода — только `Cancelled`, даже если поток уже закончил.
        assert!(
            matches!(
                wait_for_walk(&mut runtime),
                DroppedCollectionWalkCompletion::Cancelled
            ),
            "{boundary_name}: устаревший обход должен быть отброшен"
        );
        assert!(!runtime.has_dropped_collection_walk_in_flight());
        assert_eq!(
            queue_paths(&runtime),
            vec![PathBuf::from("/old.mkv")],
            "{boundary_name}: очередь не тронута"
        );
    }
}

fn truncated_folder_intent() -> InAppQueueReplacementIntent {
    let source = DroppedCollectionSource {
        first_folder_name: "Музыка".to_owned(),
        other_item_count: 0,
    };
    InAppQueueReplacementIntent::local_files_from_dropped_collection(
        vec![PathBuf::from("/new/a.mp3"), PathBuf::from("/new/b.mp3")],
        &source,
        Some(DroppedCollectionTruncation::FileLimit),
    )
}

#[test]
fn truncation_reaches_commit_immediately_for_empty_queue() {
    let mut runtime = runtime_with_queue(&[]);

    let admission = runtime
        .admit_in_app_queue_replacement(truncated_folder_intent())
        .expect("admission");

    let InAppQueueReplacementAdmission::StartNow(AdmittedQueueReplacementIntent::LocalFiles(files)) =
        admission
    else {
        panic!("пустая очередь заменяется сразу, получено {admission:?}");
    };
    assert_eq!(
        files.truncation(),
        Some(DroppedCollectionTruncation::FileLimit)
    );
}

#[test]
fn truncation_is_delivered_only_with_confirm_and_never_after_cancel() {
    // Cancel: ни файлов, ни усечения наружу не уходит.
    let mut cancelled = runtime_with_queue(&["/old.mkv"]);
    cancelled
        .admit_in_app_queue_replacement(truncated_folder_intent())
        .expect("admission");
    let model = cancelled.pending_playlist_confirmation().expect("pending");
    let outcome = cancelled.respond_to_playlist_confirmation(PlaylistConfirmationAction {
        intent_id: model.intent_id(),
        decision: QueueReplacementConfirmationDecision::Cancel,
    });
    assert!(matches!(
        outcome,
        PlaylistConfirmationApplyOutcome::Cancelled
    ));

    // Confirm: усечение едет вместе с admitted-набором к точке commit.
    let mut confirmed = runtime_with_queue(&["/old.mkv"]);
    confirmed
        .admit_in_app_queue_replacement(truncated_folder_intent())
        .expect("admission");
    let model = confirmed.pending_playlist_confirmation().expect("pending");
    let outcome = confirmed.respond_to_playlist_confirmation(PlaylistConfirmationAction {
        intent_id: model.intent_id(),
        decision: QueueReplacementConfirmationDecision::Confirm,
    });
    let PlaylistConfirmationApplyOutcome::QueueReplacementConfirmed(
        AdmittedQueueReplacementIntent::LocalFiles(files),
    ) = outcome
    else {
        panic!("Confirm должен вернуть набор файлов, получено {outcome:?}");
    };
    assert_eq!(
        files.truncation(),
        Some(DroppedCollectionTruncation::FileLimit)
    );
}

#[test]
fn append_destination_walk_result_lands_in_queue_in_walk_order_not_natural_order() {
    // Файл `z.wav` брошен раньше папки с `a.wav`: порядок обхода z, a. Натуральная сортировка
    // поставила бы a перед z, то есть сломала бы порядок, который задал пользователь.
    let root = tempfile::tempdir().expect("dir");
    let wav_bytes = crate::media_open::local::tests::pcm_wav_bytes();
    let dropped_file = root.path().join("z.wav");
    fs::write(&dropped_file, &wav_bytes).expect("z.wav");
    let folder = root.path().join("folder");
    fs::create_dir(&folder).expect("folder");
    let folder_file = folder.join("a.wav");
    fs::write(&folder_file, &wav_bytes).expect("a.wav");
    let mut runtime = runtime_with_queue(&[]);

    runtime
        .start_dropped_collection_walk(
            vec![
                DroppedCollectionEntry::File(dropped_file.clone()),
                DroppedCollectionEntry::Folder(folder),
            ],
            DroppedCollectionDestination::AppendToQueue,
        )
        .expect("walk starts");
    let collected = collected_from(wait_for_walk(&mut runtime));
    assert_eq!(
        collected.files,
        vec![dropped_file.clone(), folder_file.clone()]
    );
    runtime
        .start_manual_file_add_in_given_order(collected.files)
        .expect("manual add starts");

    let deadline = Instant::now() + Duration::from_secs(5);
    while queue_paths(&runtime).len() < 2 {
        runtime.drain_playlist_discovery();
        assert!(Instant::now() < deadline, "manual add must append files");
        std::thread::yield_now();
    }
    assert_eq!(queue_paths(&runtime), vec![dropped_file, folder_file]);
}
