//! Описание файла, выбранного пользователем: формат, язык, forced и SDH по имени.
#![allow(clippy::expect_used)]

use std::path::Path;

use subtitle_core::SubtitleFormat;
use subtitle_sidecar::{
    SidecarLocation, SidecarSubtitleFormat, describe_subtitle_file, is_subtitle_file_extension,
};

fn language_of(path: &str) -> Option<String> {
    describe_subtitle_file(Path::new(path))
        .expect("subtitle file")
        .language
        .map(|language| language.as_str().to_owned())
}

#[test]
fn language_forced_and_sdh_come_from_the_dotted_tail() {
    let candidate = describe_subtitle_file(Path::new("/media/Movie.ru.forced.SRT"))
        .expect("an srt is a subtitle file");
    assert_eq!(candidate.format, SidecarSubtitleFormat::Srt);
    assert_eq!(candidate.format.to_subtitle_format(), SubtitleFormat::Srt);
    assert_eq!(candidate.location, SidecarLocation::ChosenByUser);
    assert_eq!(candidate.path, Path::new("/media/Movie.ru.forced.SRT"));
    assert_eq!(
        candidate.language.map(|l| l.as_str().to_owned()),
        Some("ru".to_owned())
    );
    assert!(candidate.disposition.is_forced);
    assert!(!candidate.is_hearing_impaired);

    let sdh = describe_subtitle_file(Path::new("Movie.en.sdh.vtt")).expect("vtt");
    assert_eq!(sdh.format, SidecarSubtitleFormat::WebVtt);
    assert!(sdh.is_hearing_impaired);
    assert!(!sdh.disposition.is_forced);
}

#[test]
fn a_bare_name_has_no_language_even_if_the_word_looks_like_a_code() {
    assert_eq!(language_of("Movie.srt"), None);
    assert_eq!(language_of("The.srt"), None, "the title is not a hint");
    assert_eq!(language_of("Movie.English.srt"), Some("en".to_owned()));
}

#[test]
fn only_the_last_dotted_parts_are_hints() {
    // `uk` стоит далеко от конца и не попадает в окно подсказок.
    assert_eq!(language_of("Show.uk.S01E02.1080p.WEB.srt"), None);
    assert_eq!(
        language_of("Show.S01E02.1080p.WEB.ru.srt"),
        Some("ru".to_owned())
    );
}

#[test]
fn non_subtitle_files_are_not_described() {
    for name in ["notes.txt", "Movie.mkv", "Movie", "Movie.srt.bak"] {
        assert!(describe_subtitle_file(Path::new(name)).is_none(), "{name}");
    }
}

#[test]
fn extension_helpers_are_case_insensitive_and_know_all_formats() {
    for extension in ["srt", "VTT", "Ass", "ssa"] {
        assert!(is_subtitle_file_extension(extension), "{extension}");
    }
    assert!(!is_subtitle_file_extension("txt"));
    assert!(!is_subtitle_file_extension(""));
    assert_eq!(
        SidecarSubtitleFormat::from_path(Path::new("a.ass")),
        Some(SidecarSubtitleFormat::Ass)
    );
    let ass = describe_subtitle_file(Path::new("a.ass")).expect("ass is described");
    assert!(
        ass.format.is_renderable_now(),
        "ass is renderable since session 08"
    );
}

#[test]
fn release_tags_and_unrelated_parts_do_not_become_languages() {
    assert_eq!(language_of("Show.1080p.WEB.srt"), None);
    assert_eq!(
        language_of("Show.S01E02.1080p.WEB.ru.srt"),
        Some("ru".to_owned())
    );
}

#[test]
fn underscore_space_and_bracket_separated_tails_are_hints_too() {
    // Официальные субтитры Sintel названы `sintel_ru.srt`: язык после подчёркивания.
    assert_eq!(language_of("sintel_ru.srt"), Some("ru".to_owned()));
    assert_eq!(language_of("Movie English.srt"), Some("en".to_owned()));
    assert_eq!(language_of("Movie_pt-BR.srt"), Some("pt-BR".to_owned()));
    let forced =
        describe_subtitle_file(Path::new("/media/Movie [Forced].srt")).expect("subtitle extension");
    assert!(forced.disposition.is_forced);
}

#[test]
fn unknown_three_letter_words_are_not_languages_without_a_video_to_compare_with() {
    // `lie` и `tlh` синтаксически похожи на коды ISO 639, но не из таблицы известных языков.
    assert_eq!(language_of("The_Big_Lie.srt"), None);
    assert_eq!(language_of("Movie.tlh.srt"), None);
    assert_eq!(language_of("Movie.eng.srt"), Some("eng".to_owned()));
}
