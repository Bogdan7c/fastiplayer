#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Устойчивость к NUL-мусору: в SRT/WebVTT NUL удаляется, обрезка ASS по NUL видна в отчёте.

mod common;

use common::{ROOMY_LIMITS, active_texts_at, try_load_text};
use subtitle_formats::{
    AssParseLimits, LegacyEncodingHint, LoadedTextSubtitle, ParseIssueKind, SubtitleParseLimits,
    TextSubtitleFormat,
};

/// Сколько NUL-байтов дописывают к файлу некоторые редакторы (добивка до размера блока).
const NUL_PADDING_BYTES: usize = 4096;

fn load_as(
    format: TextSubtitleFormat,
    bytes: &[u8],
    limits: &SubtitleParseLimits,
) -> LoadedTextSubtitle {
    try_load_text(format, bytes, LegacyEncodingHint::Generic, limits).unwrap()
}

fn with_nul_padding(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.extend(std::iter::repeat_n(0u8, NUL_PADDING_BYTES));
    bytes
}

#[test]
fn srt_trailing_nul_padding_does_not_stick_to_the_last_cue() {
    let bytes = with_nul_padding("1\n00:00:01,000 --> 00:00:03,000\nHi");
    let loaded = load_as(TextSubtitleFormat::Srt, &bytes, &ROOMY_LIMITS);
    assert_eq!(active_texts_at(&loaded, 2_000), ["Hi"]);
}

#[test]
fn webvtt_trailing_nul_padding_does_not_stick_to_the_last_cue() {
    let bytes = with_nul_padding("WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nHi");
    let loaded = load_as(TextSubtitleFormat::WebVtt, &bytes, &ROOMY_LIMITS);
    assert_eq!(active_texts_at(&loaded, 2_000), ["Hi"]);
}

#[test]
fn nul_inside_a_cue_line_is_removed_and_line_numbers_stay_correct() {
    // Вторая реплика битая (блок начинается на строке 5): её номер в отчёте не должен сдвинуться от удаления NUL.
    let bytes = b"1\n00:00:01,000 --> 00:00:03,000\nH\0el\0lo\n\n2\nnot a timing line\ntext\n";
    let loaded = load_as(TextSubtitleFormat::Srt, bytes, &ROOMY_LIMITS);
    assert_eq!(active_texts_at(&loaded, 2_000), ["Hello"]);
    let issues = loaded.document().report().issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].line_number.get(), 5);
    assert_eq!(issues[0].kind, ParseIssueKind::MissingTimingLine);
}

#[test]
fn ass_truncation_at_nul_is_visible_in_the_report_with_its_line() {
    let ass_limits = AssParseLimits {
        max_header_bytes: 1 << 16,
        max_styles: 32,
        max_override_tags_per_event: 64,
        max_tracked_unknown_tag_names: 8,
    };
    let limits = ROOMY_LIMITS.with_ass_limits(ass_limits);
    let text = "[Events]\n\
        Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\r\n\
        Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,kept\n\0\
        Dialogue: 0,0:00:03.00,0:00:04.00,Default,,0,0,0,,lost\n";
    let loaded = load_as(TextSubtitleFormat::Ass, text.as_bytes(), &limits);
    assert_eq!(loaded.document().events().len(), 1);
    let issues = loaded.document().report().issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].kind, ParseIssueKind::TruncatedAtNul);
    // NUL стоит на строке 4 (после `\n` в конце строки с `kept`).
    assert_eq!(issues[0].line_number.get(), 4);
    // Обрезка не считается пропущенной репликой.
    assert_eq!(loaded.document().report().skipped_cues(), 0);
}
