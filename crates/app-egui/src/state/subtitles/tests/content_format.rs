//! Формат файла субтитров решает содержимое, а не расширение: переименованный файл
//! (ASS в `Movie.srt`, SRT в `.ass`/`.vtt`) читается своим разборщиком и доходит до кадра.
//! Тесты идут настоящим путём: поиск рядом с видео, фоновая задача, дорожка, overlay.

use subtitle_core::{SubtitleContent, SubtitleEvent};

use super::ass_sidecar::{SAMPLE_ASS, displayed_events};
use super::track_selection::{movie_with, overlay_present, settle};
use super::*;

/// Поднимает владельца с одним файлом `file_name` рядом с видео и ждёт окончания загрузки.
fn owner_with_sidecar(file_name: &str, content: &str) -> (SubtitleOwner, tempfile::TempDir) {
    let (directory, video) = movie_with(&[(file_name, content)]);
    let mut owner = owner_with_config(&enabled_config());
    begin_and_wait(&mut owner, instance(1), &video);
    settle(&mut owner);
    (owner, directory)
}

/// Тексты обычных (не ASS) реплик показываемой дорожки.
fn plain_texts(events: &[SubtitleEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event.content() {
            SubtitleContent::Text(cue) => Some(cue.text.plain_text()),
            _ => None,
        })
        .collect()
}

#[test]
fn ass_content_in_a_srt_named_file_is_loaded_as_ass_and_reaches_the_frame() {
    let (mut owner, _directory) = owner_with_sidecar("Movie.srt", SAMPLE_ASS);

    let events = displayed_events(&owner);
    assert_eq!(events.len(), 1, "the dialogue line must not be lost");
    assert!(matches!(events[0].content(), SubtitleContent::Ass(_)));
    assert!(
        overlay_present(&mut owner, instance(1), 2_000),
        "the dialogue is drawn at 2 s"
    );
}

#[test]
fn srt_content_in_an_ass_named_file_shows_its_cues() {
    let (mut owner, _directory) = owner_with_sidecar("Movie.ass", TWO_CUE_SRT);

    let events = displayed_events(&owner);
    assert_eq!(plain_texts(&events), ["First cue", "Second cue"]);
    assert!(overlay_present(&mut owner, instance(1), 2_000));
}

#[test]
fn srt_content_in_a_vtt_named_file_shows_its_cues() {
    let (mut owner, _directory) = owner_with_sidecar("Movie.vtt", TWO_CUE_SRT);

    let events = displayed_events(&owner);
    assert_eq!(plain_texts(&events), ["First cue", "Second cue"]);
    assert!(overlay_present(&mut owner, instance(1), 6_000));
}

#[test]
fn files_whose_content_matches_the_extension_are_unchanged() {
    let (owner_srt, _srt_dir) = owner_with_sidecar("Movie.srt", TWO_CUE_SRT);
    assert_eq!(
        plain_texts(&displayed_events(&owner_srt)),
        ["First cue", "Second cue"]
    );

    let (owner_vtt, _vtt_dir) = owner_with_sidecar(
        "Movie.vtt",
        "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nVtt cue\n",
    );
    assert_eq!(plain_texts(&displayed_events(&owner_vtt)), ["Vtt cue"]);

    let (owner_ass, _ass_dir) = owner_with_sidecar("Movie.ass", SAMPLE_ASS);
    assert_eq!(displayed_events(&owner_ass).len(), 1);
}
