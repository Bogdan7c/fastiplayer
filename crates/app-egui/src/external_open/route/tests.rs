use super::*;
use crate::external_open::request::DroppedWebUrl;
use crate::playlist_runtime::DroppedCollectionEntry;

fn plan(target: DropTarget, items: Vec<ClassifiedItem>, skipped: usize) -> ExternalOpenPlan {
    ExternalOpenPlan {
        target,
        items,
        skipped_playlist_files: skipped,
    }
}

fn media(path: &str) -> ClassifiedItem {
    ClassifiedItem::MediaFile(PathBuf::from(path))
}

#[test]
fn one_file_on_video_opens_like_open_button() {
    let steps = route_plan(plan(DropTarget::Video, vec![media("/a.mkv")], 0));
    assert_eq!(
        steps,
        vec![OpenStep::OpenSingleMediaFile(PathBuf::from("/a.mkv"))]
    );
}

#[test]
fn several_files_on_video_replace_queue_in_drop_order() {
    let steps = route_plan(plan(
        DropTarget::Video,
        vec![media("/c.mkv"), media("/a.mkv"), media("/b.mkv")],
        0,
    ));
    assert_eq!(
        steps,
        vec![OpenStep::ReplaceQueueWithMediaFiles(vec![
            PathBuf::from("/c.mkv"),
            PathBuf::from("/a.mkv"),
            PathBuf::from("/b.mkv"),
        ])]
    );
}

#[test]
fn files_on_playlist_are_appended_even_when_single() {
    let one = route_plan(plan(DropTarget::Playlist, vec![media("/a.mkv")], 0));
    assert_eq!(
        one,
        vec![OpenStep::AppendMediaFiles(vec![PathBuf::from("/a.mkv")])]
    );
    let two = route_plan(plan(
        DropTarget::Playlist,
        vec![media("/a.mkv"), media("/b.mkv")],
        0,
    ));
    assert_eq!(
        two,
        vec![OpenStep::AppendMediaFiles(vec![
            PathBuf::from("/a.mkv"),
            PathBuf::from("/b.mkv")
        ])]
    );
}

#[test]
fn folders_and_files_become_one_ordered_collection_step() {
    let steps = route_plan(plan(
        DropTarget::Video,
        vec![
            media("/c.mkv"),
            ClassifiedItem::Directory(PathBuf::from("/d")),
            media("/a.mkv"),
        ],
        0,
    ));
    assert_eq!(
        steps,
        vec![OpenStep::CollectDroppedItems {
            target: DropTarget::Video,
            entries: vec![
                DroppedCollectionEntry::File(PathBuf::from("/c.mkv")),
                DroppedCollectionEntry::Folder(PathBuf::from("/d")),
                DroppedCollectionEntry::File(PathBuf::from("/a.mkv")),
            ],
        }]
    );
}

#[test]
fn playlist_file_becomes_import_step_and_extra_playlists_get_a_notice() {
    let steps = route_plan(plan(
        DropTarget::Playlist,
        vec![
            ClassifiedItem::PlaylistFile(PathBuf::from("/p.m3u")),
            ClassifiedItem::PlaylistFile(PathBuf::from("/q.xspf")),
        ],
        0,
    ));
    assert_eq!(
        steps,
        vec![
            OpenStep::ImportPlaylistFile {
                path: PathBuf::from("/p.m3u"),
                target: DropTarget::Playlist,
            },
            OpenStep::Notify(DropNotice::PlaylistsOneAtATime),
        ]
    );
}

fn url(text: &str) -> ClassifiedItem {
    ClassifiedItem::WebUrl(DroppedWebUrl::new(text.to_owned()))
}

fn open_url_step(text: &str, target: DropTarget) -> OpenStep {
    OpenStep::OpenWebUrl {
        url: DroppedWebUrl::new(text.to_owned()),
        target,
    }
}

#[test]
fn one_url_gets_a_step_with_the_exact_url_and_drop_target() {
    for target in [DropTarget::Playlist, DropTarget::Video] {
        let steps = route_plan(plan(target, vec![url("https://example.org/v?t=1")], 0));
        assert_eq!(
            steps,
            vec![open_url_step("https://example.org/v?t=1", target)]
        );
    }
}

