use std::path::PathBuf;
use std::time::{Duration, Instant};

use playlist_core::{CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::PlaylistImportNotice;
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
        assert!(
            Instant::now() < deadline,
            "preview didn't show up 5 seconds"
        );
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

/// Кодирует ASCII и русские буквы А-я в Windows-1251 (А = 0xC0 … я = 0xFF).
///
/// Намеренно не использует библиотеку декодирования, чтобы фикстура была независимой.
fn windows_1251_bytes(text: &str) -> Vec<u8> {
    text.chars()
        .map(|character| match character {
            '\u{0410}'..='\u{044F}' => {
                u8::try_from(0xC0 + (u32::from(character) - 0x0410)).expect("cp1251 byte")
            }
            ascii if ascii.is_ascii() => u8::try_from(u32::from(ascii)).expect("ascii byte"),
            other => panic!("{other:?} is not supported by a test encoder"),
        })
        .collect()
}

#[test]
fn windows_1251_m3u_imports_cyrillic_entries_and_reports_the_encoding_once() {
    let directory = tempfile::tempdir().expect("dir");
    let path = directory.path().join("старый.m3u");
    std::fs::write(
        &path,
        windows_1251_bytes("#EXTM3U\n#EXTINF:1,Первый\none.mp3\n#EXTINF:2,Второй\ntwo.mp3\n"),
    )
    .expect("cp1251 playlist");
    let mut runtime = runtime_with_queue_row("/old.mkv");

    let start = runtime.start_playlist_import_path(path, PlaylistImportIntent::AppendToQueue);

    assert_eq!(start, PlaylistPathImportStart::Started);
    let preview = wait_for_preview(&mut runtime);
    // Записи не потеряны: невалидный UTF-8 больше не отказ.
    assert_eq!(preview.accepted().singles(), 2);
    let notices = runtime.drain_playlist_import_notices();
    assert_eq!(notices, vec![PlaylistImportNotice::TextReadAsWindows1251]);
    assert_eq!(
        notices[0].message(),
        "Playlist read using Windows-1251 encoding"
    );
    // Уведомление отдаётся ровно один раз.
    assert!(runtime.drain_playlist_import_notices().is_empty());
}

#[test]
fn windows_1251_cue_imports_tracks_and_reports_the_encoding() {
    let directory = tempfile::tempdir().expect("dir");
    let path = directory.path().join("альбом.cue");
    std::fs::write(
        &path,
        windows_1251_bytes(
            "FILE \"альбом.flac\" FLAC\n  TRACK 01 AUDIO\n    TITLE \"Первая\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Вторая\"\n    INDEX 01 01:00:00\n",
        ),
    )
    .expect("cp1251 cue");
    let mut runtime = runtime_with_queue_row("/old.mkv");

    let start = runtime.start_playlist_import_path(path, PlaylistImportIntent::AppendToQueue);

    assert_eq!(start, PlaylistPathImportStart::Started);
    let preview = wait_for_preview(&mut runtime);
    assert_eq!(preview.accepted().singles(), 2);
    assert_eq!(
        runtime.drain_playlist_import_notices(),
        vec![PlaylistImportNotice::TextReadAsWindows1251]
    );
}

#[test]
fn utf8_playlist_import_shows_no_encoding_notice() {
    let directory = tempfile::tempdir().expect("dir");
    let playlist = write_playlist(&directory);
    let mut runtime = runtime_with_queue_row("/old.mkv");

    let _start = runtime.start_playlist_import_path(playlist, PlaylistImportIntent::AppendToQueue);
    let preview = wait_for_preview(&mut runtime);

    assert_eq!(preview.accepted().singles(), 2);
    assert!(runtime.drain_playlist_import_notices().is_empty());
}
