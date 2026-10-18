#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Сквозные тесты WebVTT: байты -> `load_text_subtitle` -> `SubtitleTimeline` -> активные реплики.

mod common;

use common::{ROOMY_LIMITS, active_texts_at, try_load_text};
use subtitle_core::{
    CueLine, CueLineAlign, CuePosition, CuePositionAlign, CueTextAlign, CueWritingDirection,
    FontSlant, FontWeight, PercentOfVideo, RegionScroll, SpanContent, SubtitleColor,
    SubtitleContent, SubtitleTextCue, SubtitleTime, TextSpan, TextUnderline,
};
use subtitle_formats::{
    CueSettingName, DetectedSubtitleEncoding, LegacyEncodingHint, LoadedTextSubtitle,
    ParseIssueKind, SubtitleLoadError, SubtitleParseLimits, TextSubtitleFormat,
    sniff_text_subtitle_format,
};

fn try_load_vtt(
    text: &str,
    limits: &SubtitleParseLimits,
) -> Result<LoadedTextSubtitle, SubtitleLoadError> {
    try_load_text(
        TextSubtitleFormat::WebVtt,
        text.as_bytes(),
        LegacyEncodingHint::Generic,
        limits,
    )
}

fn load_vtt(text: &str) -> LoadedTextSubtitle {
    try_load_vtt(text, &ROOMY_LIMITS).unwrap()
}

fn cue_at(loaded: &LoadedTextSubtitle, index: usize) -> &SubtitleTextCue {
    match loaded.document().events().get(index).unwrap().content() {
        SubtitleContent::Text(cue) => cue,
        _ => panic!("expected text cue"),
    }
}

fn span_text(span: &TextSpan) -> &str {
    match &span.content {
        SpanContent::Text(text) => text,
        SpanContent::LineBreak => "\n",
    }
}

fn percent(value: f32) -> PercentOfVideo {
    PercentOfVideo::new(value).unwrap()
}

fn red() -> SubtitleColor {
    SubtitleColor::opaque(255, 0, 0)
}

const TWO_CUES: &str =
    "WEBVTT\n\n1\n00:01.000 --> 00:03.000\nFirst\n\n00:00:05.000 --> 00:00:06.500\nSecond\n";

#[test]
fn cue_is_active_exactly_inside_half_open_interval() {
    let loaded = load_vtt(TWO_CUES);
    assert_eq!(loaded.document().events().len(), 2);
    assert!(active_texts_at(&loaded, 999).is_empty());
    assert_eq!(active_texts_at(&loaded, 1_000), ["First"]);
    assert_eq!(active_texts_at(&loaded, 2_999), ["First"]);
    assert!(active_texts_at(&loaded, 3_000).is_empty());
    assert_eq!(active_texts_at(&loaded, 5_000), ["Second"]);
    assert!(active_texts_at(&loaded, 6_500).is_empty());
}

#[test]
fn crlf_cr_bom_and_header_text_do_not_change_the_result() {
    let with_header = TWO_CUES.replacen("WEBVTT", "WEBVTT - my movie\nKind: captions", 1);
    let crlf = TWO_CUES.replace('\n', "\r\n");
    let old_mac = TWO_CUES.replace('\n', "\r");
    let bom_bytes: Vec<u8> = [&[0xEF, 0xBB, 0xBF][..], TWO_CUES.as_bytes()].concat();
    for source in [with_header, crlf, old_mac] {
        let loaded = load_vtt(&source);
        assert_eq!(active_texts_at(&loaded, 1_500), ["First"]);
        assert_eq!(active_texts_at(&loaded, 5_500), ["Second"]);
        assert_eq!(loaded.document().report().skipped_cues(), 0);
    }
    let loaded = try_load_text(
        TextSubtitleFormat::WebVtt,
        &bom_bytes,
        LegacyEncodingHint::Generic,
        &ROOMY_LIMITS,
    )
    .unwrap();
    assert_eq!(loaded.encoding(), DetectedSubtitleEncoding::Utf8WithBom);
    assert_eq!(active_texts_at(&loaded, 1_500), ["First"]);
}

#[test]
fn missing_signature_is_not_webvtt_and_signature_needs_boundary() {
    for bad in [
        "00:01.000 --> 00:02.000\nx\n",
        "WEBVTTX\n\n00:01.000 --> 00:02.000\nx\n",
        "",
    ] {
        assert_eq!(
            try_load_vtt(bad, &ROOMY_LIMITS).unwrap_err(),
            SubtitleLoadError::NotWebVtt
        );
    }
}

