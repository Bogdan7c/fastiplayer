//! Разбор документа страницы: формат по подсказке и по содержимому, кодировка по языку,
//! честные отказы. Задача запускается синхронно, как в `job_runner`.

use std::sync::Arc;

use subtitle_core::SubtitleContent;

use super::*;
use crate::page_subtitles::{
    PageSubtitleDisplay, PageSubtitleLabel, PageSubtitleLanguage, PageSubtitleTrackNumber,
};
use crate::state::subtitles::limits::LoadLimits;
use crate::state::subtitles::sidecar_job::{FontLibraryPlan, SidecarJobTask};

pub(super) const VTT: &str = "WEBVTT\n\n00:00:01.000 --> 00:00:04.000\nHello world\n";
pub(super) const SRT: &str = "1\n00:00:01,000 --> 00:00:04,000\nHello world\n";
pub(super) const SRV3: &str = r#"<?xml version="1.0" encoding="utf-8" ?><timedtext format="3"><body><p t="1000" d="3000">Hello world</p></body></timedtext>"#;
pub(super) const JSON3: &str = r#"{"wireMagic":"pb3","events":[{"tStartMs":1000,"dDurationMs":3000,"segs":[{"utf8":"Hello world"}]}]}"#;
pub(super) const TTML: &str = r#"<?xml version="1.0" encoding="utf-8"?><tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="00:00:01.000" end="00:00:04.000">Hello world</p></div></body></tt>"#;

fn document(
    bytes: &[u8],
    hint: PageSubtitleFormatHint,
    language: Option<&str>,
) -> PageSubtitleDocument {
    PageSubtitleDocument {
        track: PageSubtitleTrackNumber::new(1),
        language: language.map_or(PageSubtitleLanguage::NotNamed, |tag| {
            PageSubtitleLanguage::Tagged(tag.to_owned())
        }),
        label: PageSubtitleLabel::NotGiven,
        format_hint: hint,
        display: PageSubtitleDisplay::NotShowingOnPage,
        source: crate::page_subtitles::PageSubtitleSource::PageDownload,
        bytes: Arc::from(bytes),
    }
}

fn request_with(limits: LoadLimits, document: &PageSubtitleDocument) -> SidecarJobRequest {
    SidecarJobRequest {
        task: SidecarJobTask::ParsePageTrack {
            document: Arc::new(document.clone()),
        },
        // Шрифты не строим: проверяется разбор, а не библиотека.
        fonts: FontLibraryPlan::ReuseExisting,
        limits,
    }
}

fn parse(document: &PageSubtitleDocument) -> SidecarJobOutcome {
    let request = request_with(
        crate::state::subtitles::tests::default_load_limits(),
        document,
    );
    run_page_track_parse(&request, document, &|| false)
}

fn parsed(outcome: SidecarJobOutcome) -> ParsedPageTrack {
    match outcome {
        SidecarJobOutcome::PageTrackParsed { track, .. } => *track,
        SidecarJobOutcome::Failed(failure) => panic!("the document did not parse: {failure}"),
        _ => panic!("unexpected outcome of a page parse"),
    }
}

fn failure(outcome: SidecarJobOutcome) -> SubtitleLoadFailure {
    match outcome {
        SidecarJobOutcome::Failed(failure) => failure,
        _ => panic!("the document was expected to fail"),
    }
}

