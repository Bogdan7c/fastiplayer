//! Ключ фрагмента файла: окна одного файла различаются, «без окна» равен ключу файла.

use super::*;

fn segment(start_nanos: u128, end_nanos: Option<u128>) -> Option<VideoSegment> {
    Some(VideoSegment {
        start_nanos,
        end_nanos,
    })
}

fn key_with(name: &str, segment: Option<VideoSegment>) -> VideoKey {
    VideoKey::for_locator_segment(&local_locator(name), segment).expect("local key")
}

#[test]
fn no_segment_equals_plain_locator_key() {
    assert_eq!(key_with("disc.flac", None), key_of("disc.flac"));
}

#[test]
fn a_segment_differs_from_the_whole_file_key() {
    assert_ne!(key_with("disc.flac", segment(0, None)), key_of("disc.flac"));
}

#[test]
fn different_windows_of_one_file_have_different_keys() {
    let first = key_with("disc.flac", segment(0, Some(60_000_000_000)));
    let second = key_with("disc.flac", segment(60_000_000_000, Some(120_000_000_000)));
    let open_ended = key_with("disc.flac", segment(60_000_000_000, None));

    assert_ne!(first, second);
    assert_ne!(second, open_ended);
    // Нулевой конец и отсутствие конца — разные окна.
    assert_ne!(
        key_with("disc.flac", segment(5, Some(0))),
        key_with("disc.flac", segment(5, None))
    );
}

#[test]
fn the_same_window_of_the_same_file_is_stable_and_file_specific() {
    let window = segment(1_000, Some(2_000));

    assert_eq!(key_with("a.flac", window), key_with("a.flac", window));
    assert_ne!(key_with("a.flac", window), key_with("b.flac", window));
}
