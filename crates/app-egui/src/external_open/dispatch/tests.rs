use super::*;
use crate::external_open::request::{DropTarget, DroppedWebUrl, ExternalOpenItem};
use fastiplayer_config::DroppedPlaylistFileAction;

/// Хост-запись: хранит все обращения в порядке вызова.
struct RecordingHost {
    busy: bool,
    playlist_action: DroppedPlaylistFileAction,
    calls: Vec<String>,
}

impl Default for RecordingHost {
    fn default() -> Self {
        Self {
            busy: false,
            playlist_action: DroppedPlaylistFileAction::ByDropTarget,
            calls: Vec::new(),
        }
    }
}

/// «F:путь» для файла и «D:путь» для папки: порядок и вид элементов видны в проверке.
fn describe_entries(entries: &[DroppedCollectionEntry]) -> String {
    entries
        .iter()
        .map(|entry| match entry {
            DroppedCollectionEntry::File(path) => format!("F:{}", path.display()),
            DroppedCollectionEntry::Folder(path) => format!("D:{}", path.display()),
        })
        .collect::<Vec<_>>()
        .join(",")
}

impl ExternalOpenHost for RecordingHost {
    fn open_in_progress(&self) -> bool {
        self.busy
    }
    fn notify_open_still_in_progress(&mut self) {
        self.calls.push("busy-notice".to_owned());
    }
    fn notify_info(&mut self, message: String) {
        self.calls.push(format!("info:{message}"));
    }
    fn open_single_media_file_like_open_button(&mut self, path: PathBuf) {
        self.calls.push(format!("open:{}", path.display()));
    }
    fn replace_queue_with_media_files(&mut self, paths: Vec<PathBuf>) {
        let names: Vec<String> = paths
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        self.calls.push(format!("replace:{}", names.join(",")));
    }
    fn append_media_files_to_queue(&mut self, paths: Vec<PathBuf>) {
        let names: Vec<String> = paths
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        self.calls.push(format!("append:{}", names.join(",")));
    }
    fn start_dropped_collection_walk(
        &mut self,
        entries: Vec<DroppedCollectionEntry>,
        target: DropTarget,
    ) {
        self.calls
            .push(format!("walk:{target:?}:{}", describe_entries(&entries)));
    }
    fn dropped_playlist_file_action(&self) -> DroppedPlaylistFileAction {
        self.playlist_action
    }
    fn import_playlist_file(&mut self, path: PathBuf, intent: PlaylistImportIntent) {
        self.calls
            .push(format!("import:{intent:?}:{}", path.display()));
    }
    fn add_web_url_like_add_url_button(&mut self, url: DroppedWebUrl) {
        self.calls.push(format!("add-url:{}", url.as_str()));
    }
    fn replace_queue_with_web_url(&mut self, url: DroppedWebUrl) {
        self.calls.push(format!("replace-url:{}", url.as_str()));
    }
}

fn request_with_files(target: DropTarget, files: &[&std::path::Path]) -> ExternalOpenRequest {
    ExternalOpenRequest {
        target,
        items: files
            .iter()
            .map(|path| ExternalOpenItem::LocalPath(path.to_path_buf()))
            .collect(),
    }
}

#[test]
fn busy_host_gets_only_the_busy_notice_and_nothing_else() {
    let dir = tempfile::tempdir().expect("dir");
    let file = dir.path().join("a.mkv");
    std::fs::write(&file, b"x").expect("file");
    let mut host = RecordingHost {
        busy: true,
        ..RecordingHost::default()
    };

    let outcome =
        dispatch_external_open_request(&mut host, request_with_files(DropTarget::Video, &[&file]));

    assert_eq!(outcome, ExternalOpenDispatchOutcome::IgnoredBecauseBusy);
    assert_eq!(host.calls, vec!["busy-notice"]);
}

