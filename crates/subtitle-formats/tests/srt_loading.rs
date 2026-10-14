#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Сквозные тесты SRT: байты -> `load_text_subtitle` -> `SubtitleTimeline` -> активные реплики.

mod common;

use common::{ROOMY_LIMITS, active_texts_at, plain_text_of, try_load_text};
use subtitle_core::{
    FontSlant, FontWeight, ScreenAnchor, SpanContent, SpanStyle, SubtitleColor, SubtitleContent,
    TextUnderline,
};
use subtitle_formats::{LegacyEncodingHint, LoadedTextSubtitle};
use subtitle_formats::{ParseIssueKind, TextSubtitleFormat, sniff_text_subtitle_format};

/// SRT-загрузка для тестов этого файла.
fn try_load_srt(
    bytes: &[u8],
    hint: LegacyEncodingHint,
    limits: &subtitle_formats::SubtitleParseLimits,
) -> Result<subtitle_formats::LoadedTextSubtitle, subtitle_formats::SubtitleLoadError> {
    try_load_text(
        subtitle_formats::TextSubtitleFormat::Srt,
        bytes,
        hint,
        limits,
    )
}

fn load_srt(text: &str) -> LoadedTextSubtitle {
    try_load_srt(text.as_bytes(), LegacyEncodingHint::Generic, &ROOMY_LIMITS).unwrap()
}

const TWO_CUES: &str =
    "1\n00:00:01,000 --> 00:00:03,000\nFirst\n\n2\n00:00:05,000 --> 00:00:06,500\nSecond\n";

fn only_text_cue(loaded: &subtitle_formats::LoadedTextSubtitle) -> &subtitle_core::SubtitleTextCue {
    match loaded.document().events().first().unwrap().content() {
        SubtitleContent::Text(cue) => cue,
        _ => panic!("expected text cue"),
    }
}

#[test]
fn cue_is_active_exactly_inside_half_open_interval() {
    let loaded = load_srt(TWO_CUES);
    assert_eq!(loaded.document().events().len(), 2);
    assert!(active_texts_at(&loaded, 999).is_empty());
    assert_eq!(active_texts_at(&loaded, 1_000), ["First"]);
    assert_eq!(active_texts_at(&loaded, 2_999), ["First"]);
    assert!(active_texts_at(&loaded, 3_000).is_empty());
    assert_eq!(active_texts_at(&loaded, 5_000), ["Second"]);
    assert!(active_texts_at(&loaded, 6_500).is_empty());
}

#[test]
fn crlf_cr_and_repeated_blank_lines_split_blocks_the_same_way() {
    let crlf = TWO_CUES.replace('\n', "\r\n");
    let old_mac = TWO_CUES.replace('\n', "\r");
    let spaced = TWO_CUES.replace("\n\n", "\n\n\n  \n\n");
    for source in [crlf, old_mac, spaced] {
        let loaded = load_srt(&source);
        assert_eq!(active_texts_at(&loaded, 1_500), ["First"]);
        assert_eq!(active_texts_at(&loaded, 5_500), ["Second"]);
        assert_eq!(loaded.document().report().skipped_cues(), 0);
    }
}

#[test]
fn cue_numbers_are_optional_and_multiline_text_is_kept() {
    let loaded = load_srt("00:00:01,000 --> 00:00:02,000\nline one\nline two\n");
    assert_eq!(active_texts_at(&loaded, 1_500), ["line one\nline two"]);
}

#[test]
fn broken_cue_in_the_middle_is_skipped_and_others_survive() {
    let source = "1\n00:00:01,000 --> 00:00:02,000\nA\n\n                  2\n00:00:xx,000 --> 00:00:04,000\nBROKEN\n\n                  3\n00:00:05,000 --> 00:00:06,000\nC\n";
    let loaded = load_srt(source);
    let report = loaded.document().report();
    assert_eq!(report.skipped_cues(), 1);
    assert_eq!(report.issues().len(), 1);
    assert_eq!(report.issues()[0].kind, ParseIssueKind::InvalidTimestamp);
    assert_eq!(report.issues()[0].line_number.get(), 6);
    assert_eq!(active_texts_at(&loaded, 1_500), ["A"]);
    assert!(active_texts_at(&loaded, 3_500).is_empty());
    assert_eq!(active_texts_at(&loaded, 5_500), ["C"]);
}

