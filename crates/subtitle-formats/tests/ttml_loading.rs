#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Сквозные тесты TTML / IMSC: байты -> `load_text_subtitle` -> `SubtitleTimeline` -> что на экране.
//! Фикстуры синтетические: структура настоящих файлов (Netflix/EBU-TT-D/IMSC), придуманный текст.

mod common;

use common::{ROOMY_LIMITS, active_texts_at, try_load_text};
use subtitle_core::{
    CueLine, CueLineAlign, CuePosition, CuePositionAlign, CueTextAlign, FontSlant, FontWeight,
    PercentOfVideo, SpanContent, SubtitleColor, SubtitleContent, SubtitleEvent, SubtitleTextCue,
    SubtitleTime, SubtitleTimeline, TextStrikethrough, TextUnderline,
};
use subtitle_formats::{
    LegacyEncodingHint, LoadedTextSubtitle, SubtitleLoadError, SubtitleParseLimits,
    TextSubtitleFormat, TtmlSampleWindow, load_ttml_sample, sniff_text_subtitle_format,
};

fn try_load_ttml(text: &str) -> Result<LoadedTextSubtitle, SubtitleLoadError> {
    try_load_text(
        TextSubtitleFormat::Ttml,
        text.as_bytes(),
        LegacyEncodingHint::Generic,
        &ROOMY_LIMITS,
    )
}

fn load_ttml(text: &str) -> LoadedTextSubtitle {
    try_load_ttml(text).unwrap()
}

/// Документ TTML с заголовком пространств имён и телом `body`.
fn document(attributes: &str, head: &str, body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling"
    xmlns:ttp="http://www.w3.org/ns/ttml#parameter" xml:lang="en" {attributes}>
<head>{head}</head>
<body>{body}</body>
</tt>"#
    )
}

fn timeline_of(loaded: &LoadedTextSubtitle) -> SubtitleTimeline {
    SubtitleTimeline::from_events(loaded.document().events().iter().cloned()).0
}

/// События в порядке разбора.
fn events(loaded: &LoadedTextSubtitle) -> &[SubtitleEvent] {
    loaded.document().events()
}

fn text_cue(event: &SubtitleEvent) -> &SubtitleTextCue {
    match event.content() {
        SubtitleContent::Text(cue) => cue,
        _ => panic!("expected text cue"),
    }
}

fn millis(event_time: SubtitleTime) -> i64 {
    event_time.as_micros() / 1000
}

fn percent(value: f32) -> PercentOfVideo {
    PercentOfVideo::new(value).unwrap()
}

#[test]
fn sniff_recognises_ttml_root_in_the_ttml_namespace_only() {
    let ttml = document("", "", "<div/>");
    assert_eq!(
        sniff_text_subtitle_format(&ttml),
        Some(TextSubtitleFormat::Ttml)
    );
    let prefixed = r#"<ns:tt xmlns:ns="http://www.w3.org/ns/ttml"><ns:body/></ns:tt>"#;
    assert_eq!(
        sniff_text_subtitle_format(prefixed),
        Some(TextSubtitleFormat::Ttml)
    );
    let other_xml = r#"<tt xmlns="http://example.com/other"><body/></tt>"#;
    assert_eq!(sniff_text_subtitle_format(other_xml), None);
    let styling_namespace_only = r#"<tt xmlns:tts="http://www.w3.org/ns/ttml#styling"></tt>"#;
    assert_eq!(sniff_text_subtitle_format(styling_namespace_only), None);
}

#[test]
fn clock_times_become_events_found_by_the_timeline() {
    let loaded = load_ttml(&document(
        "",
        "",
        r#"<div><p begin="00:00:01.000" end="00:00:03.500">Hello, world.</p>
<p begin="00:00:05.000" end="00:00:06.000">Second.</p></div>"#,
    ));
    assert_eq!(events(&loaded).len(), 2);
    assert_eq!(active_texts_at(&loaded, 999), Vec::<String>::new());
    assert_eq!(active_texts_at(&loaded, 1000), ["Hello, world."]);
    assert_eq!(active_texts_at(&loaded, 3499), ["Hello, world."]);
    assert_eq!(active_texts_at(&loaded, 3500), Vec::<String>::new());
    assert_eq!(active_texts_at(&loaded, 5500), ["Second."]);
    assert_eq!(loaded.document().report().skipped_cues(), 0);
}

#[test]
fn tick_rate_offsets_like_netflix_files() {
    let loaded = load_ttml(&document(
        r#"ttp:tickRate="10000000""#,
        "",
        r#"<div><p begin="12500000t" end="35000000t">Ticks.</p></div>"#,
    ));
    let event = &events(&loaded)[0];
    assert_eq!((millis(event.start()), millis(event.end())), (1250, 3500));
}