#[test]
fn files_reach_the_host_in_drop_order_through_the_right_boundary() {
    let dir = tempfile::tempdir().expect("dir");
    let first = dir.path().join("b.mkv");
    let second = dir.path().join("a.mkv");
    std::fs::write(&first, b"x").expect("file");
    std::fs::write(&second, b"x").expect("file");

    let mut host = RecordingHost::default();
    dispatch_external_open_request(
        &mut host,
        request_with_files(DropTarget::Video, &[&first, &second]),
    );
    dispatch_external_open_request(
        &mut host,
        request_with_files(DropTarget::Playlist, &[&second, &first]),
    );
    dispatch_external_open_request(&mut host, request_with_files(DropTarget::Video, &[&first]));

    let display = |path: &std::path::Path| path.display().to_string();
    assert_eq!(
        host.calls,
        vec![
            format!("replace:{},{}", display(&first), display(&second)),
            format!("append:{},{}", display(&second), display(&first)),
            format!("open:{}", display(&first)),
        ]
    );
}

#[test]
fn folder_on_video_starts_a_walk_and_touches_neither_queue_nor_open_paths() {
    let dir = tempfile::tempdir().expect("dir");
    let mut host = RecordingHost::default();

    let outcome = dispatch_external_open_request(
        &mut host,
        request_with_files(DropTarget::Video, &[dir.path()]),
    );

    assert_eq!(outcome, ExternalOpenDispatchOutcome::Dispatched);
    // Только запуск обхода: замена очереди и подтверждение случатся ПОСЛЕ результата обхода.
    assert_eq!(
        host.calls,
        vec![format!("walk:Video:D:{}", dir.path().display())]
    );
}

#[test]
fn folder_on_panel_starts_a_walk_with_playlist_target() {
    let dir = tempfile::tempdir().expect("dir");
    let mut host = RecordingHost::default();
    dispatch_external_open_request(
        &mut host,
        request_with_files(DropTarget::Playlist, &[dir.path()]),
    );
    assert_eq!(
        host.calls,
        vec![format!("walk:Playlist:D:{}", dir.path().display())]
    );
}

#[test]
fn files_and_folders_form_one_collection_in_drop_order() {
    let dir = tempfile::tempdir().expect("dir");
    let first_file = dir.path().join("b.mkv");
    let second_file = dir.path().join("a.mkv");
    let folder = dir.path().join("album");
    std::fs::write(&first_file, b"x").expect("file");
    std::fs::write(&second_file, b"x").expect("file");
    std::fs::create_dir(&folder).expect("folder");
    let mut host = RecordingHost::default();

    dispatch_external_open_request(
        &mut host,
        request_with_files(DropTarget::Video, &[&first_file, &folder, &second_file]),
    );

    assert_eq!(
        host.calls,
        vec![format!(
            "walk:Video:F:{},D:{},F:{}",
            first_file.display(),
            folder.display(),
            second_file.display()
        )]
    );
}

#[test]
fn walk_in_flight_makes_new_drop_busy_and_starts_nothing() {
    let dir = tempfile::tempdir().expect("dir");
    let mut host = RecordingHost {
        busy: true,
        ..RecordingHost::default()
    };
    let outcome = dispatch_external_open_request(
        &mut host,
        request_with_files(DropTarget::Video, &[dir.path()]),
    );
    assert_eq!(outcome, ExternalOpenDispatchOutcome::IgnoredBecauseBusy);
    assert_eq!(host.calls, vec!["busy-notice"]);
}

#[test]
fn dropped_playlist_intent_follows_setting_and_drop_target() {
    let dir = tempfile::tempdir().expect("dir");
    let playlist = dir.path().join("list.m3u");
    std::fs::write(&playlist, b"#EXTM3U").expect("playlist");
    let cases = [
        (
            DroppedPlaylistFileAction::ByDropTarget,
            DropTarget::Playlist,
            "AppendToQueue",
        ),
        (
            DroppedPlaylistFileAction::ByDropTarget,
            DropTarget::Video,
            "ReplaceQueue",
        ),
        (
            DroppedPlaylistFileAction::NewPlaylist,
            DropTarget::Playlist,
            "ReplaceQueue",
        ),
        (
            DroppedPlaylistFileAction::AppendToQueue,
            DropTarget::Video,
            "AppendToQueue",
        ),
    ];
    for (action, target, expected_intent) in cases {
        let mut host = RecordingHost {
            playlist_action: action,
            ..RecordingHost::default()
        };
        dispatch_external_open_request(&mut host, request_with_files(target, &[&playlist]));
        assert_eq!(
            host.calls,
            vec![format!("import:{expected_intent}:{}", playlist.display())],
            "{action:?} на {target:?}"
        );
    }
}

