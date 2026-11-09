#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Сквозные тесты json3 (YouTube): байты -> `load_text_subtitle` -> `SubtitleTimeline` -> что на экране.
//! Главное доказательство — эквивалентность с srv3: одинаковый смысл в двух записях даёт
//! одинаковые события. Фикстуры синтетические, с придуманным текстом.

mod common;

use common::{ROOMY_LIMITS, active_texts_at, try_load_text};
use subtitle_core::{
    SpanContent, SubtitleColor, SubtitleContent, SubtitleTextCue, SubtitleTime, SubtitleTimeline,
};
use subtitle_formats::{
    LegacyEncodingHint, LoadedTextSubtitle, SubtitleLoadError, SubtitleParseLimits,
    TextSubtitleFormat, sniff_text_subtitle_format,
};

fn try_load_with(
    format: TextSubtitleFormat,
    text: &str,
    limits: &SubtitleParseLimits,
) -> Result<LoadedTextSubtitle, SubtitleLoadError> {
    try_load_text(format, text.as_bytes(), LegacyEncodingHint::Generic, limits)
}

fn try_load_json3(text: &str) -> Result<LoadedTextSubtitle, SubtitleLoadError> {
    try_load_with(TextSubtitleFormat::Json3, text, &ROOMY_LIMITS)
}

fn load_json3(text: &str) -> LoadedTextSubtitle {
    try_load_json3(text).unwrap()
}

fn load_srv3(text: &str) -> LoadedTextSubtitle {
    try_load_with(TextSubtitleFormat::Srv3, text, &ROOMY_LIMITS).unwrap()
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

/// (текст, момент появления) каждого слова реплики в момент `millis`.
fn words_at(loaded: &LoadedTextSubtitle, millis: i64) -> Vec<(String, Option<SubtitleTime>)> {
    single_cue_at(loaded, millis)
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
        .collect()
}

/// Ручные субтитры с перьями и позицией окна + автосубтитры (бегущее окно из двух строк,
/// слова со смещениями, служебные абзацы-добавления) — в записи srv3.
const SRV3_RICH: &str = r##"<?xml version="1.0" encoding="utf-8" ?><timedtext format="3">
<head>
<pen id="0"/>
<pen id="1" b="1" fc="#FFD93D" fo="254" bc="#000000" bo="128"/>
<pen id="2" i="1" et="3"/>
<ws id="0"/>
<ws id="1" mh="2" ju="0" sd="3"/>
<wp id="0"/>
<wp id="1" ap="6" ah="20" av="100" rc="2" cc="40"/>
</head>
<body>
<w t="0" id="1" wp="1" ws="1"/>
<p t="100" d="900" wp="1" ws="1" p="1">Manual<br/>two</p>
<p t="300" d="500" p="2">Second</p>
<p t="1000" d="4000" w="1"><s ac="255">good</s><s t="200" ac="255"> morning</s></p>
<p t="2400" d="800" w="1" a="1">
</p>
<p t="2500" d="4000" w="1"><s ac="255">how</s><s t="300" ac="255"> are</s><s t="900" p="1" ac="255"> you</s></p>
<p t="5400" d="800" w="1" a="1">
</p>
<p t="5500" d="3000" w="1"><s ac="255">fine</s><s t="400" ac="255"> thanks</s></p>
</body>
</timedtext>"##;

/// Тот же смысл в записи json3 (0xFFD93D = 16767293).
const JSON3_RICH: &str = r#"{"wireMagic":"pb3",
"pens":[{},{"bAttr":1,"fcForeColor":16767293,"foForeAlpha":254,"bcBackColor":0,"boBackAlpha":128},{"iAttr":1,"etEdgeType":3}],
"wsWinStyles":[{},{"mhModeHint":2,"juJustifCode":0,"sdScrollDir":3}],
"wpWinPositions":[{},{"apPoint":6,"ahHorPos":20,"avVerPos":100,"rcRows":2,"ccCols":40}],
"events":[
{"tStartMs":0,"id":1,"wpWinPosId":1,"wsWinStyleId":1},
{"tStartMs":100,"dDurationMs":900,"wpWinPosId":1,"wsWinStyleId":1,"pPenId":1,"segs":[{"utf8":"Manual\ntwo"}]},
{"tStartMs":300,"dDurationMs":500,"pPenId":2,"segs":[{"utf8":"Second"}]},
{"tStartMs":1000,"dDurationMs":4000,"wWinId":1,"segs":[{"utf8":"good","acAsrConf":255},{"utf8":" morning","tOffsetMs":200,"acAsrConf":255}]},
{"tStartMs":2400,"dDurationMs":800,"wWinId":1,"aAppend":1,"segs":[{"utf8":"\n"}]},
{"tStartMs":2500,"dDurationMs":4000,"wWinId":1,"segs":[{"utf8":"how","acAsrConf":255},{"utf8":" are","tOffsetMs":300,"acAsrConf":255},{"utf8":" you","tOffsetMs":900,"pPenId":1,"acAsrConf":255}]},
{"tStartMs":5400,"dDurationMs":800,"wWinId":1,"aAppend":1,"segs":[{"utf8":"\n"}]},
{"tStartMs":5500,"dDurationMs":3000,"wWinId":1,"segs":[{"utf8":"fine","acAsrConf":255},{"utf8":" thanks","tOffsetMs":400,"acAsrConf":255}]}
]}"#;