#[test]
fn sniffing_recognizes_webvtt_before_srt() {
    assert_eq!(
        sniff_text_subtitle_format(TWO_CUES),
        Some(TextSubtitleFormat::WebVtt)
    );
    assert_eq!(
        sniff_text_subtitle_format("WEBVTT"),
        Some(TextSubtitleFormat::WebVtt)
    );
    assert_eq!(
        sniff_text_subtitle_format("WEBVTT\t-\n\n"),
        Some(TextSubtitleFormat::WebVtt)
    );
    assert_eq!(sniff_text_subtitle_format("WEBVTTX\nhello"), None);
    assert_eq!(
        sniff_text_subtitle_format("1\n00:00:01,000 --> 00:00:02,000\nx\n"),
        Some(TextSubtitleFormat::Srt)
    );
}

#[test]
fn cue_settings_become_expected_placement() {
    let loaded = load_vtt(
        "WEBVTT\n\n00:01.000 --> 00:02.000 vertical:rl line:10%,end position:30%,line-right size:40% align:left\nx\n\n\
         00:03.000 --> 00:04.000 line:-1 align:start\ny\n",
    );
    let first = &cue_at(&loaded, 0).placement;
    assert_eq!(
        first.writing_direction,
        CueWritingDirection::VerticalRightToLeft
    );
    assert_eq!(first.line, CueLine::Percent(percent(10.0)));
    assert_eq!(first.line_align, CueLineAlign::End);
    assert_eq!(first.position, CuePosition::Percent(percent(30.0)));
    assert_eq!(first.position_align, CuePositionAlign::LineRight);
    assert_eq!(first.size, percent(40.0));
    assert_eq!(first.text_align, CueTextAlign::Left);
    let second = &cue_at(&loaded, 1).placement;
    assert_eq!(second.line, CueLine::LineNumber(-1));
    assert_eq!(second.text_align, CueTextAlign::Start);
    assert_eq!(second.size, PercentOfVideo::FULL);
    assert!(loaded.document().report().issues().is_empty());
}

#[test]
fn invalid_setting_is_ignored_with_issue_and_cue_survives() {
    let loaded = load_vtt(
        "WEBVTT\n\nNOTE keep line numbers\n\n00:01.000 --> 00:02.000 size:300% align:end bogus:1\nhello\n",
    );
    assert_eq!(active_texts_at(&loaded, 1_500), ["hello"]);
    let cue = cue_at(&loaded, 0);
    assert_eq!(cue.placement.size, PercentOfVideo::FULL);
    assert_eq!(cue.placement.text_align, CueTextAlign::End);
    let report = loaded.document().report();
    assert_eq!(report.skipped_cues(), 0);
    let issues: Vec<_> = report
        .issues()
        .iter()
        .map(|issue| (issue.line_number.get(), issue.kind.clone()))
        .collect();
    assert_eq!(
        issues,
        [
            (
                5,
                ParseIssueKind::InvalidCueSetting {
                    name: CueSettingName::Size
                }
            ),
            (
                5,
                ParseIssueKind::InvalidCueSetting {
                    name: CueSettingName::Unknown
                }
            ),
        ]
    );
}

#[test]
fn tags_classes_voice_language_and_ruby_become_spans() {
    let loaded = load_vtt(
        "WEBVTT\n\n00:01.000 --> 00:05.000\n\
         <v Bob><b>Bold</b> and <i>italic</i> <u>under</u></v>\n\
         <c.yellow.big>cls</c> <lang fr>bonjour</lang> <ruby>漢<rt>kan</rt></ruby>\n",
    );
    let spans = cue_at(&loaded, 0).text.spans();
    let by_text = |needle: &str| spans.iter().find(|span| span_text(span) == needle).unwrap();
    let bold = by_text("Bold");
    assert_eq!(bold.style.weight, FontWeight::Bold);
    assert_eq!(bold.voice.as_deref(), Some("Bob"));
    assert_eq!(by_text("italic").style.slant, FontSlant::Italic);
    assert_eq!(by_text("under").style.underline, TextUnderline::Single);
    assert_eq!(by_text("cls").voice, None);
    assert_eq!(by_text("bonjour").language.as_deref(), Some("fr"));
    let ruby = by_text("漢");
    assert_eq!(ruby.ruby_annotation.as_deref(), Some("kan"));
    assert!(spans.iter().all(|span| span_text(span) != "kan"));
    assert!(
        spans
            .iter()
            .any(|span| span.content == SpanContent::LineBreak)
    );
    assert_eq!(
        cue_at(&loaded, 0).text.plain_text(),
        "Bold and italic under\ncls bonjour 漢"
    );
}

