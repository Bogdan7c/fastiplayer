#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Линейное время разбора разметки SRT/WebVTT в одной огромной реплике.

mod common;

use std::time::{Duration, Instant};

use common::{ROOMY_LIMITS, active_texts_at, try_load_text};
use subtitle_formats::{
    LegacyEncodingHint, LoadedTextSubtitle, SubtitleParseLimits, TextSubtitleFormat,
};

/// Граница времени разбора огромной реплики: линейный код укладывается в доли секунды,
/// квадратичный на этих размерах работает на порядки дольше.
const HUGE_CUE_PARSE_TIME_BOUND: Duration = Duration::from_secs(2);

/// Лимит входа для «огромных» реплик (мегабайты разметки).
const HUGE_INPUT_LIMITS: SubtitleParseLimits = SubtitleParseLimits::new(1 << 24, 10_000, 100);

fn load_as(
    format: TextSubtitleFormat,
    bytes: &[u8],
    limits: &SubtitleParseLimits,
) -> LoadedTextSubtitle {
    try_load_text(format, bytes, LegacyEncodingHint::Generic, limits).unwrap()
}

/// Грузит и проверяет, что огромная реплика разобрана быстро; возвращает текст реплики.
fn load_huge_cue_within_bound(format: TextSubtitleFormat, bytes: &[u8]) -> String {
    let started = Instant::now();
    let loaded = load_as(format, bytes, &HUGE_INPUT_LIMITS);
    let elapsed = started.elapsed();
    assert!(
        elapsed < HUGE_CUE_PARSE_TIME_BOUND,
        "huge cue took {elapsed:?}"
    );
    let mut texts = active_texts_at(&loaded, 2_000);
    assert_eq!(texts.len(), 1);
    texts.remove(0)
}

#[test]
fn srt_cue_made_of_lone_angle_brackets_parses_in_linear_time_and_keeps_text() {
    const BRACKETS: usize = 500_000;
    let mut text = String::from("1\n00:00:01,000 --> 00:00:03,000\n");
    text.push_str(&"<".repeat(BRACKETS));
    let cue_text = load_huge_cue_within_bound(TextSubtitleFormat::Srt, text.as_bytes());
    assert_eq!(cue_text, "<".repeat(BRACKETS));
}

#[test]
fn srt_cue_made_of_unclosed_braces_parses_in_linear_time_and_keeps_text() {
    const BRACES: usize = 500_000;
    let mut text = String::from("1\n00:00:01,000 --> 00:00:03,000\n");
    text.push_str(&"{".repeat(BRACES));
    let cue_text = load_huge_cue_within_bound(TextSubtitleFormat::Srt, text.as_bytes());
    assert_eq!(cue_text, "{".repeat(BRACES));
}

#[test]
fn webvtt_cue_made_of_unclosed_class_tags_parses_in_linear_time_and_keeps_text() {
    const REPEATS: usize = 400_000;
    let mut text = String::from("WEBVTT\n\n00:00:01.000 --> 00:00:03.000\n");
    text.push_str(&"<c.a>x".repeat(REPEATS));
    let cue_text = load_huge_cue_within_bound(TextSubtitleFormat::WebVtt, text.as_bytes());
    assert_eq!(cue_text, "x".repeat(REPEATS));
}

#[test]
fn webvtt_cue_made_of_lone_angle_brackets_parses_in_linear_time_and_keeps_text() {
    const BRACKETS: usize = 500_000;
    let mut text = String::from("WEBVTT\n\n00:00:01.000 --> 00:00:03.000\n");
    text.push_str(&"<".repeat(BRACKETS));
    let cue_text = load_huge_cue_within_bound(TextSubtitleFormat::WebVtt, text.as_bytes());
    assert_eq!(cue_text, "<".repeat(BRACKETS));
}

/// Однопроходный поиск закрывающего `>` не меняет смысла обычной разметки: теги вырезаются,
/// одиночные `<` и `>` в тексте остаются символами.
#[test]
fn ordinary_tags_and_lone_brackets_keep_their_meaning() {
    let srt = "1\n00:00:01,000 --> 00:00:03,000\na <b>x</b> 1 < 2 > 0 <i\n";
    let loaded = load_as(TextSubtitleFormat::Srt, srt.as_bytes(), &ROOMY_LIMITS);
    assert_eq!(
        active_texts_at(&loaded, 2_000),
        vec!["a x 1 < 2 > 0 <i".to_owned()]
    );

    let vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\na <c.k>x</c> 1 < 2 <b\n";
    let loaded = load_as(TextSubtitleFormat::WebVtt, vtt.as_bytes(), &ROOMY_LIMITS);
    assert_eq!(
        active_texts_at(&loaded, 2_000),
        vec!["a x 1 < 2 <b".to_owned()]
    );
}