#[test]
fn frame_based_times_use_frame_rate_and_multiplier() {
    let loaded = load_ttml(&document(
        r#"ttp:frameRate="30" ttp:frameRateMultiplier="1000 1001""#,
        "",
        r#"<div><p begin="00:00:00:30" end="60f">Frames.</p></div>"#,
    ));
    let event = &events(&loaded)[0];
    // 30 кадров при 30000/1001 к/с = 1,001 с; 60 кадров = 2,002 с.
    assert_eq!(event.start().as_micros(), 1_001_000);
    assert_eq!(event.end().as_micros(), 2_002_000);
}

#[test]
fn nested_times_are_relative_to_parent_begin_and_inherit_parent_end() {
    let loaded = load_ttml(&document(
        "",
        "",
        r#"<div begin="10s" end="20s">
  <p begin="1s" end="3s">Early.</p>
  <p begin="5s">Until div end.</p>
  <p begin="1s" dur="2s">By duration.</p>
  <p begin="15s" end="30s">Clamped.</p>
</div>"#,
    ));
    let spans: Vec<(i64, i64)> = events(&loaded)
        .iter()
        .map(|event| (millis(event.start()), millis(event.end())))
        .collect();
    // Четвёртый абзац начинается в 25 с, когда div уже закончился (20 с), и не показывается.
    assert_eq!(
        spans,
        [(11_000, 13_000), (15_000, 20_000), (11_000, 13_000)]
    );
    assert_eq!(active_texts_at(&loaded, 19_000), ["Until div end."]);
}

#[test]
fn child_starting_after_parent_end_is_not_shown() {
    let loaded = load_ttml(&document(
        "",
        "",
        r#"<div begin="0s" end="5s"><p begin="6s" end="8s">Never.</p><p begin="1s">Shown.</p></div>"#,
    ));
    assert_eq!(events(&loaded).len(), 1);
    assert_eq!(active_texts_at(&loaded, 2000), ["Shown."]);
}

#[test]
fn open_ended_paragraph_lasts_until_the_next_paragraph_begins() {
    let loaded = load_ttml(&document(
        "",
        "",
        r#"<div><p begin="1s">Open.</p><p begin="2s" end="9s">Closed.</p></div>"#,
    ));
    let open = &events(&loaded)[0];
    assert_eq!(millis(open.end()), 2000, "must not overlap the next cue");
    // Последний открытый абзац идёт до позднейшего момента документа.
    let last = load_ttml(&document(
        "",
        "",
        r#"<div><p begin="1s" end="3s">Closed.</p><p begin="2s">Open.</p></div>"#,
    ));
    let open = events(&last)
        .iter()
        .find(|event| millis(event.start()) == 2000)
        .unwrap();
    assert_eq!(millis(open.end()), 3000);
}

#[test]
fn epoch_scale_media_times_are_accepted() {
    // ~1.7e9 с (эпоха Unix): так выглядят времена живых упаковщиков в stpp.
    let loaded = load_ttml(&document(
        "",
        "",
        r#"<div><p begin="472222:13:20.250" end="472222:13:20.750">Epoch.</p></div>"#,
    ));
    let event = &events(&loaded)[0];
    let begin_seconds = 472_222_i64 * 3600 + 13 * 60 + 20;
    assert_eq!(millis(event.start()), begin_seconds * 1000 + 250);
    assert_eq!(millis(event.end()), begin_seconds * 1000 + 750);
}

#[test]
fn time_beyond_what_subtitle_time_can_hold_is_rejected_not_wrapped() {
    let loaded = load_ttml(&document(
        "",
        "",
        r#"<div><p begin="99999999999999:00:00.000" end="99999999999999:00:01.000">Huge.</p><p begin="1s" end="2s">Ok.</p></div>"#,
    ));
    assert_eq!(events(&loaded).len(), 1);
    assert_eq!(active_texts_at(&loaded, 1500), ["Ok."]);
}