#[test]
fn entities_decode_and_angle_brackets_without_tags_stay_literal() {
    let loaded =
        load_vtt("WEBVTT\n\n00:01.000 --> 00:02.000\nTom &amp; Jerry &lt;3 &#x41; a < b <i>ok\n");
    assert_eq!(
        cue_at(&loaded, 0).text.plain_text(),
        "Tom & Jerry <3 A a < b ok"
    );
    assert_eq!(
        cue_at(&loaded, 0).text.spans().last().unwrap().style.slant,
        FontSlant::Italic
    );
}

#[test]
fn style_block_colors_classes_base_cue_and_voice() {
    let loaded = load_vtt(
        "WEBVTT\n\nSTYLE\n/* comment */\n::cue { background-color: rgba(0,0,0,0.5); color: #fff }\n\
         ::cue(.red) { color: red !important }\n::cue(.em) { font-weight: bold; text-decoration: underline }\n\
         ::cue(v[voice=\"Bob\"]) { font-style: italic }\n\n\
         00:01.000 --> 00:02.000\nplain <c.red>hot</c> <c.em>strong</c> <v Bob>bob</v>\n",
    );
    let spans = cue_at(&loaded, 0).text.spans();
    let span_for = |needle: &str| {
        spans
            .iter()
            .find(|span| span_text(span).contains(needle))
            .unwrap()
    };
    let white = Some(SubtitleColor::opaque(255, 255, 255));
    let shade = Some(SubtitleColor {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 128,
    });
    let plain = span_for("plain");
    assert_eq!(plain.style.color, white);
    assert_eq!(plain.style.background, shade);
    assert_eq!(span_for("hot").style.color, Some(red()));
    assert_eq!(span_for("hot").style.background, shade);
    let strong = span_for("strong");
    assert_eq!(strong.style.weight, FontWeight::Bold);
    assert_eq!(strong.style.underline, TextUnderline::Single);
    assert_eq!(strong.style.color, white);
    assert_eq!(span_for("bob").style.slant, FontSlant::Italic);
    assert_eq!(loaded.document().report().ignored_style_rules(), 0);
}

#[test]
fn inline_tags_override_class_styles() {
    let loaded = load_vtt(
        "WEBVTT\n\nSTYLE\n::cue(.normal) { font-weight: normal }\n\n00:01.000 --> 00:02.000\n<b.normal>x</b>\n",
    );
    assert_eq!(
        cue_at(&loaded, 0).text.spans()[0].style.weight,
        FontWeight::Bold
    );
}

#[test]
fn unsupported_css_is_counted_and_never_fatal() {
    let loaded = load_vtt(
        "WEBVTT\n\nSTYLE\n::cue(b) { color: red }\n::cue(.ok) { color: lime; font-size: 2em; text-shadow: 0 0 2px }\n\
         broken {{{ \n\nSTYLE\n@media print { ::cue { color: red } }\n\n\
         00:01.000 --> 00:02.000\n<c.ok>x</c>\n",
    );
    assert_eq!(active_texts_at(&loaded, 1_500), ["x"]);
    assert_eq!(
        cue_at(&loaded, 0).text.spans()[0].style.color,
        Some(SubtitleColor::opaque(0, 255, 0))
    );
    // `::cue(b)`, font-size, text-shadow, `broken` + `@media`
    assert!(loaded.document().report().ignored_style_rules() >= 5);
    assert_eq!(loaded.document().report().skipped_cues(), 0);
}

#[test]
fn style_and_region_blocks_after_first_cue_are_ignored() {
    let loaded = load_vtt(
        "WEBVTT\n\n00:01.000 --> 00:02.000\nfirst\n\nSTYLE\n::cue { color: red }\n\nREGION\nid:late\n\n\
         00:03.000 --> 00:04.000 region:late\nsecond\n",
    );
    assert_eq!(cue_at(&loaded, 1).text.spans()[0].style.color, None);
    assert!(loaded.document().regions().is_empty());
    assert_eq!(cue_at(&loaded, 1).placement.region_id, None);
    assert_eq!(loaded.document().report().skipped_cues(), 0);
}