#[test]
fn several_urls_take_the_first_and_tell_about_one_at_a_time() {
    let steps = route_plan(plan(
        DropTarget::Playlist,
        vec![url("https://example.org"), url("https://example.net")],
        0,
    ));
    assert_eq!(
        steps,
        vec![
            open_url_step("https://example.org", DropTarget::Playlist),
            OpenStep::Notify(DropNotice::LinksOneAtATime),
        ]
    );
}

#[test]
fn urls_mixed_with_local_items_are_skipped_with_notice() {
    let steps = route_plan(plan(
        DropTarget::Video,
        vec![url("https://example.org"), media("/a.mkv")],
        0,
    ));
    assert_eq!(
        steps,
        vec![
            OpenStep::OpenSingleMediaFile(PathBuf::from("/a.mkv")),
            OpenStep::Notify(DropNotice::LinksSeparately),
        ]
    );
}

#[test]
fn url_next_to_only_missing_files_is_still_opened() {
    let steps = route_plan(plan(
        DropTarget::Video,
        vec![
            ClassifiedItem::Missing(PathBuf::from("/gone")),
            url("https://example.org"),
        ],
        0,
    ));
    assert_eq!(
        steps,
        vec![
            open_url_step("https://example.org", DropTarget::Video),
            OpenStep::Notify(DropNotice::FilesNotFound { count: 1 }),
        ]
    );
}

#[test]
fn media_is_processed_and_playlists_skipped_with_notice() {
    let steps = route_plan(plan(DropTarget::Video, vec![media("/a.mkv")], 2));
    assert_eq!(
        steps,
        vec![
            OpenStep::OpenSingleMediaFile(PathBuf::from("/a.mkv")),
            OpenStep::Notify(DropNotice::PlaylistsSeparately),
        ]
    );
}

#[test]
fn missing_and_unsupported_items_produce_notices_next_to_media_action() {
    let steps = route_plan(plan(
        DropTarget::Playlist,
        vec![
            ClassifiedItem::Missing(PathBuf::from("/gone")),
            media("/a.mkv"),
            ClassifiedItem::Missing(PathBuf::from("/gone2")),
            ClassifiedItem::Unsupported {
                scheme: "ftp".to_owned(),
            },
        ],
        0,
    ));
    assert_eq!(
        steps,
        vec![
            OpenStep::AppendMediaFiles(vec![PathBuf::from("/a.mkv")]),
            OpenStep::Notify(DropNotice::FilesNotFound { count: 2 }),
            OpenStep::Notify(DropNotice::UnsupportedLink),
        ]
    );
}

#[test]
fn empty_plan_says_there_is_nothing_to_open() {
    assert_eq!(
        route_plan(plan(DropTarget::Video, Vec::new(), 0)),
        vec![OpenStep::Notify(DropNotice::NothingToOpen)]
    );
}

#[test]
fn notice_texts_are_human_english_without_debug_noise() {
    assert_eq!(
        DropNotice::FilesNotFound { count: 1 }.text(),
        "File not found"
    );
    assert_eq!(
        DropNotice::FilesNotFound { count: 3 }.text(),
        "Files not found: 3"
    );
    assert_eq!(
        DropNotice::PlaylistsSeparately.text(),
        "Drag Playlists separately"
    );
    assert_eq!(
        DropNotice::LinksOneAtATime.text(),
        "Add links one at a time"
    );
    assert_eq!(DropNotice::LinksSeparately.text(), "Drag links separately");
    assert_eq!(
        DropNotice::FolderHasNoMedia.text(),
        "The folder contains no media files"
    );
    assert_eq!(
        DropNotice::FolderFileLimitReached { taken: 2000 }.text(),
        "Added the first 2000 files from the folder"
    );
    assert_eq!(
        DropNotice::PlaylistsOneAtATime.text(),
        "Import Playlists one at a time"
    );
}