#[test]
fn json3_and_srv3_with_the_same_meaning_produce_identical_events() {
    let from_json3 = load_json3(JSON3_RICH);
    let from_srv3 = load_srv3(SRV3_RICH);

    // Главное: события (текст, стиль слов, размещение, моменты появления) совпадают целиком.
    assert_eq!(
        from_json3.document().events(),
        from_srv3.document().events()
    );
    // Отчёт тоже: перо `et`, окно `mh/sd` и позиция `cc` — три проигнорированных правила стиля.
    let (json3_report, srv3_report) = (
        from_json3.document().report(),
        from_srv3.document().report(),
    );
    assert_eq!(json3_report.ignored_style_rules(), 3);
    assert_eq!(
        json3_report.ignored_style_rules(),
        srv3_report.ignored_style_rules()
    );
    assert_eq!(json3_report.skipped_cues(), srv3_report.skipped_cues());
    assert_eq!(json3_report.empty_cues(), srv3_report.empty_cues());

    // Сравнение не пустое: события реально несут стиль, позицию и построчное появление слов.
    assert!(from_json3.document().events().len() >= 6);
    let manual = single_cue_at(&from_json3, 150);
    assert_eq!(manual.text.plain_text(), "Manual\ntwo");
    let bold_yellow = manual.text.spans()[0].style.color;
    assert_eq!(bold_yellow, Some(SubtitleColor::opaque(0xFF, 0xD9, 0x3D)));
    let rolling = words_at(&from_json3, 2_500);
    assert_eq!(
        rolling.last(),
        Some(&(" you".to_owned(), Some(time(3_400))))
    );
    // Строки и время идентичны и на всей оси времени.
    for millis in [0, 150, 350, 1_100, 2_500, 4_999, 5_000, 5_500, 7_000, 8_500] {
        assert_eq!(
            active_texts_at(&from_json3, millis),
            active_texts_at(&from_srv3, millis),
            "at {millis} ms"
        );
    }
}

#[test]
fn per_word_offsets_become_absolute_reveal_moments() {
    let loaded = load_json3(JSON3_RICH);
    assert_eq!(
        words_at(&loaded, 2_500),
        vec![
            ("good".to_owned(), None),
            (" morning".to_owned(), None),
            ("\n".to_owned(), None),
            ("how".to_owned(), None),
            (" are".to_owned(), Some(time(2_800))),
            (" you".to_owned(), Some(time(3_400))),
        ]
    );
    let (timeline, _) = SubtitleTimeline::from_events(loaded.document().events().iter().cloned());
    assert_eq!(timeline.next_change_after(time(2_500)), Some(time(2_800)));
    assert_eq!(timeline.next_change_after(time(2_800)), Some(time(3_400)));
}

