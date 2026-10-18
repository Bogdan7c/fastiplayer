#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Сквозные тесты srv3 (YouTube): байты -> `load_text_subtitle` -> `SubtitleTimeline` -> что на экране.
//! Фикстуры синтетические: повторяют структуру настоящих файлов YouTube, но с придуманным текстом.

mod common;

use common::{ROOMY_LIMITS, active_texts_at, try_load_text};
use subtitle_core::{
    CueLine, CueLineAlign, CuePosition, CuePositionAlign, CueTextAlign, FontWeight, SpanContent,
    SubtitleColor, SubtitleContent, SubtitleTextCue, SubtitleTime, SubtitleTimeline,
};
use subtitle_formats::{
    LegacyEncodingHint, LoadedTextSubtitle, SubtitleLoadError, SubtitleParseLimits,
    TextSubtitleFormat, sniff_text_subtitle_format,
};

fn try_load_srv3(text: &str) -> Result<LoadedTextSubtitle, SubtitleLoadError> {
    try_load_text(
        TextSubtitleFormat::Srv3,
        text.as_bytes(),
        LegacyEncodingHint::Generic,
        &ROOMY_LIMITS,
    )
}

fn load_srv3(text: &str) -> LoadedTextSubtitle {
    try_load_srv3(text).unwrap()
}

fn time(millis: i64) -> SubtitleTime {
    SubtitleTime::from_millis(millis)
}

/// Реплика, активная в момент `millis` (должна быть ровно одна).
fn single_cue_at(loaded: &LoadedTextSubtitle, millis: i64) -> SubtitleTextCue {
    let (timeline, _) = SubtitleTimeline::from_events(loaded.document().events().iter().cloned());
    let mut ids = Vec::new();
    timeline.collect_active_at(time(millis), &mut ids);
    assert_eq!(ids.len(), 1, "exactly one cue expected at {millis} ms");
    match timeline.event(ids[0]).unwrap().content() {
        SubtitleContent::Text(cue) => cue.clone(),
        _ => panic!("expected text cue"),
    }
}

/// Простой ручной файл: без `<head>`, текст с сущностями и литеральным переводом строки.
const MANUAL: &str = r#"<?xml version="1.0" encoding="utf-8" ?><timedtext format="3">
<body>
<p t="1000" d="2000">It&#39;s a test.</p>
<p t="4000" d="1500">first line
second line</p>
<p t="6000" d="1000">with<br/>break</p>
</body>
</timedtext>"#;

/// Автосубтитры: окно из двух строк, слова с `t`, служебные абзацы `a="1"`.
const ROLLING: &str = r#"<?xml version="1.0" encoding="utf-8" ?><timedtext format="3">
<head>
<ws id="0"/>
<ws id="1" mh="2" ju="0" sd="3"/>
<wp id="0"/>
<wp id="1" ap="6" ah="20" av="100" rc="2" cc="40"/>
</head>
<body>
<w t="0" id="1" wp="1" ws="1"/>
<p t="1000" d="4000" w="1"><s ac="255">good</s><s t="200" ac="255"> morning</s></p>
<p t="2400" d="800" w="1" a="1">
</p>
<p t="2500" d="4000" w="1"><s ac="255">how</s><s t="300" ac="255"> are</s><s t="900" ac="255"> you</s></p>
<p t="5400" d="800" w="1" a="1">
</p>
<p t="5500" d="3000" w="1"><s ac="255">fine</s><s t="400" ac="255"> thanks</s></p>
</body>
</timedtext>"#;

#[test]
fn manual_paragraphs_become_events_with_decoded_entities_and_line_breaks() {
    let loaded = load_srv3(MANUAL);
    assert_eq!(loaded.document().events().len(), 3);
    assert!(active_texts_at(&loaded, 999).is_empty());
    assert_eq!(active_texts_at(&loaded, 1_000), ["It's a test."]);
    assert_eq!(active_texts_at(&loaded, 2_999), ["It's a test."]);
    assert!(active_texts_at(&loaded, 3_000).is_empty());
    assert_eq!(active_texts_at(&loaded, 4_000), ["first line\nsecond line"]);
    assert_eq!(active_texts_at(&loaded, 6_500), ["with\nbreak"]);
    assert_eq!(loaded.document().report().skipped_cues(), 0);
}

#[test]
fn manual_paragraph_uses_default_bottom_placement() {
    let cue = single_cue_at(&load_srv3(MANUAL), 1_500);
    assert_eq!(cue.placement.line, CueLine::Auto);
    assert_eq!(cue.placement.position, CuePosition::Auto);
    assert_eq!(cue.placement.text_align, CueTextAlign::Center);
}