fn subtitle(path: &str) -> ClassifiedItem {
    ClassifiedItem::SubtitleFile(PathBuf::from(path))
}

fn paths(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

#[test]
fn lone_subtitles_attach_to_current_media_on_either_target() {
    for target in [DropTarget::Video, DropTarget::Playlist] {
        let steps = route_plan(plan(
            target,
            vec![subtitle("/a.srt"), subtitle("/b.vtt")],
            0,
        ));
        assert_eq!(
            steps,
            vec![OpenStep::AttachSubtitlesToCurrentMedia(paths(&[
                "/a.srt", "/b.vtt"
            ]))]
        );
    }
}

#[test]
fn video_with_subtitle_on_video_queues_subtitles_before_opening() {
    let steps = route_plan(plan(
        DropTarget::Video,
        vec![subtitle("/a.srt"), media("/m.mkv")],
        0,
    ));
    assert_eq!(
        steps,
        vec![
            OpenStep::QueueSubtitlesForOpenedMedia(paths(&["/a.srt"])),
            OpenStep::OpenSingleMediaFile(PathBuf::from("/m.mkv")),
        ]
    );
}

#[test]
fn several_videos_with_subtitle_replace_queue_and_queue_subtitles_first() {
    let steps = route_plan(plan(
        DropTarget::Video,
        vec![media("/1.mkv"), media("/2.mkv"), subtitle("/a.srt")],
        0,
    ));
    assert_eq!(
        steps[0],
        OpenStep::QueueSubtitlesForOpenedMedia(paths(&["/a.srt"]))
    );
    assert_eq!(
        steps[1],
        OpenStep::ReplaceQueueWithMediaFiles(paths(&["/1.mkv", "/2.mkv"]))
    );
}

#[test]
fn folder_with_subtitle_on_video_keeps_the_walk_and_queues_subtitles() {
    let steps = route_plan(plan(
        DropTarget::Video,
        vec![
            ClassifiedItem::Directory(PathBuf::from("/d")),
            subtitle("/a.srt"),
        ],
        0,
    ));
    assert_eq!(
        steps[0],
        OpenStep::QueueSubtitlesForOpenedMedia(paths(&["/a.srt"]))
    );
    assert!(matches!(steps[1], OpenStep::CollectDroppedItems { .. }));
    assert_eq!(steps.len(), 2);
}

#[test]
fn playlist_target_mixed_drop_appends_media_and_never_attaches_subtitles() {
    let steps = route_plan(plan(
        DropTarget::Playlist,
        vec![media("/m.mkv"), subtitle("/a.srt")],
        0,
    ));
    assert_eq!(
        steps,
        vec![
            OpenStep::Notify(DropNotice::SubtitlesNeedPlayingVideo),
            OpenStep::AppendMediaFiles(paths(&["/m.mkv"])),
        ]
    );
}

#[test]
fn url_with_subtitle_on_video_opens_the_url_and_queues_subtitles() {
    let steps = route_plan(plan(
        DropTarget::Video,
        vec![url("https://example.org/v"), subtitle("/a.srt")],
        0,
    ));
    assert_eq!(
        steps,
        vec![
            OpenStep::QueueSubtitlesForOpenedMedia(paths(&["/a.srt"])),
            open_url_step("https://example.org/v", DropTarget::Video),
        ]
    );
}

#[test]
fn subtitle_next_to_missing_file_still_attaches_and_reports_the_missing_one() {
    let steps = route_plan(plan(
        DropTarget::Video,
        vec![
            ClassifiedItem::Missing(PathBuf::from("/gone")),
            subtitle("/a.srt"),
        ],
        0,
    ));
    assert_eq!(
        steps,
        vec![
            OpenStep::AttachSubtitlesToCurrentMedia(paths(&["/a.srt"])),
            OpenStep::Notify(DropNotice::FilesNotFound { count: 1 }),
        ]
    );
}

#[test]
fn subtitle_notice_text_is_the_documented_hint() {
    assert_eq!(
        DropNotice::SubtitlesNeedPlayingVideo.text(),
        "Drop subtitles onto a playing video."
    );
}
