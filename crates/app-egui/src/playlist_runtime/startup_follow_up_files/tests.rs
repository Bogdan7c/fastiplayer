//! Сквозные проверки набора CLI-файлов на настоящем `PlaylistRuntime`: тот же протокол
//! установки, что у одного CLI-файла (accept → Ready → dispatch → Installed), затем
//! добавление остальных файлов.

use std::num::NonZeroU64;
use std::path::PathBuf;

use player_core::{MediaInstallRequestId, MediaInstanceId, PlaybackIntentRevision};
use playlist_core::{CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::media_open::{
    AuthorizationDispatchResolution, MediaOpenRequestId, PlayerDispatchRejection,
};
use crate::playlist_runtime::PlaylistBindingGeneration;
use crate::playlist_runtime::controller::InstallReadyOutcome;

const REQUEST_ID: u64 = 71;
const PLAYER_REQUEST_ID: u64 = 81;

fn draft(path: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(path)),
        None,
        CachedPlaylistMetadata::new(path.to_owned(), PlaylistMediaKind::Video),
    )
}

fn request_id() -> MediaOpenRequestId {
    MediaOpenRequestId::from_non_zero(NonZeroU64::new(REQUEST_ID).expect("request id"))
}

fn player_request_id() -> MediaInstallRequestId {
    MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(PLAYER_REQUEST_ID).expect("player request id"),
    )
}

