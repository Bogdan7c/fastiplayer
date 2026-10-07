use super::*;
use crate::external_open::uri::item_from_uri;

fn request(target: DropTarget, items: Vec<ExternalOpenItem>) -> ExternalOpenRequest {
    ExternalOpenRequest { target, items }
}

fn local(path: &std::path::Path) -> ExternalOpenItem {
    ExternalOpenItem::LocalPath(path.to_path_buf())
}

#[test]
fn files_directories_playlists_and_missing_paths_are_told_apart() {
    let dir = tempfile::tempdir().expect("temp dir");
    let media = dir.path().join("clip.mkv");
    let playlist = dir.path().join("list.M3U8");
    let folder = dir.path().join("album");
    let missing = dir.path().join("gone.mp4");
    std::fs::write(&media, b"x").expect("media");
    std::fs::write(&playlist, b"#EXTM3U").expect("playlist");
    std::fs::create_dir(&folder).expect("folder");

    // Каждый вид отдельным запросом: смесь с плейлистом проверяется в другом тесте.
    let classify_one =
        |item: ExternalOpenItem| classify_request(request(DropTarget::Video, vec![item])).items;
    assert_eq!(
        classify_one(local(&media)),
        vec![ClassifiedItem::MediaFile(media.clone())]
    );
    assert_eq!(
        classify_one(local(&folder)),
        vec![ClassifiedItem::Directory(folder.clone())]
    );
    assert_eq!(
        classify_one(local(&playlist)),
        vec![ClassifiedItem::PlaylistFile(playlist.clone())]
    );
    assert_eq!(
        classify_one(local(&missing)),
        vec![ClassifiedItem::Missing(missing.clone())]
    );
}

#[test]
fn web_urls_and_unsupported_items_pass_through_with_target() {
    let plan = classify_request(request(
        DropTarget::Playlist,
        vec![
            item_from_uri("https://example.org/v"),
            item_from_uri("ftp://example.org/a"),
        ],
    ));
    assert_eq!(plan.target, DropTarget::Playlist);
    assert!(matches!(plan.items[0], ClassifiedItem::WebUrl(_)));
    assert_eq!(
        plan.items[1],
        ClassifiedItem::Unsupported {
            scheme: "ftp".to_owned()
        }
    );
    assert_eq!(plan.skipped_playlist_files, 0);
}

#[test]
fn playlist_mixed_with_media_is_skipped_and_counted() {
    let dir = tempfile::tempdir().expect("temp dir");
    let media = dir.path().join("a.mkv");
    let playlist_one = dir.path().join("one.m3u");
    let playlist_two = dir.path().join("two.xspf");
    for path in [&media, &playlist_one, &playlist_two] {
        std::fs::write(path, b"x").expect("fixture");
    }

    let plan = classify_request(request(
        DropTarget::Video,
        vec![local(&playlist_one), local(&media), local(&playlist_two)],
    ));

    assert_eq!(plan.items, vec![ClassifiedItem::MediaFile(media)]);
    assert_eq!(plan.skipped_playlist_files, 2);
}

#[test]
fn playlists_alone_are_kept_for_the_playlist_stage() {
    let dir = tempfile::tempdir().expect("temp dir");
    let playlist = dir.path().join("only.m3u");
    std::fs::write(&playlist, b"x").expect("fixture");

    let plan = classify_request(request(DropTarget::Video, vec![local(&playlist)]));

    assert_eq!(plan.items, vec![ClassifiedItem::PlaylistFile(playlist)]);
    assert_eq!(plan.skipped_playlist_files, 0);
}

#[test]
fn empty_request_gives_empty_plan() {
    let plan = classify_request(request(DropTarget::Video, Vec::new()));
    assert!(plan.items.is_empty());
}

#[test]
fn playlist_next_to_missing_file_or_unsupported_link_is_not_skipped() {
    let dir = tempfile::tempdir().expect("temp dir");
    let playlist = dir.path().join("list.m3u");
    std::fs::write(&playlist, b"x").expect("fixture");
    let missing = dir.path().join("gone.mkv");

    let with_missing = classify_request(request(
        DropTarget::Video,
        vec![local(&playlist), local(&missing)],
    ));
    let with_unsupported = classify_request(request(
        DropTarget::Video,
        vec![
            local(&playlist),
            ExternalOpenItem::Unsupported {
                scheme: "ftp".to_owned(),
            },
        ],
    ));

    // Пропавший файл ничего не открывает: плейлист остаётся и импортируется.
    assert_eq!(with_missing.skipped_playlist_files, 0);
    assert_eq!(
        with_missing.items,
        vec![
            ClassifiedItem::PlaylistFile(playlist.clone()),
            ClassifiedItem::Missing(missing)
        ]
    );
    assert_eq!(with_unsupported.skipped_playlist_files, 0);
    assert!(
        with_unsupported
            .items
            .contains(&ClassifiedItem::PlaylistFile(playlist))
    );
}