#[test]
fn several_playlists_import_only_the_first_with_notice() {
    let dir = tempfile::tempdir().expect("dir");
    let first = dir.path().join("one.m3u");
    let second = dir.path().join("two.xspf");
    std::fs::write(&first, b"#EXTM3U").expect("playlist");
    std::fs::write(&second, b"<x/>").expect("playlist");
    let mut host = RecordingHost::default();

    dispatch_external_open_request(
        &mut host,
        request_with_files(DropTarget::Playlist, &[&first, &second]),
    );

    assert_eq!(
        host.calls,
        vec![
            format!("import:AppendToQueue:{}", first.display()),
            "info:Плейлисты импортируются по одному".to_owned(),
        ]
    );
}

fn request_with_urls(target: DropTarget, urls: &[&str]) -> ExternalOpenRequest {
    ExternalOpenRequest {
        target,
        items: urls
            .iter()
            .map(|text| ExternalOpenItem::WebUrl(DroppedWebUrl::new((*text).to_owned())))
            .collect(),
    }
}

#[test]
fn url_on_panel_goes_to_add_url_and_on_video_to_queue_replacement() {
    let mut host = RecordingHost::default();
    dispatch_external_open_request(
        &mut host,
        request_with_urls(DropTarget::Playlist, &["https://example.org/a?x=1"]),
    );
    dispatch_external_open_request(
        &mut host,
        request_with_urls(DropTarget::Video, &["https://example.org/b"]),
    );
    assert_eq!(
        host.calls,
        vec![
            "add-url:https://example.org/a?x=1",
            "replace-url:https://example.org/b",
        ]
    );
}

#[test]
fn several_urls_use_only_the_first_and_notify() {
    let mut host = RecordingHost::default();
    dispatch_external_open_request(
        &mut host,
        request_with_urls(
            DropTarget::Playlist,
            &["https://one.test", "https://two.test"],
        ),
    );
    assert_eq!(
        host.calls,
        vec![
            "add-url:https://one.test",
            "info:Ссылки добавляются по одной"
        ]
    );
}

#[test]
fn url_mixed_with_a_file_is_skipped_while_the_file_is_handled() {
    let dir = tempfile::tempdir().expect("dir");
    let file = dir.path().join("a.mkv");
    std::fs::write(&file, b"x").expect("file");
    let mut host = RecordingHost::default();
    dispatch_external_open_request(
        &mut host,
        ExternalOpenRequest {
            target: DropTarget::Video,
            items: vec![
                ExternalOpenItem::WebUrl(DroppedWebUrl::new("https://example.org".to_owned())),
                ExternalOpenItem::LocalPath(file.clone()),
            ],
        },
    );
    assert_eq!(
        host.calls,
        vec![
            format!("open:{}", file.display()),
            "info:Ссылки перетаскивайте отдельно".to_owned(),
        ]
    );
}

#[test]
fn busy_host_rejects_a_url_drop_without_starting_anything() {
    let mut host = RecordingHost {
        busy: true,
        ..RecordingHost::default()
    };
    let outcome = dispatch_external_open_request(
        &mut host,
        request_with_urls(DropTarget::Video, &["https://example.org"]),
    );
    assert_eq!(outcome, ExternalOpenDispatchOutcome::IgnoredBecauseBusy);
    assert_eq!(host.calls, vec!["busy-notice"]);
}

#[test]
fn empty_drop_tells_the_user_there_is_nothing_to_open() {
    let mut host = RecordingHost::default();
    dispatch_external_open_request(&mut host, request_with_files(DropTarget::Video, &[]));
    assert_eq!(host.calls, vec!["info:Здесь нечего открывать"]);
}