fn load_sample(text: &str, window_millis: (i64, i64)) -> LoadedTextSubtitle {
    load_ttml_sample(
        text.as_bytes(),
        LegacyEncodingHint::Generic,
        &ROOMY_LIMITS,
        TtmlSampleWindow::new(
            SubtitleTime::from_millis(window_millis.0),
            SubtitleTime::from_millis(window_millis.1),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn sample_document_is_active_only_inside_its_sample_interval() {
    // Реплика 0..10 с в сэмпле 2..4 с: видна только внутри сэмпла.
    let loaded = load_sample(
        &document("", "", r#"<div><p begin="0s" end="10s">Clipped.</p></div>"#),
        (2000, 4000),
    );
    let all = events(&loaded);
    assert_eq!(all.len(), 1);
    assert_eq!((millis(all[0].start()), millis(all[0].end())), (2000, 4000));
}

#[test]
fn sample_document_drops_cues_entirely_outside_the_interval() {
    let loaded = load_sample(
        &document(
            "",
            "",
            r#"<div><p begin="0s" end="1s">Before.</p><p begin="5s" end="6s">After.</p></div>"#,
        ),
        (2000, 4000),
    );
    assert!(events(&loaded).is_empty());
}

#[test]
fn open_ended_paragraph_in_a_sample_ends_with_the_sample_not_after_five_seconds() {
    let loaded = load_sample(
        &document("", "", r#"<div><p begin="2s">Open.</p></div>"#),
        (2000, 12_000),
    );
    let all = events(&loaded);
    assert_eq!(
        (millis(all[0].start()), millis(all[0].end())),
        (2000, 12_000)
    );
}

#[test]
fn empty_sample_window_cannot_be_built() {
    assert!(
        TtmlSampleWindow::new(SubtitleTime::from_millis(5), SubtitleTime::from_millis(5)).is_none()
    );
}

#[test]
fn span_timing_splits_a_paragraph_into_karaoke_like_intervals() {
    let loaded = load_ttml(&document(
        "",
        "",
        r#"<div><p begin="0s" end="4s"><span begin="0s">one</span> <span begin="1s">two</span> <span begin="2s" end="3s">three</span></p></div>"#,
    ));
    let timeline = timeline_of(&loaded);
    let texts_at = |millis: i64| {
        let mut ids = Vec::new();
        timeline.collect_active_at(SubtitleTime::from_millis(millis), &mut ids);
        ids.into_iter()
            .map(|id| match timeline.event(id).unwrap().content() {
                SubtitleContent::Text(cue) => cue.text.plain_text(),
                _ => panic!("text expected"),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(texts_at(500), ["one"]);
    assert_eq!(texts_at(1500), ["one two"]);
    assert_eq!(texts_at(2500), ["one two three"]);
    assert_eq!(texts_at(3500), ["one two"]);
    // Интервалы не пересекаются: ровно одно событие в любой момент.
    assert_eq!(events(&loaded).len(), 4);
}

#[test]
fn identical_consecutive_intervals_are_merged() {
    let loaded = load_ttml(&document(
        "",
        "",
        r#"<div><p begin="0s" end="6s"><span begin="1s" end="2s"></span><span begin="3s">same</span>
<span begin="4s" end="5s"></span></p></div>"#,
    ));
    // Невидимые пустые спаны не создают границ с разным текстом: "same" идёт одним событием.
    assert_eq!(events(&loaded).len(), 1);
    let event = &events(&loaded)[0];
    assert_eq!((millis(event.start()), millis(event.end())), (3000, 6000));
}

const STYLED: &str = r##"
<styling>
  <style xml:id="base" tts:color="white" tts:fontWeight="bold"/>
  <style xml:id="yellowItalic" style="base" tts:color="#ffff00" tts:fontStyle="italic"/>
  <style xml:id="deco" tts:textDecoration="underline lineThrough"/>
</styling>"##;

#[test]
fn style_chain_inline_attributes_and_inheritance_reach_span_styles() {
    let loaded = load_ttml(&document(
        "",
        STYLED,
        r##"<div style="deco"><p begin="0s" end="2s" style="yellowItalic">Plain <span tts:color="rgb(255,0,0)" tts:fontWeight="normal" tts:backgroundColor="#00000080">red</span></p></div>"##,
    ));
    let cue = text_cue(&events(&loaded)[0]);
    let spans = cue.text.spans();
    assert_eq!(spans.len(), 2);
    // Цепочка: yellowItalic -> base (жирный), собственный цвет перекрывает цвет base.
    assert_eq!(
        spans[0].style.color,
        Some(SubtitleColor::opaque(255, 255, 0))
    );
    assert_eq!(spans[0].style.weight, FontWeight::Bold);
    assert_eq!(spans[0].style.slant, FontSlant::Italic);
    // Декорации унаследованы от div.
    assert_eq!(spans[0].style.underline, TextUnderline::Single);
    assert_eq!(spans[0].style.strikethrough, TextStrikethrough::Single);
    // Инлайн-атрибуты span перекрывают унаследованное.
    assert_eq!(spans[1].style.color, Some(SubtitleColor::opaque(255, 0, 0)));
    assert_eq!(spans[1].style.weight, FontWeight::Normal);
    assert_eq!(spans[1].style.slant, FontSlant::Italic);
    assert_eq!(
        spans[1].style.background,
        Some(SubtitleColor {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 0x80
        })
    );
    assert_eq!(spans[0].style.background, None);
    assert_eq!(cue.text.plain_text(), "Plain red");
}

#[test]
fn language_of_nearest_xml_lang_goes_to_spans() {
    let loaded = load_ttml(&document(
        "",
        "",
        r#"<div><p begin="0s" end="1s">Hi <span xml:lang="fr">bonjour</span></p></div>"#,
    ));
    let spans = text_cue(&events(&loaded)[0]).text.spans();
    assert_eq!(spans[0].language.as_deref(), Some("en"));
    assert_eq!(spans[1].language.as_deref(), Some("fr"));
}

const REGIONS: &str = r#"
<layout>
  <region xml:id="top" tts:origin="10% 5%" tts:extent="80% 20%" tts:displayAlign="before" tts:textAlign="center"/>
  <region xml:id="bottom" tts:origin="10% 75%" tts:extent="80% 20%" tts:displayAlign="after" tts:textAlign="center"/>
  <region xml:id="left" tts:origin="0% 40%" tts:extent="30% 20%" tts:displayAlign="before" tts:textAlign="start"/>
</layout>"#;

#[test]
fn region_origin_and_extent_become_cue_placement() {
    let loaded = load_ttml(&document(
        "",
        REGIONS,
        r#"<div><p begin="0s" end="1s" region="bottom">Down</p>
<p begin="0s" end="1s" region="top">Up</p>
<p begin="0s" end="1s" region="left">Side</p>
<p begin="0s" end="1s">Default</p></div>"#,
    ));
    let placements: Vec<_> = events(&loaded)
        .iter()
        .map(|event| text_cue(event).placement.clone())
        .collect();
    let bottom = &placements[0];
    assert_eq!(bottom.line, CueLine::Percent(percent(95.0)));
    assert_eq!(bottom.line_align, CueLineAlign::End);
    assert_eq!(bottom.position, CuePosition::Percent(percent(50.0)));
    assert_eq!(bottom.position_align, CuePositionAlign::Center);
    assert_eq!(bottom.size, percent(80.0));
    assert_eq!(bottom.text_align, CueTextAlign::Center);

    let top = &placements[1];
    assert_eq!(top.line, CueLine::Percent(percent(5.0)));
    assert_eq!(top.line_align, CueLineAlign::Start);

    let side = &placements[2];
    assert_eq!(side.position, CuePosition::Percent(percent(0.0)));
    assert_eq!(side.position_align, CuePositionAlign::LineLeft);
    assert_eq!(side.size, percent(30.0));
    assert_eq!(side.text_align, CueTextAlign::Start);

    // Без региона — размещение по умолчанию (внизу по центру).
    assert_eq!(placements[3].line, CueLine::Auto);
    assert_eq!(placements[3].position, CuePosition::Auto);
}

#[test]
fn region_is_inherited_from_div_and_pixel_regions_use_the_root_extent() {
    let loaded = load_ttml(&document(
        r#"tts:extent="1920px 1080px""#,
        r#"<layout><region xml:id="r" tts:origin="192px 108px" tts:extent="960px 540px" tts:displayAlign="before"/></layout>"#,
        r#"<div region="r"><p begin="0s" end="1s">Inherited</p></div>"#,
    ));
    let placement = &text_cue(&events(&loaded)[0]).placement;
    assert_eq!(placement.line, CueLine::Percent(percent(10.0)));
    assert_eq!(placement.size, percent(50.0));
    // textAlign не задан -> по центру региона: 10% + 25%.
    assert_eq!(placement.position, CuePosition::Percent(percent(35.0)));
}

#[test]
fn pixel_region_without_root_extent_is_counted_and_falls_back_to_full_frame() {
    let loaded = load_ttml(&document(
        "",
        r#"<layout><region xml:id="r" tts:origin="100px 100px" tts:extent="200px 100px"/></layout>"#,
        r#"<div><p begin="0s" end="1s" region="r">Px</p></div>"#,
    ));
    assert!(loaded.document().report().ignored_style_rules() >= 2);
    assert_eq!(events(&loaded).len(), 1);
}

#[test]
fn whitespace_is_collapsed_and_br_breaks_lines() {
    let loaded = load_ttml(&document(
        "",
        "",
        "<div><p begin=\"0s\" end=\"1s\">\n     First   line  <br/>\n   second\n     line   </p></div>",
    ));
    let cue = text_cue(&events(&loaded)[0]);
    assert_eq!(cue.text.plain_text(), "First line\nsecond line");
    assert!(
        cue.text
            .spans()
            .iter()
            .any(|span| span.content == SpanContent::LineBreak)
    );
}

#[test]
fn xml_space_preserve_keeps_whitespace_and_newlines() {
    let loaded = load_ttml(&document(
        "",
        "",
        "<div><p begin=\"0s\" end=\"1s\" xml:space=\"preserve\">a  b\nc</p></div>",
    ));
    assert_eq!(text_cue(&events(&loaded)[0]).text.plain_text(), "a  b\nc");
}

#[test]
fn unsupported_features_are_counted_but_do_not_fail_the_load() {
    let loaded = load_ttml(&document(
        r#"ttp:timeBase="smpte""#,
        r#"<styling><style xml:id="s" tts:fontFamily="Arial" tts:fontSize="120%"/></styling>"#,
        r#"<div><p begin="0s" end="1s" style="s" tts:textOutline="black 2px">Text<set begin="0s" tts:color="red"/></p></div>"#,
    ));
    assert_eq!(events(&loaded).len(), 1);
    // timeBase + 2 свойства стиля + textOutline + анимация.
    assert_eq!(loaded.document().report().ignored_style_rules(), 5);
}

#[test]
fn malformed_xml_and_foreign_roots_are_typed_errors() {
    assert_eq!(
        try_load_ttml("<tt xmlns=\"http://www.w3.org/ns/ttml\"><body></tt>").unwrap_err(),
        SubtitleLoadError::MalformedTtml
    );
    assert_eq!(
        try_load_ttml("<tt><body/></tt>").unwrap_err(),
        SubtitleLoadError::NotTtml
    );
    assert_eq!(
        try_load_ttml(r#"<timedtext format="3"/>"#).unwrap_err(),
        SubtitleLoadError::NotTtml
    );
    assert_eq!(
        try_load_ttml("<!DOCTYPE tt [<!ENTITY x \"y\">]><tt xmlns=\"http://www.w3.org/ns/ttml\"/>")
            .unwrap_err(),
        SubtitleLoadError::MalformedTtml
    );
    let message = SubtitleLoadError::MalformedTtml.to_string();
    assert!(
        !message.contains("tt"),
        "message must not echo content: {message}"
    );
}

#[test]
fn xml_budgets_are_enforced_as_ttml_too_complex() {
    let deep_open = "<span>".repeat(100);
    let deep_close = "</span>".repeat(100);
    let deep = document(
        "",
        "",
        &format!(r#"<div><p begin="0s" end="1s">{deep_open}x{deep_close}</p></div>"#),
    );
    assert_eq!(
        try_load_ttml(&deep).unwrap_err(),
        SubtitleLoadError::TtmlTooComplex
    );

    let many_spans = "<span>a</span>".repeat(3000);
    let wide = document(
        "",
        "",
        &format!(r#"<div><p begin="0s" end="1s">{many_spans}</p></div>"#),
    );
    assert_eq!(
        try_load_ttml(&wide).unwrap_err(),
        SubtitleLoadError::TtmlTooComplex
    );
}

#[test]
fn event_limit_is_a_fatal_error_and_invalid_times_skip_only_that_paragraph() {
    let limits = SubtitleParseLimits::new(1 << 20, 1, 100);
    let two = document(
        "",
        "",
        r#"<div><p begin="0s" end="1s">a</p><p begin="2s" end="3s">b</p></div>"#,
    );
    let error = try_load_text(
        TextSubtitleFormat::Ttml,
        two.as_bytes(),
        LegacyEncodingHint::Generic,
        &limits,
    )
    .unwrap_err();
    assert_eq!(error, SubtitleLoadError::TooManyEvents { limit: 1 });

    let loaded = load_ttml(&document(
        "",
        "",
        r#"<div><p begin="nonsense" end="1s">bad</p><p begin="0s" end="1s">good</p></div>"#,
    ));
    assert_eq!(active_texts_at(&loaded, 500), ["good"]);
}

#[test]
fn utf16_file_with_bom_is_decoded_and_parsed() {
    let text = document("", "", r#"<div><p begin="0s" end="1s">Привет</p></div>"#);
    let mut bytes = vec![0xff, 0xfe];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let loaded = try_load_text(
        TextSubtitleFormat::Ttml,
        &bytes,
        LegacyEncodingHint::Generic,
        &ROOMY_LIMITS,
    )
    .unwrap();
    assert_eq!(active_texts_at(&loaded, 500), ["Привет"]);
}