fn paths(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

/// Нативные пути очереди в её порядке.
fn queue_paths(runtime: &PlaylistRuntime) -> Vec<PathBuf> {
    runtime
        .playlist_controller()
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

/// Путь строки, которая сейчас играет.
fn active_path(runtime: &PlaylistRuntime) -> Option<PathBuf> {
    let controller = runtime.playlist_controller()?;
    let item_id = controller.active_media()?.item_id()?;
    controller
        .queue()
        .item(item_id)?
        .locator()
        .as_local()
        .and_then(LocalLocator::expose_native_path_for_open)
        .map(std::path::Path::to_path_buf)
}

/// Runtime с восстановленной очередью и принятой startup-установкой первого CLI-файла.
fn runtime_with_first_cli_file_awaiting_install(first: &str) -> PlaylistRuntime {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.resolve_missing_state_for_test();
    runtime
        .controller
        .as_mut()
        .expect("controller")
        .append(vec![draft("/restored/one.mkv"), draft("/restored/two.mkv")])
        .expect("seed restored queue");
    runtime
        .accept_explicit_target_install(
            request_id(),
            player_request_id(),
            draft(first),
            PlaybackIntentRevision::from_non_zero(NonZeroU64::MIN),
        )
        .expect("accept first CLI file");
    runtime.begin_startup_action_retention();
    let controller = runtime.controller.as_mut().expect("controller");
    assert!(matches!(
        controller.on_ready_to_commit(request_id()),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));
    controller
        .begin_authorization_dispatch(request_id())
        .expect("begin dispatch");
    runtime
}

/// Плеер принял первый файл и установил его (точный `Installed`).
fn install_first_cli_file(runtime: &mut PlaylistRuntime) {
    let controller = runtime.controller.as_mut().expect("controller");
    controller
        .resolve_authorization_dispatch(
            request_id(),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .expect("enqueue wins");
    controller
        .on_installed(
            request_id(),
            player_request_id(),
            MediaInstanceId::from_non_zero(NonZeroU64::new(91).expect("instance id")),
            PlaylistBindingGeneration(1),
        )
        .expect("first CLI file installed");
}

#[test]
fn two_files_become_the_queue_in_given_order_and_first_is_playing() {
    let mut runtime = runtime_with_first_cli_file_awaiting_install("/cli/b-first.mkv");
    // До барьера восстановленная очередь остаётся запасной и не тронута.
    assert_eq!(
        queue_paths(&runtime),
        paths(&["/restored/one.mkv", "/restored/two.mkv"])
    );

    install_first_cli_file(&mut runtime);
    let dirty_before = runtime
        .playlist_controller()
        .expect("controller")
        .dirty_revision();
    let outcome = runtime.append_local_files_after_startup_target(paths(&["/cli/a-second.mkv"]));

    assert_eq!(
        outcome,
        StartupFollowUpFilesOutcome::Appended { file_count: 1 }
    );
    // Порядок — как в командной строке, а не алфавитный; соседей не добавлено.
    assert_eq!(
        queue_paths(&runtime),
        paths(&["/cli/b-first.mkv", "/cli/a-second.mkv"])
    );
    assert_eq!(
        active_path(&runtime),
        Some(PathBuf::from("/cli/b-first.mkv"))
    );
    // Изменение очереди уходит в persistence (новый dirty receipt).
    assert_ne!(
        runtime
            .playlist_controller()
            .expect("controller")
            .dirty_revision(),
        dirty_before
    );
}

#[test]
fn first_file_rejected_before_barrier_keeps_restored_queue_as_fallback() {
    let mut runtime = runtime_with_first_cli_file_awaiting_install("/cli/broken.mkv");
    runtime
        .controller
        .as_mut()
        .expect("controller")
        .resolve_authorization_dispatch(
            request_id(),
            AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue {
                rejection: PlayerDispatchRejection::Backpressure,
            },
        )
        .expect("player rejects first file before enqueue");

    // Startup owner в этом случае хвост не добавляет; но даже вызов не меняет очередь.
    let outcome = runtime.append_local_files_after_startup_target(paths(&["/cli/second.mkv"]));

    assert_eq!(outcome, StartupFollowUpFilesOutcome::MissingInstalledTarget);
    assert_eq!(
        queue_paths(&runtime),
        paths(&["/restored/one.mkv", "/restored/two.mkv"])
    );
}

#[test]
fn append_during_pending_install_is_refused_without_touching_queue() {
    let mut runtime = runtime_with_first_cli_file_awaiting_install("/cli/first.mkv");
    // Restored queue без играющего элемента: нет закреплённого первого файла.
    let outcome = runtime.append_local_files_after_startup_target(paths(&["/cli/second.mkv"]));

    assert_eq!(outcome, StartupFollowUpFilesOutcome::MissingInstalledTarget);
    assert_eq!(
        queue_paths(&runtime),
        paths(&["/restored/one.mkv", "/restored/two.mkv"])
    );
}

#[test]
fn absent_controller_and_empty_list_are_distinct_no_ops() {
    let mut pending =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    assert_eq!(
        pending.append_local_files_after_startup_target(paths(&["/cli/a.mkv"])),
        StartupFollowUpFilesOutcome::LoadDecisionPending
    );

    let mut runtime = runtime_with_first_cli_file_awaiting_install("/cli/first.mkv");
    install_first_cli_file(&mut runtime);
    let dirty_before = runtime
        .playlist_controller()
        .expect("controller")
        .dirty_revision();
    assert_eq!(
        runtime.append_local_files_after_startup_target(Vec::new()),
        StartupFollowUpFilesOutcome::NoFilesProvided
    );
    assert_eq!(queue_paths(&runtime), paths(&["/cli/first.mkv"]));
    assert_eq!(
        runtime
            .playlist_controller()
            .expect("controller")
            .dirty_revision(),
        dirty_before
    );
}

#[test]
fn retained_clear_made_during_install_still_wins_over_appended_files() {
    let mut runtime = runtime_with_first_cli_file_awaiting_install("/cli/first.mkv");
    assert_eq!(
        runtime.clear_playlist(std::time::Instant::now()),
        crate::playlist_runtime::RuntimeRemovalOutcome::DeferredUntilStartupInstallResolution
    );
    install_first_cli_file(&mut runtime);

    // Порядок startup owner-а: сначала хвост набора, затем retained-действия пользователя.
    assert_eq!(
        runtime.append_local_files_after_startup_target(paths(&["/cli/second.mkv"])),
        StartupFollowUpFilesOutcome::Appended { file_count: 1 }
    );
    assert_eq!(
        runtime
            .apply_retained_startup_actions()
            .expect("apply retained clear"),
        crate::playlist_runtime::RetainedStartupApplyOutcome::Cleared
    );
    assert!(queue_paths(&runtime).is_empty());
}