#[test]
fn block_without_timing_line_and_reversed_times_are_reported_separately() {
    let source = "1\njust text without timing\n\n                  2\n00:00:05,000 --> 00:00:04,000\nreversed\n\n                  3\n00:00:06,000 --> 00:00:07,000\nok\n";
    let loaded = load_srt(source);
    let report = loaded.document().report();
    assert_eq!(report.skipped_cues(), 2);
    let kinds: Vec<_> = report
        .issues()
        .iter()
        .map(|issue| issue.kind.clone())
        .collect();
    assert_eq!(
        kinds,
        [
            ParseIssueKind::MissingTimingLine,
            ParseIssueKind::EndNotAfterStart
        ]
    );
    assert_eq!(active_texts_at(&loaded, 6_500), ["ok"]);
}

#[test]
fn empty_cue_text_is_counted_not_reported_as_error() {
    let source = "1\n00:00:01,000 --> 00:00:02,000\n\n2\n00:00:03,000 --> 00:00:04,000\n<i></i>\n\n                  3\n00:00:05,000 --> 00:00:06,000\nreal\n";
    let loaded = load_srt(source);
    let report = loaded.document().report();
    assert_eq!(report.empty_cues(), 2);
    assert_eq!(report.skipped_cues(), 0);
    assert_eq!(loaded.document().events().len(), 1);
}

#[test]
fn overlapping_cues_are_both_active_in_appearance_order() {
    let source =
        "1\n00:00:01,000 --> 00:00:05,000\nlong\n\n2\n00:00:02,000 --> 00:00:03,000\nshort\n";
    let loaded = load_srt(source);
    assert_eq!(active_texts_at(&loaded, 2_500), ["long", "short"]);
    assert_eq!(active_texts_at(&loaded, 4_000), ["long"]);
}

#[test]
fn issue_list_is_capped_but_skipped_count_is_exact() {
    let mut source = String::new();
    for _ in 0..5 {
        source.push_str("1\nnot a timing\n\n");
    }
    let limits = subtitle_formats::SubtitleParseLimits::new(1 << 20, 100, 2);
    let loaded = try_load_srt(
        source.as_bytes(),
        subtitle_formats::LegacyEncodingHint::Generic,
        &limits,
    )
    .unwrap();
    let report = loaded.document().report();
    assert_eq!(report.skipped_cues(), 5);
    assert_eq!(report.issues().len(), 2);
    assert_eq!(report.unreported_issues(), 3);
}

#[test]
fn markup_italic_bold_color_and_anchor_survive_into_the_model() {
    let source = "1\n00:00:01,000 --> 00:00:02,000\n                  {\\an8}<i>slanted</i> <b>heavy</b> <font color=\"#ff0000\">red</font><u>x</u>\n";
    let loaded = load_srt(source);
    let cue = only_text_cue(&loaded);
    assert_eq!(cue.placement.anchor, Some(ScreenAnchor::TopCenter));
    assert_eq!(cue.text.plain_text(), "slanted heavy redx");
    let styled = |needle: &str| {
        cue.text
            .spans()
            .iter()
            .find(|span| span.content == SpanContent::Text(needle.to_owned()))
            .unwrap_or_else(|| panic!("no span {needle}"))
            .style
    };
    assert_eq!(styled("slanted").slant, FontSlant::Italic);
    assert_eq!(styled("slanted").weight, FontWeight::Normal);
    assert_eq!(styled("heavy").weight, FontWeight::Bold);
    assert_eq!(styled("red").color, Some(SubtitleColor::opaque(255, 0, 0)));
    assert_eq!(styled("x").underline, TextUnderline::Single);
    assert_eq!(styled(" "), SpanStyle::default());
}

#[test]
fn sniffing_recognizes_srt_with_and_without_numbering_but_not_prose() {
    assert_eq!(
        sniff_text_subtitle_format(TWO_CUES),
        Some(TextSubtitleFormat::Srt)
    );
    assert_eq!(
        sniff_text_subtitle_format("\n\n00:00:01.000 --> 00:00:02.000\nx"),
        Some(TextSubtitleFormat::Srt)
    );
    assert_eq!(sniff_text_subtitle_format("hello\nworld\n"), None);
    assert_eq!(sniff_text_subtitle_format(""), None);
    assert_eq!(
        sniff_text_subtitle_format("1\n2\n00:00:01,000 --> 00:00:02,000\nx"),
        None
    );
}

#[test]
fn first_event_text_matches_plain_text_helper() {
    let loaded = load_srt(TWO_CUES);
    assert_eq!(plain_text_of(&loaded.document().events()[1]), "Second");
}