#[test]
fn segments_with_newlines_empty_text_and_missing_duration() {
    let loaded = load_json3(
        r#"{"events":[
        {"tStartMs":1000,"dDurationMs":1000,"segs":[{"utf8":"one\ntwo"},{"utf8":""},{"utf8":" three"}]},
        {"tStartMs":3000,"dDurationMs":500,"segs":[{"utf8":""}]},
        {"tStartMs":4000,"segs":[{"utf8":"no duration"}]}
        ]}"#,
    );
    // Перевод строки в seg делит строки; пустой seg ничего не добавляет.
    assert_eq!(active_texts_at(&loaded, 1_500), ["one\ntwo three"]);
    // Событие только с пустым текстом — пустая реплика в отчёте, на экране ничего.
    assert!(active_texts_at(&loaded, 3_200).is_empty());
    assert_eq!(loaded.document().report().empty_cues(), 1);
    // Без `dDurationMs` — 5 секунд по умолчанию (как srv3 без `d`).
    assert_eq!(active_texts_at(&loaded, 8_999), ["no duration"]);
    assert!(active_texts_at(&loaded, 9_000).is_empty());
}

#[test]
fn unreadable_fields_skip_only_the_broken_event() {
    let loaded = load_json3(
        r#"{"events":[
        {"tStartMs":"soon","dDurationMs":1000,"segs":[{"utf8":"bad start"}]},
        7,
        {"tStartMs":2000,"dDurationMs":1000,"segs":[{"utf8":"good"}]}
        ]}"#,
    );
    assert_eq!(active_texts_at(&loaded, 2_500), ["good"]);
    assert_eq!(loaded.document().events().len(), 1);
    // Нечитаемое время и событие-не-объект видны в отчёте, а не пропадают молча.
    assert_eq!(loaded.document().report().skipped_cues(), 2);
}

#[test]
fn unsupported_style_fields_are_counted_once_per_item() {
    let loaded = load_json3(
        r##"{"pens":[{"bAttr":1,"szPenSize":100},{"etEdgeType":1},"oops",{"fcForeColor":16777216}],
        "events":[{"tStartMs":0,"dDurationMs":1000,"pPenId":3,"segs":[{"utf8":"x"}]}]}"##,
    );
    // Перо 1 (обводка), перо 2 (не объект) и перо 3 (цвет вне 0xRRGGBB); перо 0 поддержано полностью.
    assert_eq!(loaded.document().report().ignored_style_rules(), 3);
    // Перо с нечитаемым цветом остаётся пером без цвета: стиль пользователя.
    assert_eq!(
        single_cue_at(&loaded, 100).text.spans()[0].style.color,
        None
    );
}