#[test]
fn rolling_window_shows_the_last_two_lines_with_the_first_fully_revealed() {
    let loaded = load_srv3(ROLLING);
    // Один абзац: только его строка.
    assert_eq!(active_texts_at(&loaded, 1_100), ["good morning"]);
    // Оба абзаца активны: обе строки, новая снизу.
    assert_eq!(
        active_texts_at(&loaded, 2_500),
        ["good morning\nhow are you"]
    );
    assert_eq!(
        active_texts_at(&loaded, 4_999),
        ["good morning\nhow are you"]
    );
    // Первый абзац кончился на 5000, второй ещё идёт, третий начался на 5500.
    assert_eq!(active_texts_at(&loaded, 5_000), ["how are you"]);
    assert_eq!(
        active_texts_at(&loaded, 5_500),
        ["how are you\nfine thanks"]
    );
    // Второй кончился на 6500: остаётся последний.
    assert_eq!(active_texts_at(&loaded, 7_000), ["fine thanks"]);
    assert!(active_texts_at(&loaded, 8_500).is_empty());
}

#[test]
fn rolling_window_keeps_only_rc_last_lines() {
    let three_overlapping = ROLLING.replace(
        r#"<p t="5500" d="3000" w="1">"#,
        r#"<p t="3000" d="3000" w="1">"#,
    );
    let loaded = load_srv3(&three_overlapping);
    // В 3500 активны три абзаца, окно вмещает две строки: первая ушла вверх.
    assert_eq!(
        active_texts_at(&loaded, 3_500),
        ["how are you\nfine thanks"]
    );
}

#[test]
fn word_reveal_times_are_absolute_and_already_shown_words_have_none() {
    let loaded = load_srv3(ROLLING);
    // Отрезок [2500, 5000): первая строка уже произнесена целиком, у второй слова появятся.
    let cue = single_cue_at(&loaded, 2_500);
    let words: Vec<(String, Option<SubtitleTime>)> = cue
        .text
        .spans()
        .iter()
        .map(|span| {
            let text = match &span.content {
                SpanContent::Text(text) => text.clone(),
                SpanContent::LineBreak => "\n".to_owned(),
            };
            (text, span.reveal_at)
        })
        .collect();
    assert_eq!(
        words,
        vec![
            ("good".to_owned(), None),
            (" morning".to_owned(), None),
            // Перевод строки появляется вместе с первым словом новой строки (то есть сразу).
            ("\n".to_owned(), None),
            ("how".to_owned(), None),
            (" are".to_owned(), Some(time(2_800))),
            (" you".to_owned(), Some(time(3_400))),
        ]
    );
    // Самый первый отрезок: «morning» появится через 200 мс после начала абзаца.
    let first = single_cue_at(&loaded, 1_000);
    assert_eq!(first.text.spans()[1].reveal_at, Some(time(1_200)));
}

#[test]
fn timeline_reports_each_word_reveal_as_a_change_point() {
    let loaded = load_srv3(ROLLING);
    let (timeline, _) = SubtitleTimeline::from_events(loaded.document().events().iter().cloned());
    assert_eq!(timeline.next_change_after(time(1_000)), Some(time(1_200)));
    assert_eq!(timeline.next_change_after(time(1_200)), Some(time(2_500)));
    assert_eq!(timeline.next_change_after(time(2_500)), Some(time(2_800)));
    assert_eq!(timeline.next_change_after(time(2_800)), Some(time(3_400)));
}

#[test]
fn window_position_and_justification_become_cue_placement() {
    let cue = single_cue_at(&load_srv3(ROLLING), 1_100);
    // ap=6: левый нижний угол; ah=20, av=100; ju=0: текст слева.
    assert_eq!(cue.placement.position_align, CuePositionAlign::LineLeft);
    assert_eq!(cue.placement.line_align, CueLineAlign::End);
    assert_eq!(cue.placement.text_align, CueTextAlign::Left);
    match (cue.placement.position, cue.placement.line) {
        (CuePosition::Percent(horizontal), CueLine::Percent(vertical)) => {
            assert_eq!(horizontal.get(), 20.0);
            assert_eq!(vertical.get(), 100.0);
        }
        other => panic!("explicit percent placement expected, got {other:?}"),
    }
}

