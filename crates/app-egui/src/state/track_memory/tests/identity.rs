//! Ключ видео: локальные файлы, web-ссылки (без раскрытия ссылки) и окна воспроизведения.

use std::time::Instant;

use media_core::MediaTime;
use player_core::MediaPlaybackWindow;

use super::*;
use crate::media_open::WebMediaSourceIntent;
use crate::state::ActiveMediaSource;
use crate::state::track_memory::identity::video_key_for_source;

fn web_source(url: &str) -> ActiveMediaSource {
    let locator = media_source_open::direct_progressive_open::classify_direct_media_url(url)
        .expect("direct fixture URL is valid");
    ActiveMediaSource::Web(WebMediaSourceIntent::direct(locator))
}

fn window(start_secs: u64, end_secs: Option<u64>) -> MediaPlaybackWindow {
    MediaPlaybackWindow::new(
        MediaTime::from_duration(Duration::from_secs(start_secs)),
        end_secs.map(|secs| MediaTime::from_duration(Duration::from_secs(secs))),
    )
    .expect("window is non-empty")
}

#[test]
fn the_same_web_url_gives_the_same_key_and_another_url_another_key() {
    let first =
        video_key_for_source(&web_source("https://cdn.example.test/a.mp4?token=one")).expect("key");
    let again =
        video_key_for_source(&web_source("https://cdn.example.test/a.mp4?token=one")).expect("key");
    let other =
        video_key_for_source(&web_source("https://cdn.example.test/b.mp4?token=one")).expect("key");

    assert_eq!(first, again);
    assert_ne!(first, other);
}

#[test]
fn the_raw_web_url_never_reaches_the_memory_file_or_the_key_debug_output() {
    let secret_url = "https://user:hunter2@cdn.example.test/private/movie.mp4?token=supersecret";
    let source = web_source(secret_url);
    let key = video_key_for_source(&source).expect("key");
    let directory = tempfile::tempdir().expect("tempdir");
    let (handle, _) = start_in(directory.path(), 10);
    {
        let mut owner = handle.lock();
        let now = Instant::now();
        owner.begin_video(instance(1), Some(key), now);
        owner.remember_audio(instance(1), audio_hint("eng", "Original", "A_AAC", 0), now);
        owner.shutdown(Duration::from_secs(5));
    }
    wait_for_entry_on_disk(directory.path(), &key);

    let file_text =
        String::from_utf8_lossy(&std::fs::read(memory_file(directory.path())).expect("read file"))
            .into_owned();
    let debug_text = format!("{key:?}");

    for forbidden in ["hunter2", "supersecret", "cdn.example.test", "movie.mp4"] {
        assert!(!file_text.contains(forbidden), "file leaks {forbidden}");
        assert!(!debug_text.contains(forbidden), "debug leaks {forbidden}");
    }
}

#[test]
fn local_files_are_keyed_by_exact_path() {
    let a =
        video_key_for_source(&ActiveMediaSource::LocalFile("/videos/a.mkv".into())).expect("key");
    let same =
        video_key_for_source(&ActiveMediaSource::LocalFile("/videos/a.mkv".into())).expect("key");
    let moved =
        video_key_for_source(&ActiveMediaSource::LocalFile("/other/a.mkv".into())).expect("key");

    assert_eq!(a, same);
    assert_ne!(a, moved, "another path is another video");
}

#[test]
fn playback_windows_of_one_file_are_different_videos() {
    let file = || ActiveMediaSource::LocalFile("/music/album.flac".into());
    let whole = video_key_for_source(&file()).expect("key");
    let first_track =
        video_key_for_source(&file().with_playback_window(window(0, Some(200)))).expect("key");
    let second_track =
        video_key_for_source(&file().with_playback_window(window(200, Some(400)))).expect("key");
    let second_track_again =
        video_key_for_source(&file().with_playback_window(window(200, Some(400)))).expect("key");
    let last_track =
        video_key_for_source(&file().with_playback_window(window(400, None))).expect("key");

    assert_ne!(first_track, second_track);
    assert_ne!(second_track, last_track);
    assert_ne!(whole, first_track, "a window is not the whole file");
    assert_eq!(second_track, second_track_again, "stable between runs");
}

#[test]
fn a_window_over_a_web_source_is_keyed_by_the_url_and_the_window() {
    let url = "https://cdn.example.test/album.flac";
    let first = video_key_for_source(&web_source(url).with_playback_window(window(0, Some(60))))
        .expect("key");
    let second = video_key_for_source(&web_source(url).with_playback_window(window(60, Some(120))))
        .expect("key");

    assert_ne!(first, second);
    assert_ne!(
        first,
        video_key_for_source(
            &web_source("https://cdn.example.test/other.flac")
                .with_playback_window(window(0, Some(60)))
        )
        .expect("key")
    );
}
