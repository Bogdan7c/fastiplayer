//! Язык текста реплик: дорожка (по имени файла), иначе язык системы, доходит до растеризатора.
//!
//! Идём настоящим путём: файл `Movie.ja.srt` на диске -> фоновая задача -> дорожка ->
//! презентатор -> запрос к растеризатору. Видимый результат подбора лица проверяют тесты
//! `subtitle-raster`; здесь проверяется, что запрос НЕСЁТ нужный язык.

use std::path::Path;

use subtitle_core::SubtitleLanguage;
use subtitle_raster::TextLanguagePreference;

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};

const GENERATION: u64 = 3;
/// Середина первой реплики `TWO_CUE_SRT`.
const INSIDE_CUE_ONE: u64 = 2_000;

fn language(tag: &str) -> SubtitleLanguage {
    SubtitleLanguage::parse(tag).expect("valid tag")
}

fn preferred(tag: &str) -> TextLanguagePreference {
    TextLanguagePreference::preferred(&language(tag))
}

/// Владелец с заданным языком системы и встроенными шрифтами.
fn owner_with_system_language(system_language: Option<SubtitleLanguage>) -> SubtitleOwner {
    let mut owner = SubtitleOwner::new(
        AppWakePort::disconnected(AppWakeOwner::Subtitles),
        system_language,
        &enabled_config(),
    );
    owner.use_embedded_fonts_only();
    owner
}

/// `Movie.mkv` и файл субтитров с именем `subtitle_name` рядом.
fn write_movie_with_subtitle(directory: &Path, subtitle_name: &str) -> PathBuf {
    let video = directory.join("Movie.mkv");
    std::fs::write(&video, b"not a real video").expect("write dummy video");
    std::fs::write(directory.join(subtitle_name), TWO_CUE_SRT).expect("write srt");
    video
}

/// Загружает дорожку, показывает первую реплику и возвращает язык, с которым рисовали.
fn language_used_to_draw(
    owner: &mut SubtitleOwner,
    media: MediaInstanceId,
    video: &Path,
) -> TextLanguagePreference {
    begin_and_wait(owner, media, video);
    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Ready);
    let rect = full_rect(1280, 720);
    let shown = owner
        .overlay_for_frame(
            current(media, GENERATION),
            &frame(INSIDE_CUE_ONE, GENERATION, rect),
        )
        .is_some();
    assert!(shown, "the cue must be drawn");
    owner.presentation_stats().last_text_language
}

#[test]
fn request_carries_the_language_from_the_subtitle_file_name() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie_with_subtitle(directory.path(), "Movie.ja.srt");
    // Язык системы другой: побеждает язык дорожки.
    let mut owner = owner_with_system_language(Some(language("ru-RU")));
    let used = language_used_to_draw(&mut owner, instance(1), &video);
    assert_eq!(used, preferred("ja"));
    assert_ne!(used, preferred("ru-RU"));
}

#[test]
fn request_falls_back_to_the_system_language_when_the_track_has_none() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie_with_subtitle(directory.path(), "Movie.srt");
    let mut owner = owner_with_system_language(Some(language("zh-TW")));
    let used = language_used_to_draw(&mut owner, instance(1), &video);
    assert_eq!(used, preferred("zh-TW"));
}

#[test]
fn request_has_no_preference_without_track_or_system_language() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie_with_subtitle(directory.path(), "Movie.srt");
    let mut owner = owner_with_system_language(None);
    let used = language_used_to_draw(&mut owner, instance(1), &video);
    assert_eq!(used, TextLanguagePreference::unknown());
}

#[test]
fn new_media_with_another_track_language_redraws_with_the_new_language() {
    let japanese_dir = tempfile::tempdir().expect("tempdir");
    let japanese_video = write_movie_with_subtitle(japanese_dir.path(), "Movie.ja.srt");
    let korean_dir = tempfile::tempdir().expect("tempdir");
    let korean_video = write_movie_with_subtitle(korean_dir.path(), "Movie.ko.srt");
    let mut owner = owner_with_system_language(None);

    let first = language_used_to_draw(&mut owner, instance(1), &japanese_video);
    let calls_after_first = owner.presentation_stats().rasterizer_calls;
    let second = language_used_to_draw(&mut owner, instance(2), &korean_video);

    assert_eq!(first, preferred("ja"));
    assert_eq!(second, preferred("ko"));
    assert_eq!(
        owner.presentation_stats().rasterizer_calls,
        calls_after_first + 1,
        "the shown overlay of the old track must not be reused"
    );
}

/// Реальный случай из жизни: `Movie.rus.srt` и `Movie.eng.srt` рядом, предпочтения `ru, en`.
#[test]
fn three_letter_sidecar_codes_are_matched_by_two_letter_preferences() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = directory.path().join("Movie.mkv");
    std::fs::write(&video, b"not a real video").expect("write dummy video");
    std::fs::write(directory.path().join("Movie.eng.srt"), TWO_CUE_SRT).expect("write eng");
    std::fs::write(directory.path().join("Movie.rus.srt"), TWO_CUE_SRT).expect("write rus");
    let mut owner = owner_with_config(&SubtitlesConfig {
        enabled: true,
        preferred_languages: "ru, en".to_owned(),
        ..SubtitlesConfig::default()
    });
    begin_and_wait(&mut owner, instance(1), &video);

    let view = owner.menu_view();
    let selected = view
        .tracks
        .iter()
        .find(|track| track.is_selected)
        .expect("a track is selected");
    assert_eq!(selected.label.file_name.as_deref(), Some("Movie.rus.srt"));
}
