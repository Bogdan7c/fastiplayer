use std::path::PathBuf;
use std::time::{Duration, Instant};

use playlist_core::{CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::controller::PlaylistController;

fn runtime_with_queue_row(existing: &str) -> PlaylistRuntime {
    let wake = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
    let mut runtime = PlaylistRuntime::new(wake);
    runtime.controller.install(PlaylistController::new());
    runtime
        .controller
        .append(vec![PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from(existing)),
            None,
            CachedPlaylistMetadata::new(existing, PlaylistMediaKind::Video),
        )])
        .expect("existing row");
    runtime
}

/// Ждёт, пока фоновый разбор положит preview; зависание — точный провал теста.
fn wait_for_preview(runtime: &mut PlaylistRuntime) -> super::super::PlaylistImportPreview {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _changed = runtime.drain_playlist_import_job();
        if let Some(preview) = runtime.pending_playlist_import_preview() {
            return preview;
        }
        assert!(Instant::now() < deadline, "preview не появился за 5 секунд");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn write_playlist(directory: &tempfile::TempDir) -> PathBuf {
    let path = directory.path().join("dropped list.m3u");
    std::fs::write(
        &path,
        "#EXTM3U\n#EXTINF:1,Первый\none.mp3\n#EXTINF:2,Второй\ntwo.mp3\n",
    )
    .expect("playlist fixture");
    path
}

#[test]
fn dropped_m3u_path_stages_preview_with_its_entries_for_append_and_replace() {
    for intent in [
        PlaylistImportIntent::AppendToQueue,
        PlaylistImportIntent::ReplaceQueue,
    ] {
        let directory = tempfile::tempdir().expect("dir");
        let playlist = write_playlist(&directory);
        let mut runtime = runtime_with_queue_row("/old.mkv");

        let start = runtime.start_playlist_import_path(playlist, intent);

        assert_eq!(start, PlaylistPathImportStart::Started);
        assert!(runtime.has_playlist_import_in_flight());
        let preview = wait_for_preview(&mut runtime);
        assert_eq!(preview.intent(), intent);
        assert_eq!(preview.accepted().singles(), 2);
        // До явного подтверждения очередь не меняется.
        assert_eq!(
            runtime
                .controller
                .as_ref()
                .map(|controller| controller.queue().top_level_entry_count()),
            Some(1)
        );
    }
}

#[test]
fn second_path_import_while_one_is_running_is_reported_and_keeps_the_first() {
    let directory = tempfile::tempdir().expect("dir");
    let playlist = write_playlist(&directory);
    let mut runtime = runtime_with_queue_row("/old.mkv");
    assert_eq!(
        runtime.start_playlist_import_path(playlist.clone(), PlaylistImportIntent::AppendToQueue),
        PlaylistPathImportStart::Started
    );

    assert_eq!(
        runtime.start_playlist_import_path(playlist, PlaylistImportIntent::ReplaceQueue),
        PlaylistPathImportStart::ImportAlreadyRunning
    );

    // Первый импорт не сброшен вторым: его намерение осталось Append.
    let preview = wait_for_preview(&mut runtime);
    assert_eq!(preview.intent(), PlaylistImportIntent::AppendToQueue);
}

#[test]
fn closed_runtime_does_not_start_path_import() {
    let directory = tempfile::tempdir().expect("dir");
    let playlist = write_playlist(&directory);
    let mut runtime = runtime_with_queue_row("/old.mkv");
    runtime.shutdown_until(crate::process_shutdown::ShutdownDeadline::after(
        Duration::from_secs(2),
    ));

    assert_eq!(
        runtime.start_playlist_import_path(playlist, PlaylistImportIntent::AppendToQueue),
        PlaylistPathImportStart::RuntimeClosed
    );
    assert!(!runtime.has_playlist_import_in_flight());
}