#[test]
fn sniff_recognises_json3_and_not_other_json_or_formats() {
    let detect = |text: &str| sniff_text_subtitle_format(text);
    assert_eq!(detect(JSON3_RICH), Some(TextSubtitleFormat::Json3));
    assert_eq!(
        detect("\u{feff} \n {\"events\" : []}"),
        Some(TextSubtitleFormat::Json3)
    );
    // `events` после таблиц стилей, без `wireMagic`.
    assert_eq!(
        detect(r#"{"pens":[{}],"events":[]}"#),
        Some(TextSubtitleFormat::Json3)
    );
    // Другой JSON и слово в значении — не json3.
    assert_eq!(detect(r#"{"foo":1}"#), None);
    assert_eq!(detect(r#"{"title":"events","x":"wireMagic"}"#), None);
    assert_eq!(detect(r#"["events"]"#), None);
    // Остальные форматы по-прежнему определяются как раньше.
    assert_eq!(
        detect("<timedtext format=\"3\"><body/></timedtext>"),
        Some(TextSubtitleFormat::Srv3)
    );
    assert_eq!(
        detect("1\n00:00:01,000 --> 00:00:02,000\nHi\n"),
        Some(TextSubtitleFormat::Srt)
    );
}

#[test]
fn sniffed_json3_loads_end_to_end() {
    let format = sniff_text_subtitle_format(JSON3_RICH).unwrap();
    let loaded = try_load_with(format, JSON3_RICH, &ROOMY_LIMITS).unwrap();
    assert_eq!(active_texts_at(&loaded, 1_100), ["good morning"]);
}

#[test]
fn bom_before_json_is_accepted() {
    let with_bom = format!("\u{feff}{JSON3_RICH}");
    let loaded = load_json3(&with_bom);
    assert_eq!(active_texts_at(&loaded, 1_100), ["good morning"]);
}

#[test]
fn malformed_json_is_a_typed_error_without_echoing_content() {
    for broken in [
        r#"{"events":[{"tStartMs":1,"segs":"#,
        r#"{"events":[],}"#,
        "{events:[]}",
        "",
        r#"{"events":[]} trailing"#,
    ] {
        let error = try_load_json3(broken).unwrap_err();
        assert_eq!(
            error,
            SubtitleLoadError::MalformedJson3,
            "input: {broken:?}"
        );
    }
    let message = try_load_json3(r#"{"secret-title":"#)
        .unwrap_err()
        .to_string();
    assert!(!message.contains("secret-title"));
}

#[test]
fn valid_json_that_is_not_json3_is_rejected() {
    for other in [
        r#"{"foo":1}"#,
        "[1,2,3]",
        "\"just a string\"",
        "42",
        r#"{"events":5}"#,
        r#"{"events":{"a":1}}"#,
    ] {
        let error = try_load_json3(other).unwrap_err();
        assert_eq!(error, SubtitleLoadError::NotJson3, "input: {other:?}");
    }
}

#[test]
fn header_without_events_is_an_empty_document() {
    let loaded = load_json3(r#"{"wireMagic":"pb3"}"#);
    assert!(loaded.document().events().is_empty());
}

#[test]
fn event_limit_is_enforced_and_exact_count_passes() {
    let three_events = r#"{"events":[
        {"tStartMs":0,"dDurationMs":100,"segs":[{"utf8":"a"}]},
        {"tStartMs":200,"dDurationMs":100,"segs":[{"utf8":"b"}]},
        {"tStartMs":400,"dDurationMs":100,"segs":[{"utf8":"c"}]}]}"#;
    let limited = SubtitleParseLimits::new(1 << 20, 2, 100);
    let error = try_load_with(TextSubtitleFormat::Json3, three_events, &limited).unwrap_err();
    assert_eq!(error, SubtitleLoadError::TooManyEvents { limit: 2 });
    let exact = SubtitleParseLimits::new(1 << 20, 3, 100);
    let loaded = try_load_with(TextSubtitleFormat::Json3, three_events, &exact).unwrap();
    assert_eq!(loaded.document().events().len(), 3);
    // Бегущее окно: лимит действует и на события, порождённые общим конвейером.
    let rolling_limit = SubtitleParseLimits::new(1 << 20, 3, 100);
    let error = try_load_with(TextSubtitleFormat::Json3, JSON3_RICH, &rolling_limit).unwrap_err();
    assert_eq!(error, SubtitleLoadError::TooManyEvents { limit: 3 });
}

#[test]
fn input_size_limit_is_enforced_before_parsing() {
    let tiny = SubtitleParseLimits::new(16, 100, 100);
    let error = try_load_with(TextSubtitleFormat::Json3, JSON3_RICH, &tiny).unwrap_err();
    assert!(matches!(error, SubtitleLoadError::InputTooLarge { .. }));
}

#[test]
fn deep_nesting_is_a_clean_error_not_a_crash() {
    // Глубже и предела serde_json (128), и нашего: без защиты это переполнение стека или ошибка рекурсии.
    let depth = 100_000;
    let deep_array = format!(r#"{{"events":{}{}}}"#, "[".repeat(depth), "]".repeat(depth));
    let limits = SubtitleParseLimits::new(1 << 22, 100, 100);
    let error = try_load_with(TextSubtitleFormat::Json3, &deep_array, &limits).unwrap_err();
    assert_eq!(error, SubtitleLoadError::Json3TooComplex);

    let deep_object = format!(
        r#"{{"events":[{{"segs":[{}{}]}}]}}"#,
        r#"{"a":"#.repeat(40),
        "1}".repeat(40)
    );
    let error = try_load_with(TextSubtitleFormat::Json3, &deep_object, &limits).unwrap_err();
    assert_eq!(error, SubtitleLoadError::Json3TooComplex);
}

#[test]
fn brackets_inside_strings_do_not_count_as_nesting() {
    let text = format!(
        r#"{{"events":[{{"tStartMs":0,"dDurationMs":1000,"segs":[{{"utf8":"{}\"{}"}}]}}]}}"#,
        "[{".repeat(200),
        "}]".repeat(200)
    );
    let loaded = load_json3(&text);
    assert_eq!(loaded.document().events().len(), 1);
}