#[test]
fn region_is_parsed_and_referenced_by_cue() {
    let loaded = load_vtt(
        "WEBVTT\n\nREGION\nid:fred\nwidth:40%\nlines:3\nregionanchor:0%,100%\nviewportanchor:10%,90%\nscroll:up\n\n\
         00:01.000 --> 00:02.000 region:fred\nin region\n\n\
         00:03.000 --> 00:04.000 region:ghost\nunknown region\n",
    );
    let regions = loaded.document().regions();
    assert_eq!(regions.len(), 1);
    let region = &regions[0];
    assert_eq!(region.id, "fred");
    assert_eq!(region.width, percent(40.0));
    assert_eq!(region.lines, 3);
    assert_eq!(region.viewport_anchor.x, percent(10.0));
    assert_eq!(region.viewport_anchor.y, percent(90.0));
    assert_eq!(region.scroll, RegionScroll::Up);
    assert_eq!(
        cue_at(&loaded, 0).placement.region_id.as_deref(),
        Some("fred")
    );
    // Ссылка на не объявленный регион игнорируется, реплика остаётся.
    assert_eq!(cue_at(&loaded, 1).placement.region_id, None);
    assert_eq!(active_texts_at(&loaded, 3_500), ["unknown region"]);
    let issues = loaded.document().report().issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(
        issues[0].kind,
        ParseIssueKind::InvalidCueSetting {
            name: CueSettingName::Region
        }
    );
}

#[test]
fn later_region_with_same_id_wins_and_idless_region_is_reported() {
    let loaded = load_vtt(
        "WEBVTT\n\nREGION\nid:a\nwidth:10%\n\nREGION\nwidth:99%\n\nREGION\nid:a width:20% lines:oops\n\n\
         00:01.000 --> 00:02.000 region:a\nx\n",
    );
    let regions = loaded.document().regions();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].width, percent(20.0));
    assert_eq!(regions[0].lines, 3);
    let kinds: Vec<_> = loaded
        .document()
        .report()
        .issues()
        .iter()
        .map(|issue| issue.kind.clone())
        .collect();
    assert_eq!(
        kinds,
        [
            ParseIssueKind::RegionWithoutId,
            ParseIssueKind::InvalidRegionSetting
        ]
    );
}

#[test]
fn note_blocks_are_ignored_even_with_cue_like_text() {
    let loaded = load_vtt(
        "WEBVTT\n\nNOTE this is a comment\nsecond comment line\n\nNOTE\n\n\
         00:01.000 --> 00:02.000\nreal\n\nNOTES are not notes\n",
    );
    assert_eq!(loaded.document().events().len(), 1);
    assert_eq!(active_texts_at(&loaded, 1_500), ["real"]);
    // "NOTES ..." — не комментарий, а блок без времени: пропущенная реплика.
    assert_eq!(loaded.document().report().skipped_cues(), 1);
}

#[test]
fn karaoke_timestamps_set_reveal_and_out_of_range_ones_are_ignored() {
    let loaded = load_vtt(
        "WEBVTT\n\n00:01.000 --> 00:05.000\none <00:02.000>two <c.x>three</c><00:03.500> four <00:09.000>late <00:00:02,000>bad\n",
    );
    let spans = cue_at(&loaded, 0).text.spans();
    let reveal_of = |needle: &str| {
        spans
            .iter()
            .find(|span| span_text(span).contains(needle))
            .unwrap()
            .reveal_at
    };
    assert_eq!(reveal_of("one"), None);
    assert_eq!(reveal_of("two"), Some(SubtitleTime::from_millis(2_000)));
    assert_eq!(reveal_of("three"), Some(SubtitleTime::from_millis(2_000)));
    assert_eq!(reveal_of("four"), Some(SubtitleTime::from_millis(3_500)));
    // Метки за пределами реплики и битая запись не меняют момент показа.
    assert_eq!(reveal_of("late"), Some(SubtitleTime::from_millis(3_500)));
    assert_eq!(reveal_of("bad"), Some(SubtitleTime::from_millis(3_500)));
}