/// Тексты реплик и их начала (мс).
fn cues(track: &ParsedPageTrack) -> Vec<(i64, String)> {
    track
        .timeline
        .iter()
        .filter_map(|(_, event)| match event.content() {
            SubtitleContent::Text(cue) => Some((
                event.start().as_micros() / 1_000,
                cue.text.plain_text().trim().to_owned(),
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn every_page_format_parses_by_its_hint() {
    for (text, hint, expected) in [
        (
            VTT,
            PageSubtitleFormatHint::WebVtt,
            TextSubtitleFormat::WebVtt,
        ),
        (SRT, PageSubtitleFormatHint::Srt, TextSubtitleFormat::Srt),
        (SRV3, PageSubtitleFormatHint::Srv3, TextSubtitleFormat::Srv3),
        (
            JSON3,
            PageSubtitleFormatHint::Json3,
            TextSubtitleFormat::Json3,
        ),
        (TTML, PageSubtitleFormatHint::Ttml, TextSubtitleFormat::Ttml),
    ] {
        let track = parsed(parse(&document(text.as_bytes(), hint, Some("en"))));
        assert_eq!(track.format, expected, "{hint:?}");
        assert_eq!(
            cues(&track),
            vec![(1_000, "Hello world".to_owned())],
            "{hint:?}"
        );
    }
}

#[test]
fn content_wins_over_a_wrong_hint_and_decides_alone_without_one() {
    for (text, expected) in [
        (VTT, TextSubtitleFormat::WebVtt),
        (SRT, TextSubtitleFormat::Srt),
        (SRV3, TextSubtitleFormat::Srv3),
        (JSON3, TextSubtitleFormat::Json3),
        (TTML, TextSubtitleFormat::Ttml),
    ] {
        // Экстеншен ошибся: назвал всё SRT.
        let wrong = parsed(parse(&document(
            text.as_bytes(),
            PageSubtitleFormatHint::Srt,
            None,
        )));
        assert_eq!(wrong.format, expected);
        // Экстеншен формата не знает.
        let unknown = parsed(parse(&document(
            text.as_bytes(),
            PageSubtitleFormatHint::Unknown,
            None,
        )));
        assert_eq!(unknown.format, expected);
        assert_eq!(cues(&unknown).len(), 1);
    }
}

#[test]
fn garbage_without_a_hint_is_an_honest_unrecognized_format() {
    let outcome = parse(&document(
        b"<html>not subtitles</html>",
        PageSubtitleFormatHint::Unknown,
        None,
    ));
    assert!(matches!(
        failure(outcome),
        SubtitleLoadFailure::UnrecognizedFormat
    ));
}

#[test]
fn garbage_with_a_hint_fails_parsing_and_an_empty_document_has_no_cues() {
    let garbage = parse(&document(
        b"\x00\x01\x02 binary junk",
        PageSubtitleFormatHint::WebVtt,
        None,
    ));
    assert!(matches!(
        failure(garbage),
        SubtitleLoadFailure::ParseFailed(_)
    ));
    let empty = parse(&document(
        b"WEBVTT\n\n",
        PageSubtitleFormatHint::WebVtt,
        None,
    ));
    assert!(matches!(failure(empty), SubtitleLoadFailure::NoCuesFound));
}

/// Кириллица в Windows-1251 (как старые русские SRT): А..я = 0xC0..0xFF, ё = 0xB8.
fn windows_1251(text: &str) -> Vec<u8> {
    text.chars()
        .map(|character| match character {
            'А'..='я' => {
                u8::try_from(u32::from(character) - 0x0410 + 0xC0).expect("cp1251 range")
            }
            'ё' => 0xB8,
            other => u8::try_from(u32::from(other)).expect("ASCII only besides Cyrillic"),
        })
        .collect()
}

#[test]
fn a_windows_1251_srt_of_a_russian_track_reads_as_cyrillic() {
    let text =
        "1\n00:00:01,000 --> 00:00:04,000\nПривет, как дела? Это проверка русских субтитров.\n";
    let track = parsed(parse(&document(
        &windows_1251(text),
        PageSubtitleFormatHint::Srt,
        Some("ru"),
    )));
    assert_eq!(
        cues(&track),
        vec![(
            1_000,
            "Привет, как дела? Это проверка русских субтитров.".to_owned()
        )]
    );
}

#[test]
fn a_document_over_the_configured_file_limit_is_refused_before_parsing() {
    let page_document = document(VTT.as_bytes(), PageSubtitleFormatHint::WebVtt, None);
    let mut limits = crate::state::subtitles::tests::default_load_limits();
    limits.file_max_bytes = VTT.len() - 1;
    let request = request_with(limits, &page_document);
    let outcome = run_page_track_parse(&request, &page_document, &|| false);
    assert!(matches!(
        failure(outcome),
        SubtitleLoadFailure::FileTooLarge { limit_bytes } if limit_bytes == VTT.len() - 1
    ));
}

#[test]
fn a_cancelled_parse_returns_nothing() {
    let page_document = document(VTT.as_bytes(), PageSubtitleFormatHint::WebVtt, None);
    let request = request_with(
        crate::state::subtitles::tests::default_load_limits(),
        &page_document,
    );
    assert!(matches!(
        run_page_track_parse(&request, &page_document, &|| true),
        SidecarJobOutcome::Cancelled
    ));
}
