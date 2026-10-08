use std::ffi::OsString;
use std::path::{Path, PathBuf};

use fastiplayer_config::AppConfig;

use super::*;

fn arguments(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn paths(values: &[&str]) -> Vec<PathBuf> {
    values.iter().map(PathBuf::from).collect()
}

/// Разбирает аргументы и сразу разворачивает набор так же, как это делает startup controller.
fn resolve_to_single(values: &[&str]) -> (Option<SingleStartupTarget>, Option<String>) {
    let (initial_media, startup_error) =
        resolve_initial_media_arguments(arguments(values), &AppConfig::default());
    let single = initial_media.map(|initial_media| match initial_media {
        InitialMedia::Several(several) => several.into_single_target(),
        other => panic!("ожидался набор из нескольких аргументов, получено {other:?}"),
    });
    (single, startup_error)
}

fn url_text(locator: &StartupUrlLocator) -> String {
    locator
        .to_playlist_locator()
        .expect("reopenable URL")
        .expose_secret_for_persistence()
        .to_owned()
}

#[test]
fn zero_arguments_open_nothing() {
    let (initial_media, startup_error) =
        resolve_initial_media_arguments(Vec::new(), &AppConfig::default());
    assert!(initial_media.is_none());
    assert!(startup_error.is_none());
}

#[test]
fn single_argument_takes_exactly_the_previous_single_path() {
    let config = AppConfig::default();
    let (file, file_error) = resolve_initial_media_arguments(arguments(&["/tmp/a.mkv"]), &config);
    assert!(file_error.is_none());
    assert!(matches!(file, Some(InitialMedia::File(path)) if path == Path::new("/tmp/a.mkv")));

    let (playlist, _) = resolve_initial_media_arguments(arguments(&["/tmp/list.m3u"]), &config);
    assert!(
        matches!(playlist, Some(InitialMedia::Playlist(path)) if path == Path::new("/tmp/list.m3u"))
    );

    let (url, _) =
        resolve_initial_media_arguments(arguments(&["https://youtu.be/video-id"]), &config);
    assert!(
        matches!(url, Some(InitialMedia::Url(locator)) if url_text(&locator) == "https://youtu.be/video-id")
    );

    let (rejected, rejected_error) =
        resolve_initial_media_arguments(arguments(&["rtsp://192.0.2.10/v.mp4"]), &config);
    assert!(rejected.is_none());
    assert!(
        rejected_error
            .as_deref()
            .is_some_and(|error| error.contains("scheme не поддерживается"))
    );
}

#[test]
fn several_local_files_open_in_given_order_without_notice() {
    let (single, startup_error) = resolve_to_single(&["/v/c.mkv", "/v/a.mkv", "/v/b.mkv"]);
    let single = single.expect("набор файлов открывается");

    assert!(startup_error.is_none());
    assert!(matches!(single.target, InitialMedia::File(path) if path == Path::new("/v/c.mkv")));
    assert_eq!(single.follow_up_files, paths(&["/v/a.mkv", "/v/b.mkv"]));
    assert_eq!(single.skipped_notice, None);
}

#[test]
fn mixed_list_opens_only_local_files_and_counts_skipped_playlists_and_links() {
    let (single, startup_error) = resolve_to_single(&[
        "/v/list.m3u",
        "/v/a.mkv",
        "https://youtu.be/video-id",
        "/v/b.mp3",
        "rtsp://192.0.2.10/v.mp4",
    ]);
    let single = single.expect("локальные файлы побеждают");

    // Неподдерживаемая ссылка среди пропущенных не превращается в ошибку запуска.
    assert!(startup_error.is_none());
    assert!(matches!(single.target, InitialMedia::File(path) if path == Path::new("/v/a.mkv")));
    assert_eq!(single.follow_up_files, paths(&["/v/b.mp3"]));
    assert_eq!(
        single.skipped_notice.as_deref(),
        Some("Открыты только видео- и аудиофайлы, пропущено: 1 плейлист и 2 ссылки.")
    );
}

#[test]
fn exactly_one_local_file_among_others_opens_as_single_file_with_notice() {
    let (single, _) = resolve_to_single(&["https://youtu.be/video-id", "/v/only.mkv"]);
    let single = single.expect("один локальный файл");

    assert!(matches!(single.target, InitialMedia::File(path) if path == Path::new("/v/only.mkv")));
    assert!(single.follow_up_files.is_empty());
    assert_eq!(
        single.skipped_notice.as_deref(),
        Some("Открыты только видео- и аудиофайлы, пропущено: 1 ссылка.")
    );
}

#[test]
fn without_local_files_first_playlist_wins_and_rest_are_skipped() {
    let (single, startup_error) =
        resolve_to_single(&["https://youtu.be/video-id", "/v/one.xspf", "/v/two.m3u8"]);
    let single = single.expect("первый плейлист");

    assert!(startup_error.is_none());
    assert!(
        matches!(single.target, InitialMedia::Playlist(path) if path == Path::new("/v/one.xspf"))
    );
    assert!(single.follow_up_files.is_empty());
    assert_eq!(
        single.skipped_notice.as_deref(),
        Some("Открыт только первый плейлист, пропущено: 1 плейлист и 1 ссылка.")
    );
}

#[test]
fn links_only_open_the_first_link() {
    let (single, startup_error) = resolve_to_single(&[
        "https://youtu.be/first-id",
        "https://cdn.example.test/video.mp4",
        "https://youtu.be/third-id",
    ]);
    let single = single.expect("первая ссылка");

    assert!(startup_error.is_none());
    assert!(
        matches!(&single.target, InitialMedia::Url(locator) if url_text(locator) == "https://youtu.be/first-id")
    );
    assert_eq!(
        single.skipped_notice.as_deref(),
        Some("Открыта только первая ссылка, пропущено: 2 ссылки.")
    );
}

#[test]
fn unsupported_first_link_keeps_the_existing_safe_error_path() {
    let (initial_media, startup_error) = resolve_initial_media_arguments(
        arguments(&["rtsp://192.0.2.10/v.mp4", "https://youtu.be/video-id"]),
        &AppConfig::default(),
    );

    assert!(initial_media.is_none());
    assert!(
        startup_error
            .as_deref()
            .is_some_and(|error| error.contains("scheme не поддерживается"))
    );
}

#[cfg(unix)]
#[test]
fn several_non_utf8_paths_stay_exact_native_paths() {
    use std::os::unix::ffi::OsStringExt;

    let first = OsString::from_vec(b"/v/movie-\xFF.mkv".to_vec());
    let second = OsString::from_vec(b"/v/\xFE-clip.mkv".to_vec());
    let (initial_media, _) =
        resolve_initial_media_arguments(vec![first.clone(), second.clone()], &AppConfig::default());
    let Some(InitialMedia::Several(several)) = initial_media else {
        panic!("ожидался набор файлов");
    };
    let single = several.into_single_target();

    assert!(matches!(single.target, InitialMedia::File(path) if path.as_os_str() == first));
    assert_eq!(single.follow_up_files, vec![PathBuf::from(second)]);
}

#[test]
fn local_files_beyond_queue_capacity_are_not_opened_and_reported() {
    let classified = ["/v/1.mkv", "/v/2.mkv", "/v/3.mkv", "/v/4.mkv"]
        .into_iter()
        .map(|path| ClassifiedStartupArgument::LocalMedia(PathBuf::from(path)))
        .collect();

    let (initial_media, startup_error) = select_among_several(classified, 2);
    let Some(InitialMedia::Several(several)) = initial_media else {
        panic!("ожидался набор файлов");
    };
    let single = several.into_single_target();

    assert!(startup_error.is_none());
    assert!(matches!(single.target, InitialMedia::File(path) if path == Path::new("/v/1.mkv")));
    assert_eq!(single.follow_up_files, paths(&["/v/2.mkv"]));
    assert_eq!(
        single.skipped_notice.as_deref(),
        Some("В очередь помещается не больше 2 файлов, остальные 2 не открыты.")
    );
}

#[test]
fn debug_of_several_arguments_hides_paths() {
    let (initial_media, _) = resolve_initial_media_arguments(
        arguments(&["/home/secret-dir/a.mkv", "/home/secret-dir/b.mkv"]),
        &AppConfig::default(),
    );
    let debug = format!("{initial_media:?}");

    assert!(!debug.contains("secret-dir"), "{debug}");
    assert!(debug.contains("file_count: 2"), "{debug}");
}