#[test]
fn broken_cues_are_skipped_and_counted_while_neighbours_survive() {
    let loaded = load_vtt(
        "WEBVTT\n\n00:01.000 --> 00:02.000\nA\n\n\
         00:03,000 --> 00:04.000\ncomma timestamps are invalid in WebVTT\n\n\
         00:05.000 --> 00:04.000\nend before start\n\n\
         00:06.000 --> 00:06.000\nzero length\n\n\
         id without timing\ntext\n\n\
         00:07.000 --> 00:08.000\n\n\
         00:09.000 --> 00:10.000\nZ\n",
    );
    assert_eq!(active_texts_at(&loaded, 1_500), ["A"]);
    assert_eq!(active_texts_at(&loaded, 9_500), ["Z"]);
    let report = loaded.document().report();
    assert_eq!(report.skipped_cues(), 4);
    assert_eq!(report.empty_cues(), 1);
    let kinds: Vec<_> = report
        .issues()
        .iter()
        .map(|issue| issue.kind.clone())
        .collect();
    assert_eq!(
        kinds,
        [
            ParseIssueKind::InvalidTimestamp,
            ParseIssueKind::EndNotAfterStart,
            ParseIssueKind::EndNotAfterStart,
            ParseIssueKind::MissingTimingLine,
        ]
    );
    assert_eq!(report.issues()[0].line_number.get(), 6);
}

#[test]
fn overlapping_cues_are_both_active() {
    let loaded =
        load_vtt("WEBVTT\n\n00:01.000 --> 00:04.000\nlong\n\n00:02.000 --> 00:03.000\nshort\n");
    assert_eq!(active_texts_at(&loaded, 2_500), ["long", "short"]);
    assert_eq!(active_texts_at(&loaded, 3_500), ["long"]);
}

#[test]
fn cues_without_blank_line_between_them_are_still_separated() {
    let loaded = load_vtt("WEBVTT\n\n00:01.000 --> 00:02.000\nA\n00:03.000 --> 00:04.000\nB\n");
    assert_eq!(active_texts_at(&loaded, 1_500), ["A"]);
    assert_eq!(active_texts_at(&loaded, 3_500), ["B"]);
}

#[test]
fn limits_apply_to_webvtt_like_srt() {
    let two_cues_limit = SubtitleParseLimits::new(1 << 20, 2, 10);
    assert_eq!(
        try_load_vtt(TWO_CUES, &two_cues_limit)
            .unwrap()
            .document()
            .events()
            .len(),
        2
    );
    let one_cue_limit = SubtitleParseLimits::new(1 << 20, 1, 10);
    assert_eq!(
        try_load_vtt(TWO_CUES, &one_cue_limit).unwrap_err(),
        SubtitleLoadError::TooManyEvents { limit: 1 }
    );
    let tiny_input = SubtitleParseLimits::new(10, 100, 10);
    assert!(matches!(
        try_load_vtt(TWO_CUES, &tiny_input),
        Err(SubtitleLoadError::InputTooLarge {
            limit_bytes: 10,
            ..
        })
    ));
}

#[test]
fn issue_list_is_capped_by_limit_but_counters_keep_counting() {
    let mut source = String::from("WEBVTT\n\n");
    for _ in 0..5 {
        source.push_str("00:05.000 --> 00:04.000\nbad\n\n");
    }
    let limits = SubtitleParseLimits::new(1 << 20, 100, 2);
    let loaded = try_load_vtt(&source, &limits).unwrap();
    let report = loaded.document().report();
    assert_eq!(report.skipped_cues(), 5);
    assert_eq!(report.issues().len(), 2);
    assert_eq!(report.unreported_issues(), 3);
}

#[test]
fn epoch_scale_hours_are_parsed_up_to_the_six_digit_hour_limit() {
    // Эпоха Unix (~1.7e9 с) = 472222 ч: шесть цифр часов (предел грамматики) покрывают её
    // и все даты примерно до 2084 года; сдвиг на шкалу плеера делается уже после разбора.
    let loaded = load_vtt("WEBVTT\n\n472222:13:20.250 --> 472222:13:20.750\nepoch\n");
    let seconds = 472_222_i64 * 3600 + 13 * 60 + 20;
    let event = loaded.document().events().first().unwrap();
    assert_eq!(event.start().as_micros(), seconds * 1_000_000 + 250_000);
    assert_eq!(event.end().as_micros(), seconds * 1_000_000 + 750_000);
}
