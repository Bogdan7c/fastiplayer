//! Сквозные тесты конвейера без GUI: события жеста → запрос → классификация → хост.
//!
//! Проверяют, что вся цепочка (URI со спецсимволами, hit-test панели, порядок файлов,
//! правило смеси) вместе даёт нужное обращение к существующим границам открытия.

use std::path::{Path, PathBuf};

use super::dispatch::{ExternalOpenDispatchOutcome, ExternalOpenHost};
use super::event::PhysicalDropPosition;
use super::uri::items_from_drop_payload;
use super::{DropGestureEvent, ExternalOpenOwner, dispatch_external_open_request};

/// Хост-запись обращений к границам открытия.
#[derive(Default)]
struct RecordingHost {
    calls: Vec<String>,
}

fn joined(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("|")
}

impl ExternalOpenHost for RecordingHost {
    fn open_in_progress(&self) -> bool {
        false
    }
    fn notify_open_still_in_progress(&mut self) {
        self.calls.push("busy".to_owned());
    }
    fn notify_info(&mut self, message: String) {
        self.calls.push(format!("info:{message}"));
    }
    fn open_single_media_file_like_open_button(&mut self, path: PathBuf) {
        self.calls.push(format!("open:{}", path.display()));
    }
    fn replace_queue_with_media_files(&mut self, paths: Vec<PathBuf>) {
        self.calls.push(format!("replace:{}", joined(&paths)));
    }
    fn append_media_files_to_queue(&mut self, paths: Vec<PathBuf>) {
        self.calls.push(format!("append:{}", joined(&paths)));
    }
    fn start_dropped_collection_walk(
        &mut self,
        entries: Vec<crate::playlist_runtime::DroppedCollectionEntry>,
        target: super::DropTarget,
    ) {
        self.calls
            .push(format!("walk:{target:?}:{}", entries.len()));
    }
    fn dropped_playlist_file_action(&self) -> fastiplayer_config::DroppedPlaylistFileAction {
        fastiplayer_config::DroppedPlaylistFileAction::ByDropTarget
    }
    fn import_playlist_file(
        &mut self,
        path: PathBuf,
        intent: crate::playlist_runtime::PlaylistImportIntent,
    ) {
        self.calls
            .push(format!("import:{intent:?}:{}", path.display()));
    }
    fn add_web_url_like_add_url_button(&mut self, url: super::request::DroppedWebUrl) {
        self.calls.push(format!("add-url:{}", url.as_str()));
    }
    fn replace_queue_with_web_url(&mut self, url: super::request::DroppedWebUrl) {
        self.calls.push(format!("replace-url:{}", url.as_str()));
    }
}

/// `file://`-URI настоящего пути (пробелы кодируются, как делает файловый менеджер).
fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display().to_string().replace(' ', "%20"))
}

/// Прогоняет жест `Entered → Dropped` через владельца и хост; возвращает обращения хоста.
fn drop_uris_at(uris: &[String], x: f64) -> Vec<String> {
    let mut owner = ExternalOpenOwner::default();
    // Панель плейлиста справа от 800 точек; масштаб окна 1.0.
    owner.record_playlist_panel_rect(Some(egui::Rect::from_min_max(
        egui::pos2(800.0, 0.0),
        egui::pos2(1200.0, 700.0),
    )));
    let position = PhysicalDropPosition { x, y: 100.0 };
    owner.apply_gesture_event(
        DropGestureEvent::Entered {
            position: Some(position),
        },
        1.0,
    );
    let request = owner
        .apply_gesture_event(
            DropGestureEvent::Dropped {
                position: Some(position),
                items: items_from_drop_payload(uris, None),
            },
            1.0,
        )
        .expect("drop yields one request");
    let mut host = RecordingHost::default();
    assert_eq!(
        dispatch_external_open_request(&mut host, request),
        ExternalOpenDispatchOutcome::Dispatched
    );
    host.calls
}

#[test]
fn three_files_with_spaces_dropped_on_video_replace_queue_in_drop_order() {
    let directory = tempfile::tempdir().expect("dir");
    let paths: Vec<PathBuf> = ["third part.mkv", "first.mkv", "second one.mkv"]
        .iter()
        .map(|name| directory.path().join(name))
        .collect();
    for path in &paths {
        std::fs::write(path, b"x").expect("fixture");
    }
    let uris: Vec<String> = paths.iter().map(|path| file_uri(path)).collect();

    let calls = drop_uris_at(&uris, 300.0);

    assert_eq!(calls, vec![format!("replace:{}", joined(&paths))]);
}

#[test]
fn same_files_dropped_on_playlist_panel_are_appended() {
    let directory = tempfile::tempdir().expect("dir");
    let path = directory.path().join("clip one.mkv");
    std::fs::write(&path, b"x").expect("fixture");

    let calls = drop_uris_at(&[file_uri(&path)], 1000.0);

    assert_eq!(calls, vec![format!("append:{}", path.display())]);
}

#[test]
fn single_file_on_video_opens_like_open_button_and_playlist_in_mix_is_skipped() {
    let directory = tempfile::tempdir().expect("dir");
    let media = directory.path().join("movie.mkv");
    let playlist = directory.path().join("list.m3u");
    std::fs::write(&media, b"x").expect("media");
    std::fs::write(&playlist, b"#EXTM3U").expect("playlist");

    let calls = drop_uris_at(&[file_uri(&playlist), file_uri(&media)], 300.0);

    assert_eq!(
        calls,
        vec![
            format!("open:{}", media.display()),
            "info:Плейлисты перетаскивайте отдельно".to_owned(),
        ]
    );
}

#[test]
fn folder_dropped_on_video_reaches_the_walk_boundary_with_video_target() {
    let directory = tempfile::tempdir().expect("dir");
    let folder = directory.path().join("Моя музыка");
    std::fs::create_dir(&folder).expect("folder");

    let calls = drop_uris_at(&[file_uri(&folder)], 300.0);

    assert_eq!(calls, vec!["walk:Video:1".to_owned()]);
}

#[test]
fn playlist_file_dropped_on_panel_is_imported_as_append() {
    let directory = tempfile::tempdir().expect("dir");
    let playlist = directory.path().join("my list.m3u");
    std::fs::write(&playlist, b"#EXTM3U").expect("playlist");

    let calls = drop_uris_at(&[file_uri(&playlist)], 1000.0);

    assert_eq!(
        calls,
        vec![format!("import:AppendToQueue:{}", playlist.display())]
    );
}

#[test]
fn url_dropped_on_panel_and_on_video_reaches_the_right_boundary() {
    let url = "https://example.org/watch?v=1&t=2".to_owned();
    assert_eq!(
        drop_uris_at(std::slice::from_ref(&url), 1000.0),
        vec![format!("add-url:{url}")]
    );
    assert_eq!(
        drop_uris_at(std::slice::from_ref(&url), 300.0),
        vec![format!("replace-url:{url}")]
    );
}

#[test]
fn text_only_drop_with_web_link_still_becomes_a_url_request() {
    // Браузеры без uri-list отдают только текст: он тоже превращается в ссылку.
    let items = items_from_drop_payload(&[], Some("заголовок\nhttps://example.org/page"));
    let mut owner = ExternalOpenOwner::default();
    let request = owner
        .apply_gesture_event(
            DropGestureEvent::Dropped {
                position: None,
                items,
            },
            1.0,
        )
        .expect("drop yields one request");
    let mut host = RecordingHost::default();
    dispatch_external_open_request(&mut host, request);
    assert_eq!(host.calls, vec!["replace-url:https://example.org/page"]);
}