#[test]
fn append_paragraphs_are_not_reported_as_empty_cues_but_blank_ones_are() {
    let loaded = load_srv3(ROLLING);
    assert_eq!(loaded.document().report().empty_cues(), 0);
    let with_blank = MANUAL.replace("</body>", r#"<p t="9000" d="500">  </p></body>"#);
    let loaded = load_srv3(&with_blank);
    assert_eq!(loaded.document().report().empty_cues(), 1);
    assert_eq!(loaded.document().events().len(), 3);
}

/// Стилизованный файл в духе YTSubConverter: пера, подкладка из U+200B и пробелов, дубликат
/// абзаца для обводки.
const STYLED: &str = "<?xml version=\"1.0\" encoding=\"utf-8\" ?><timedtext format=\"3\">\n<head>\n\
<pen id=\"1\" fc=\"#000000\" fo=\"0\" bo=\"0\" sz=\"100\"/>\n\
<pen id=\"3\" b=\"1\" fc=\"#FFD93D\" fo=\"254\" bo=\"0\" ec=\"#000000\" sz=\"100\" et=\"4\"/>\n\
<pen id=\"4\" b=\"1\" fc=\"#FFD93D\" fo=\"254\" bo=\"0\" ec=\"#000000\" sz=\"100\" et=\"3\"/>\n\
<ws id=\"1\" ju=\"2\" pd=\"0\" sd=\"0\"/>\n<wp id=\"1\" ap=\"7\" ah=\"50\" av=\"98\"/>\n</head>\n<body>\n\
<p t=\"1000\" d=\"1500\" wp=\"1\" ws=\"1\"><s p=\"1\">\u{200B}</s><s p=\"3\">\u{200B} \u{200B}- hello \u{200B} \u{200B}</s><s p=\"1\">\u{200B}</s></p>\n\
<p t=\"1000\" d=\"1500\" wp=\"1\" ws=\"1\"><s p=\"1\">\u{200B}</s><s p=\"4\">\u{200B} \u{200B}- hello \u{200B} \u{200B}</s><s p=\"1\">\u{200B}</s></p>\n\
</body>\n</timedtext>";

#[test]
fn pens_map_to_span_style_and_zero_width_padding_is_trimmed() {
    let loaded = load_srv3(STYLED);
    let cue = single_cue_at(&loaded, 1_200);
    assert_eq!(cue.text.plain_text(), "- hello");
    let span = &cue.text.spans()[0];
    assert_eq!(span.style.weight, FontWeight::Bold);
    assert_eq!(
        span.style.color,
        Some(SubtitleColor::opaque(0xFF, 0xD9, 0x3D)),
        "the author's color is preserved"
    );
}

#[test]
fn duplicate_outline_paragraph_is_dropped_and_counted_as_ignored_style() {
    let loaded = load_srv3(STYLED);
    assert_eq!(loaded.document().events().len(), 1);
    // Дубликат + перья с обводкой/размером (et/ec) + ws с pd/sd: всё посчитано, ничего не фатально.
    assert!(loaded.document().report().ignored_style_rules() >= 1);
    assert_eq!(loaded.document().report().skipped_cues(), 0);
}

#[test]
fn explicit_position_near_the_bottom_is_a_percent_line() {
    let cue = single_cue_at(&load_srv3(STYLED), 1_200);
    match cue.placement.line {
        CueLine::Percent(vertical) => assert_eq!(vertical.get(), 98.0),
        other => panic!("av=98 must become an explicit line, got {other:?}"),
    }
    assert_eq!(cue.placement.text_align, CueTextAlign::Center);
}

#[test]
fn missing_duration_lasts_until_the_next_paragraph_of_the_same_window() {
    let loaded = load_srv3(
        r#"<timedtext><body>
<p t="1000">one</p>
<p t="3000" d="500">two</p>
</body></timedtext>"#,
    );
    // Атрибута `format` нет — файл принят.
    assert_eq!(active_texts_at(&loaded, 2_999), ["one"]);
    assert_eq!(active_texts_at(&loaded, 3_000), ["two"]);
    assert!(active_texts_at(&loaded, 3_500).is_empty());
}

#[test]
fn last_paragraph_without_duration_gets_a_bounded_default() {
    let loaded =
        load_srv3(r#"<timedtext format="3"><body><p t="1000">solo</p></body></timedtext>"#);
    assert_eq!(active_texts_at(&loaded, 5_999), ["solo"]);
    assert!(active_texts_at(&loaded, 6_000).is_empty());
}

#[test]
fn bad_paragraph_timing_is_reported_and_other_paragraphs_survive() {
    let loaded = load_srv3(
        r#"<timedtext format="3"><body>
<p t="abc" d="1000">no start</p>
<p t="2000" d="0">zero length</p>
<p t="3000" d="1000">good</p>
</body></timedtext>"#,
    );
    assert_eq!(loaded.document().events().len(), 1);
    assert_eq!(loaded.document().report().skipped_cues(), 2);
    assert_eq!(active_texts_at(&loaded, 3_500), ["good"]);
}

#[test]
fn malformed_xml_is_a_typed_fatal_error_without_content_echo() {
    let error =
        try_load_srv3(r#"<timedtext format="3"><body><p t="1" d="2">oops</body></timedtext>"#)
            .unwrap_err();
    assert_eq!(error, SubtitleLoadError::MalformedSrv3);
    assert!(!error.to_string().contains("oops"));
}

#[test]
fn doctype_and_custom_entities_are_rejected() {
    let doctype = r#"<!DOCTYPE timedtext [<!ENTITY x "y">]><timedtext format="3"><body><p t="1" d="2">&x;</p></body></timedtext>"#;
    assert_eq!(
        try_load_srv3(doctype).unwrap_err(),
        SubtitleLoadError::MalformedSrv3
    );
}

#[test]
fn other_xml_and_other_timedtext_versions_are_not_srv3() {
    assert_eq!(
        try_load_srv3("<tt><body/></tt>").unwrap_err(),
        SubtitleLoadError::NotSrv3
    );
    assert_eq!(
        try_load_srv3(r#"<timedtext format="2"><body/></timedtext>"#).unwrap_err(),
        SubtitleLoadError::NotSrv3
    );
}

#[test]
fn deeply_nested_xml_hits_the_structure_limit() {
    let nested = format!(
        r#"<timedtext format="3"><body>{}</body></timedtext>"#,
        "<x>".repeat(64) + &"</x>".repeat(64)
    );
    assert_eq!(
        try_load_srv3(&nested).unwrap_err(),
        SubtitleLoadError::Srv3TooComplex
    );
}

#[test]
fn event_and_size_limits_are_enforced() {
    let two_events_limit = SubtitleParseLimits::new(1 << 20, 2, 100);
    let error = try_load_text(
        TextSubtitleFormat::Srv3,
        MANUAL.as_bytes(),
        LegacyEncodingHint::Generic,
        &two_events_limit,
    )
    .unwrap_err();
    assert_eq!(error, SubtitleLoadError::TooManyEvents { limit: 2 });

    let tiny = SubtitleParseLimits::new(16, 100, 100);
    let error = try_load_text(
        TextSubtitleFormat::Srv3,
        MANUAL.as_bytes(),
        LegacyEncodingHint::Generic,
        &tiny,
    )
    .unwrap_err();
    assert!(matches!(error, SubtitleLoadError::InputTooLarge { .. }));
}

#[test]
fn rolling_window_event_count_respects_the_event_limit() {
    let limit = SubtitleParseLimits::new(1 << 20, 3, 100);
    let error = try_load_text(
        TextSubtitleFormat::Srv3,
        ROLLING.as_bytes(),
        LegacyEncodingHint::Generic,
        &limit,
    )
    .unwrap_err();
    assert_eq!(error, SubtitleLoadError::TooManyEvents { limit: 3 });
}

#[test]
fn sniff_detects_srv3_with_and_without_declaration_or_bom() {
    let detect = |text: &str| sniff_text_subtitle_format(text);
    assert_eq!(detect(MANUAL), Some(TextSubtitleFormat::Srv3));
    assert_eq!(
        detect(r#"<timedtext format="3"><body/></timedtext>"#),
        Some(TextSubtitleFormat::Srv3)
    );
    assert_eq!(
        detect("\u{feff}  \n<?xml version=\"1.0\"?><!-- c --><timedtext>"),
        Some(TextSubtitleFormat::Srv3)
    );
    // Не принимаем другие корни и похожие имена.
    assert_eq!(detect("<timedtextual/>"), None);
    assert_eq!(detect("<tt xmlns=\"http://www.w3.org/ns/ttml\"/>"), None);
    // SRT по-прежнему определяется как SRT.
    assert_eq!(
        detect("1\n00:00:01,000 --> 00:00:02,000\nHi\n"),
        Some(TextSubtitleFormat::Srt)
    );
}

#[test]
fn utf8_bom_in_file_is_accepted() {
    let with_bom = format!("\u{feff}{MANUAL}");
    let loaded = try_load_text(
        TextSubtitleFormat::Srv3,
        with_bom.as_bytes(),
        LegacyEncodingHint::Generic,
        &ROOMY_LIMITS,
    )
    .unwrap();
    assert_eq!(loaded.document().events().len(), 3);
}
