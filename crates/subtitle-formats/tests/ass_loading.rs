#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Сквозные тесты ASS/SSA: байты -> `load_text_subtitle` / `parse_ass_script` +
//! `parse_embedded_ass_event` -> события с разобранными стилями и тегами.

use std::sync::Arc;

use subtitle_core::{
    AssAlignment, AssAlphaTarget, AssAxes, AssBold, AssBorderStyle, AssColor, AssCue,
    AssHardOverrides, AssHorizontalAlign, AssOverride, AssRgb, AssScaledBorderAndShadow, AssScript,
    AssScriptKind, AssTextItem, AssUnsupportedTag, AssVerticalAlign, AssWrapStyle, SubtitleContent,
    SubtitleEvent, SubtitleTime, SubtitleTimeline,
};
use subtitle_formats::{
    AssParseError, AssParseLimits, AssTagStats, EmbeddedCueError, EmbeddedCueLimits,
    EmbeddedCueMalformation, LegacyEncodingHint, LoadedTextSubtitle, ParseIssueKind,
    SubtitleLoadError, SubtitleParseLimits, TextSubtitleFormat, load_text_subtitle,
    parse_ass_script, parse_embedded_ass_event, sniff_text_subtitle_format,
};

/// Щедрые лимиты файла: тесты, проверяющие лимиты, задают свои.
const ROOMY_LIMITS: SubtitleParseLimits = SubtitleParseLimits::new(1 << 20, 10_000, 100);

const ASS_LIMITS: AssParseLimits = AssParseLimits {
    max_header_bytes: 1 << 16,
    max_styles: 32,
    max_override_tags_per_event: 64,
    max_tracked_unknown_tag_names: 8,
};

fn limits() -> SubtitleParseLimits {
    ROOMY_LIMITS.with_ass_limits(ASS_LIMITS)
}

fn try_load(
    text: &str,
    limits: &SubtitleParseLimits,
) -> Result<LoadedTextSubtitle, SubtitleLoadError> {
    load_text_subtitle(
        text.as_bytes(),
        TextSubtitleFormat::Ass,
        LegacyEncodingHint::Generic,
        limits,
    )
}

fn load(text: &str) -> LoadedTextSubtitle {
    try_load(text, &limits()).unwrap()
}

fn cue_of(event: &SubtitleEvent) -> &AssCue {
    match event.content() {
        SubtitleContent::Ass(cue) => cue,
        other => panic!("expected ASS content, got {other:?}"),
    }
}

const STYLE_FORMAT: &str = "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding";
const EVENT_FORMAT: &str =
    "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text";

/// Типичный сценарий ASS с двумя стилями и разными событиями.
fn v4plus_script() -> String {
    format!(
        "[Script Info]\n\
         ScriptType: v4.00+\n\
         PlayResX: 1920\n\
         PlayResY: 1080\n\
         WrapStyle: 1\n\
         ScaledBorderAndShadow: yes\n\
         \n\
         [V4+ Styles]\n\
         {STYLE_FORMAT}\n\
         Style: Default,Noto Sans,48,&H00FFFFFF,&H000000FF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,2,1,2,10,10,40,1\n\
         Style: Sign,Arial,36,&H0000FFFF,&H000000FF,&H00112233,&H00000000,-1,1,0,0,80,120,1.5,0,3,0,0,8,5,6,7,204\n\
         \n\
         [Events]\n\
         {EVENT_FORMAT}\n\
         Dialogue: 0,0:00:01.00,0:00:03.50,Default,Alice,0,0,0,,Hello, world\n\
         Dialogue: 2,0:00:02.00,0:00:04.00,Sign,,15,25,35,Banner;5,{{\\an8\\bord2\\fscx120\\1c&H0000FF&}}Top text\n\
         Comment: 0,0:00:00.00,0:00:09.00,Default,,0,0,0,,ignored comment\n"
    )
}

#[test]
fn full_v4plus_script_loads_times_styles_margins_layers_and_overrides() {
    let loaded = load(&v4plus_script());
    let document = loaded.document();
    let events = document.events();
    assert_eq!(events.len(), 2, "Comment must not become an event");
    assert_eq!(document.report().skipped_cues(), 0);

    let first = &events[0];
    assert_eq!(first.start(), SubtitleTime::from_millis(1000));
    assert_eq!(first.end(), SubtitleTime::from_millis(3500));
    assert_eq!(first.layer().get(), 0);
    let first_cue = cue_of(first);
    assert_eq!(first_cue.read_order, 0);
    assert_eq!(first_cue.name, "Alice");
    assert_eq!(first_cue.style().name, "Default");
    // Запятая внутри Text не делит поле.
    assert_eq!(
        first_cue.text,
        vec![AssTextItem::Text("Hello, world".to_owned())]
    );

    let second = &events[1];
    assert_eq!(second.layer().get(), 2);
    let second_cue = cue_of(second);
    assert_eq!(second_cue.read_order, 1);
    assert_eq!(second_cue.effect, "Banner;5");
    let style = second_cue.style();
    assert_eq!(style.name, "Sign");
    assert_eq!(style.font_name, "Arial");
    assert_eq!(style.bold, AssBold::Bold);
    assert!(style.italic);
    assert_eq!((style.scale_x, style.scale_y), (0.8, 1.2));
    assert_eq!(style.spacing, 1.5);
    assert_eq!(style.border_style, AssBorderStyle::OpaqueBox);
    // &H0000FFFF: прозрачность 0, BB=00 GG=FF RR=FF -> жёлтый.
    assert_eq!(style.primary_color, AssColor::new(0xff, 0xff, 0x00, 0));
    assert_eq!(style.outline_color, AssColor::new(0x33, 0x22, 0x11, 0));
    assert_eq!(
        style.alignment,
        AssAlignment {
            horizontal: AssHorizontalAlign::Center,
            vertical: AssVerticalAlign::Top
        }
    );
    assert_eq!(style.encoding, 204);
    // Отступы события 15/25/35 перекрывают стиль; нулевые берутся из стиля.
    let resolved = second_cue.resolved_margins();
    assert_eq!(
        (resolved.left, resolved.right, resolved.vertical),
        (15, 25, 35)
    );
    let default_margins = first_cue.resolved_margins();
    assert_eq!(
        (
            default_margins.left,
            default_margins.right,
            default_margins.vertical
        ),
        (10, 10, 40)
    );
    assert_eq!(
        second_cue.text,
        vec![
            AssTextItem::Overrides(vec![
                AssOverride::Alignment(AssAlignment::from_numpad_tag(8)),
                AssOverride::Border {
                    axes: AssAxes::Both,
                    value: Some(2.0)
                },
                AssOverride::ScaleX(Some(1.2)),
                AssOverride::Color {
                    slot: subtitle_core::AssColorSlot::Primary,
                    value: Some(AssRgb::new(255, 0, 0))
                },
            ]),
            AssTextItem::Text("Top text".to_owned()),
        ]
    );
}

#[test]
fn script_is_exposed_with_resolved_info_and_shared_by_events() {
    let loaded = load(&v4plus_script());
    let document = loaded.document();
    let script = document.ass_script().expect("ASS document has a script");
    assert_eq!(script.kind(), AssScriptKind::Ass);
    assert_eq!(
        (script.info().play_res.width, script.info().play_res.height),
        (1920, 1080)
    );
    assert_eq!(script.info().wrap_style, AssWrapStyle::EndOfLine);
    assert_eq!(
        script.info().scaled_border_and_shadow,
        AssScaledBorderAndShadow::Enabled
    );
    for event in document.events() {
        assert!(Arc::ptr_eq(&cue_of(event).script, script));
    }
}

#[test]
fn script_without_playres_gets_384x288() {
    let loaded = load(&format!(
        "[Script Info]\n[Events]\n{EVENT_FORMAT}\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,x\n"
    ));
    let script = loaded.document().ass_script().unwrap();
    assert_eq!(
        (script.info().play_res.width, script.info().play_res.height),
        (384, 288)
    );
    // Стиля Default в файле нет — события используют встроенный стиль libass.
    let cue = cue_of(&loaded.document().events()[0]);
    assert_eq!(cue.style().font_name, "Arial");
    assert_eq!(cue.style().font_size, 18.0);
}

#[test]
fn timecode_fraction_is_centiseconds_times_ten() {
    let loaded = load(&format!(
        "[Events]\n{EVENT_FORMAT}\n\
         Dialogue: 0,0:00:01.5,0:00:02.123,Default,,0,0,0,,quirk\n"
    ));
    let event = &loaded.document().events()[0];
    // `.5` = 5 сотых = 50 мс; `.123` = 123 сотых = 1230 мс.
    assert_eq!(event.start(), SubtitleTime::from_millis(1050));
    assert_eq!(event.end(), SubtitleTime::from_millis(3230));
}

#[test]
fn ssa_v4_script_maps_legacy_alignment_tertiary_colour_and_alpha_level() {
    let loaded = load(
        "[Script Info]\n\
         ScriptType: v4.00\n\
         PlayResX: 640\n\
         PlayResY: 480\n\
         \n\
         [V4 Styles]\n\
         Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, TertiaryColour, BackColour, Bold, Italic, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, AlphaLevel, Encoding\n\
         Style: Default,Arial,20,16777215,255,65280,255,-1,0,1,2,3,6,10,10,10,64,0\n\
         Style: Odd,Arial,20,16777215,255,65280,255,0,0,1,2,3,8,10,10,10,0,0\n\
         \n\
         [Events]\n\
         Format: Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
         Dialogue: Marked=0,0:00:01.00,0:00:02.00,Default,,0,0,0,,SSA line\n\
         Dialogue: Marked=0,0:00:03.00,0:00:04.00,Odd,,0,0,0,,Odd line\n",
    );
    let script = loaded.document().ass_script().unwrap();
    assert_eq!(script.kind(), AssScriptKind::Ssa);
    let events = loaded.document().events();
    assert_eq!(events.len(), 2);
    let style = cue_of(&events[0]).style();
    // Старое выравнивание 6 = верх по центру.
    assert_eq!(
        style.alignment,
        AssAlignment {
            horizontal: AssHorizontalAlign::Center,
            vertical: AssVerticalAlign::Top
        }
    );
    // Контур берётся из BackColour (255 -> красный), AlphaLevel 64 — прозрачность цветов,
    // а у тени прозрачность всегда 0x80.
    assert_eq!(style.outline_color, AssColor::new(255, 0, 0, 64));
    assert_eq!(style.primary_color.transparency, 64);
    assert_eq!(style.back_color, AssColor::new(255, 0, 0, 0x80));
    // VSFilter: SSA-выравнивание 8 превращается в 3 (низ справа).
    assert_eq!(
        cue_of(&events[1]).style().alignment,
        AssAlignment {
            horizontal: AssHorizontalAlign::Right,
            vertical: AssVerticalAlign::Bottom
        }
    );
    // `Marked` не читается как слой.
    assert_eq!(events[0].layer().get(), 0);
}

#[test]
fn custom_event_format_order_and_actor_alias_are_honoured() {
    let loaded = load(
        "[V4+ Styles]\n\
         Format: Name, Fontname, Fontsize\n\
         Style: Narrator,Georgia,30\n\
         [Events]\n\
         Format: Start, End, Actor, Style, Text\n\
         Dialogue: 0:00:05.00,0:00:06.00,Alice,Narrator,Custom order\n",
    );
    let document = loaded.document();
    let event = &document.events()[0];
    assert_eq!(event.start(), SubtitleTime::from_millis(5000));
    let cue = cue_of(event);
    assert_eq!(cue.name, "Alice");
    assert_eq!(cue.style().name, "Narrator");
    assert_eq!(cue.style().font_name, "Georgia");
    // Нестандартный Format без явного ScaledBorderAndShadow включает его (совместимость).
    assert_eq!(
        document
            .ass_script()
            .unwrap()
            .info()
            .scaled_border_and_shadow,
        AssScaledBorderAndShadow::Enabled
    );
}

#[test]
fn broken_dialogue_line_is_skipped_and_counted_while_rest_loads() {
    let text = format!(
        "[Events]\n{EVENT_FORMAT}\n\
         Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,first\n\
         Dialogue: 0,0:00:02.00,0:00:03.00\n\
         Dialogue: 0,0:00:03.00,0:00:04.00,Default,,0,0,0,,third\n"
    );
    let loaded = load(&text);
    let document = loaded.document();
    assert_eq!(document.events().len(), 2);
    let report = document.report();
    assert_eq!(report.skipped_cues(), 1);
    let issue = &report.issues()[0];
    // Строка 4: [Events], Format, Dialogue, Битая.
    assert_eq!(issue.line_number.get(), 4);
    assert_eq!(issue.kind, ParseIssueKind::MalformedAssDialogue);
    let texts: Vec<_> = document
        .events()
        .iter()
        .map(|event| cue_of(event).text.clone())
        .collect();
    assert_eq!(
        texts,
        vec![
            vec![AssTextItem::Text("first".to_owned())],
            vec![AssTextItem::Text("third".to_owned())]
        ]
    );
}

#[test]
fn zero_duration_and_tag_only_events_are_skipped_with_distinct_counters() {
    let text = format!(
        "[Events]\n{EVENT_FORMAT}\n\
         Dialogue: 0,0:00:01.00,0:00:01.00,Default,,0,0,0,,zero length\n\
         Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{{\\an8}}\n\
         Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,visible\n"
    );
    let loaded = load(&text);
    let document = loaded.document();
    assert_eq!(document.events().len(), 1);
    assert_eq!(document.report().skipped_cues(), 1);
    assert_eq!(
        document.report().issues()[0].kind,
        ParseIssueKind::EndNotAfterStart
    );
    assert_eq!(document.report().empty_cues(), 1);
    // read_order — индекс среди принятых строк `Dialogue:` (как индекс события в libass).
    assert_eq!(cue_of(&document.events()[0]).read_order, 2);
}

#[test]
fn unknown_and_unsupported_tags_are_counted_and_text_is_kept() {
    let text = format!(
        "[Events]\n{EVENT_FORMAT}\n\
         Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{{\\zzz9\\fs20\\frz30\\move(1,2,3,4)\\k1}}Kept text\n"
    );
    let loaded = load(&text);
    let document = loaded.document();
    let cue = cue_of(&document.events()[0]);
    assert!(
        cue.text
            .contains(&AssTextItem::Text("Kept text".to_owned())),
        "{:?}",
        cue.text
    );
    let tags = &document.ass_report().unwrap().tags;
    assert_eq!(tags.unknown_total(), 1);
    assert_eq!(tags.unknown_names().get("zzz9"), Some(&1));
    assert_eq!(tags.unsupported_count(AssUnsupportedTag::Transform), 0);
    assert_eq!(tags.unsupported_count(AssUnsupportedTag::Karaoke), 1);
    // `\move` поддержан и в «не поддержано» не считается.
    assert_eq!(tags.unsupported().len(), 1);
}

#[test]
fn tag_order_traps_survive_a_full_file_roundtrip() {
    let text = format!(
        "[Events]\n{EVENT_FORMAT}\n\
         Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{{\\fscx120\\bord2\\shad3\\s1\\an8\\a6\\alpha&H80&\\fs+2\\fn}}x\n"
    );
    let loaded = load(&text);
    let cue = cue_of(&loaded.document().events()[0]);
    let Some(AssTextItem::Overrides(tags)) = cue.text.first() else {
        panic!("expected overrides first: {:?}", cue.text);
    };
    assert_eq!(tags[0], AssOverride::ScaleX(Some(1.2)));
    assert_eq!(
        tags[1],
        AssOverride::Border {
            axes: AssAxes::Both,
            value: Some(2.0)
        }
    );
    assert_eq!(
        tags[2],
        AssOverride::Shadow {
            axes: AssAxes::Both,
            value: Some(3.0)
        }
    );
    assert_eq!(tags[3], AssOverride::StrikeOut(Some(true)));
    assert_eq!(
        tags[4],
        AssOverride::Alignment(AssAlignment::from_numpad_tag(8))
    );
    assert_eq!(
        tags[5],
        AssOverride::Alignment(AssAlignment::from_legacy_tag(6))
    );
    assert_eq!(
        tags[6],
        AssOverride::Alpha {
            target: AssAlphaTarget::All,
            value: Some(0x80)
        }
    );
    assert_eq!(
        tags[7],
        AssOverride::FontSize(subtitle_core::AssFontSize::Relative(2.0))
    );
    assert_eq!(tags[8], AssOverride::FontName(None));
}

#[test]
fn breaks_nbsp_and_escaped_braces_in_a_file_event() {
    let text = format!(
        "[Events]\n{EVENT_FORMAT}\n\
         Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,one\\Ntwo\\nthree\\hfour \\{{x\\}}  \r\n"
    );
    let loaded = load(&text);
    let cue = cue_of(&loaded.document().events()[0]);
    assert_eq!(
        cue.text,
        vec![
            AssTextItem::Text("one".to_owned()),
            AssTextItem::HardBreak,
            AssTextItem::Text("two".to_owned()),
            AssTextItem::SoftBreak,
            AssTextItem::Text("three\u{a0}four {x}".to_owned()),
        ]
    );
}

#[test]
fn loaded_ass_events_play_through_a_real_timeline() {
    let loaded = load(&v4plus_script());
    let (timeline, build_report) =
        SubtitleTimeline::from_events(loaded.document().events().iter().cloned());
    assert_eq!(build_report.inserted, 2);
    let mut active = Vec::new();
    timeline.collect_active_at(SubtitleTime::from_millis(2500), &mut active);
    let names: Vec<_> = active
        .iter()
        .filter_map(|id| timeline.event(*id))
        .map(|event| cue_of(event).style().name.clone())
        .collect();
    // В 2.5 с одновременно видны обе реплики: нижний слой (0) раньше верхнего (2).
    assert_eq!(names, vec!["Default".to_owned(), "Sign".to_owned()]);
    active.clear();
    timeline.collect_active_at(SubtitleTime::from_millis(4500), &mut active);
    assert!(active.is_empty());
}

#[test]
fn sniffing_detects_ass_with_bom_blank_lines_and_any_case() {
    assert_eq!(
        sniff_text_subtitle_format("\u{feff}[Script Info]\nTitle: x\n"),
        Some(TextSubtitleFormat::Ass)
    );
    assert_eq!(
        sniff_text_subtitle_format("\n\n  [script info]\r\n"),
        Some(TextSubtitleFormat::Ass)
    );
    assert_eq!(sniff_text_subtitle_format("[Events]\nDialogue: x\n"), None);
    // SRT и WebVTT по-прежнему распознаются.
    assert_eq!(
        sniff_text_subtitle_format("1\n00:00:01,000 --> 00:00:02,000\nHi\n"),
        Some(TextSubtitleFormat::Srt)
    );
    assert_eq!(
        sniff_text_subtitle_format("WEBVTT\n"),
        Some(TextSubtitleFormat::WebVtt)
    );
}

#[test]
fn bom_prefixed_file_loads_through_the_whole_pipeline() {
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice(v4plus_script().as_bytes());
    let loaded = load_text_subtitle(
        &bytes,
        TextSubtitleFormat::Ass,
        LegacyEncodingHint::Generic,
        &limits(),
    )
    .unwrap();
    assert_eq!(loaded.document().events().len(), 2);
    assert_eq!(
        loaded.document().ass_script().unwrap().kind(),
        AssScriptKind::Ass
    );
}

#[test]
fn loading_ass_without_ass_limits_is_a_typed_error() {
    let error = try_load(&v4plus_script(), &ROOMY_LIMITS).unwrap_err();
    assert_eq!(error, SubtitleLoadError::AssLimitsMissing);
}

#[test]
fn too_many_events_is_fatal_and_styles_over_limit_are_counted() {
    let tiny_events = SubtitleParseLimits::new(1 << 20, 1, 100).with_ass_limits(ASS_LIMITS);
    let error = try_load(&v4plus_script(), &tiny_events).unwrap_err();
    assert_eq!(error, SubtitleLoadError::TooManyEvents { limit: 1 });

    let one_style = AssParseLimits {
        max_styles: 1,
        ..ASS_LIMITS
    };
    let loaded = try_load(&v4plus_script(), &ROOMY_LIMITS.with_ass_limits(one_style)).unwrap();
    let report = loaded.document().ass_report().unwrap();
    assert_eq!(report.header.skipped_styles, 1);
    // «Sign» пропущен: реплика с этим стилем падает на стиль по умолчанию.
    let sign_event = &loaded.document().events()[1];
    assert_eq!(cue_of(sign_event).style().name, "Default");
}

#[test]
fn override_tags_over_limit_are_ignored_and_counted() {
    let few_tags = AssParseLimits {
        max_override_tags_per_event: 2,
        ..ASS_LIMITS
    };
    let loaded = try_load(
        &format!(
            "[Events]\n{EVENT_FORMAT}\n\
             Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{{\\i1\\b1\\u1\\s1}}t\n"
        ),
        &ROOMY_LIMITS.with_ass_limits(few_tags),
    )
    .unwrap();
    let document = loaded.document();
    let cue = cue_of(&document.events()[0]);
    assert_eq!(
        cue.text[0],
        AssTextItem::Overrides(vec![
            AssOverride::Italic(Some(true)),
            AssOverride::Bold(Some(AssBold::Bold)),
        ])
    );
    assert_eq!(document.ass_report().unwrap().tags.over_limit(), 2);
}

// --- Заголовок дорожки (codec_private) и пакеты MKV ---

/// Заголовок из `crates/symphonia-demux/tests/fixtures/subtitles/generate.sh` (styled.ass,
/// без строк Dialogue — MKV хранит их пакетами).
const MKV_STYLED_HEADER: &str = "[Script Info]\n\
ScriptType: v4.00+\n\
PlayResX: 64\n\
PlayResY: 64\n\
\n\
[V4+ Styles]\n\
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
Style: Default,Noto Sans Hebrew,12,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,2,2,2,1\n\
\n\
[Events]\n\
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
Dialogue: 0,0:00:00.50,0:00:01.50,Default,,0,0,0,,Styled {\\b1}line{\\b0}\n";

fn embedded_limits() -> EmbeddedCueLimits {
    EmbeddedCueLimits::new(4096, 2048)
}

fn mkv_script() -> Arc<AssScript> {
    let (script, report) = parse_ass_script(MKV_STYLED_HEADER.as_bytes(), &ASS_LIMITS).unwrap();
    assert_eq!(report.skipped_styles, 0);
    script
}

#[test]
fn codec_private_header_and_mkv_chunk_produce_a_cue_with_style_and_read_order() {
    let script = mkv_script();
    assert_eq!(
        (script.info().play_res.width, script.info().play_res.height),
        (64, 64)
    );
    let mut stats = AssTagStats::new(&ASS_LIMITS);
    let event = parse_embedded_ass_event(
        b"7,3,Default,Bob,1,2,3,fx,Styled {\\b1}line{\\b0}, with comma",
        &script,
        &embedded_limits(),
        &ASS_LIMITS,
        &mut stats,
    )
    .unwrap();
    assert_eq!(event.read_order, 7);
    assert_eq!(event.layer.get(), 3);
    assert!(event.has_visible_text());
    assert!(!event.had_replacement_characters);
    let cue = &event.cue;
    assert_eq!(cue.read_order, 7);
    assert_eq!(cue.name, "Bob");
    assert_eq!(cue.effect, "fx");
    assert_eq!(cue.style().font_name, "Noto Sans Hebrew");
    let margins = cue.margins;
    assert_eq!((margins.left, margins.right, margins.vertical), (1, 2, 3));
    assert!(Arc::ptr_eq(&cue.script, &script));
    assert_eq!(
        cue.text,
        vec![
            AssTextItem::Text("Styled ".to_owned()),
            AssTextItem::Overrides(vec![AssOverride::Bold(Some(AssBold::Bold))]),
            AssTextItem::Text("line".to_owned()),
            AssTextItem::Overrides(vec![AssOverride::Bold(Some(AssBold::Normal))]),
            AssTextItem::Text(", with comma".to_owned()),
        ]
    );
}

#[test]
fn chunk_with_unknown_style_uses_the_default_style_and_counts_tags_in_accumulator() {
    let script = mkv_script();
    let mut stats = AssTagStats::new(&ASS_LIMITS);
    let event = parse_embedded_ass_event(
        b"0,0,*DEFAULT,,0,0,0,,{\\k2}a",
        &script,
        &embedded_limits(),
        &ASS_LIMITS,
        &mut stats,
    )
    .unwrap();
    assert_eq!(event.cue.style().name, "Default");
    let missing_style = parse_embedded_ass_event(
        b"1,0,NoSuchStyle,,0,0,0,,{\\k2}b",
        &script,
        &embedded_limits(),
        &ASS_LIMITS,
        &mut stats,
    )
    .unwrap();
    assert_eq!(missing_style.cue.style, script.default_style_index());
    // Накопитель суммирует по пакетам.
    assert_eq!(stats.unsupported_count(AssUnsupportedTag::Karaoke), 2);
}

#[test]
fn chunk_with_missing_fields_or_oversized_payload_is_rejected_with_typed_errors() {
    let script = mkv_script();
    let mut stats = AssTagStats::new(&ASS_LIMITS);
    let broken = parse_embedded_ass_event(
        b"0,0,Default",
        &script,
        &embedded_limits(),
        &ASS_LIMITS,
        &mut stats,
    );
    assert_eq!(
        broken,
        Err(EmbeddedCueError::Malformed(
            EmbeddedCueMalformation::AssMissingEventField
        ))
    );
    let no_layer =
        parse_embedded_ass_event(b"5", &script, &embedded_limits(), &ASS_LIMITS, &mut stats);
    assert_eq!(
        no_layer,
        Err(EmbeddedCueError::Malformed(
            EmbeddedCueMalformation::AssMissingEventField
        ))
    );
    let tiny = EmbeddedCueLimits::new(8, 100);
    let too_large = parse_embedded_ass_event(
        b"0,0,Default,,0,0,0,,long text",
        &script,
        &tiny,
        &ASS_LIMITS,
        &mut stats,
    );
    assert!(matches!(
        too_large,
        Err(EmbeddedCueError::PayloadTooLarge { limit_bytes: 8, .. })
    ));
    let few_chars = EmbeddedCueLimits::new(4096, 5);
    let too_long = parse_embedded_ass_event(
        b"0,0,Default,,0,0,0,,long text",
        &script,
        &few_chars,
        &ASS_LIMITS,
        &mut stats,
    );
    assert!(matches!(
        too_long,
        Err(EmbeddedCueError::TextTooLong { limit_chars: 5, .. })
    ));
}

#[test]
fn ssa_chunk_with_marked_field_and_invalid_utf8_is_repaired() {
    let (script, _) = parse_ass_script(
        b"[V4 Styles]\nStyle: Default,Arial,20\n[Events]\nFormat: Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        &ASS_LIMITS,
    )
    .unwrap();
    assert_eq!(script.kind(), AssScriptKind::Ssa);
    let mut stats = AssTagStats::new(&ASS_LIMITS);
    let mut payload = b"2,Marked=0,Default,,0,0,0,,bad ".to_vec();
    payload.push(0xff);
    let event = parse_embedded_ass_event(
        &payload,
        &script,
        &embedded_limits(),
        &ASS_LIMITS,
        &mut stats,
    )
    .unwrap();
    // Нечисловое `Marked` даёт слой 0.
    assert_eq!(event.layer.get(), 0);
    assert!(event.had_replacement_characters);
    assert_eq!(
        event.cue.text,
        vec![AssTextItem::Text("bad \u{fffd}".to_owned())]
    );
}

#[test]
fn tag_only_chunk_is_reported_as_not_visible() {
    let script = mkv_script();
    let mut stats = AssTagStats::new(&ASS_LIMITS);
    let event = parse_embedded_ass_event(
        b"0,0,Default,,0,0,0,,{\\an8}",
        &script,
        &embedded_limits(),
        &ASS_LIMITS,
        &mut stats,
    )
    .unwrap();
    assert!(!event.has_visible_text());
}

#[test]
fn header_without_event_format_uses_the_ass_fallback_layout() {
    let (script, _) =
        parse_ass_script(b"[V4+ Styles]\nStyle: Default,Arial,20\n", &ASS_LIMITS).unwrap();
    let mut stats = AssTagStats::new(&ASS_LIMITS);
    let event = parse_embedded_ass_event(
        b"4,1,Default,,0,0,0,,text",
        &script,
        &embedded_limits(),
        &ASS_LIMITS,
        &mut stats,
    )
    .unwrap();
    assert_eq!(event.layer.get(), 1);
    assert_eq!(event.cue.text, vec![AssTextItem::Text("text".to_owned())]);
}

#[test]
fn oversized_header_is_a_typed_error_before_parsing() {
    let tiny = AssParseLimits {
        max_header_bytes: 16,
        ..ASS_LIMITS
    };
    let error = parse_ass_script(MKV_STYLED_HEADER.as_bytes(), &tiny).unwrap_err();
    assert_eq!(
        error,
        AssParseError::HeaderTooLarge {
            actual_bytes: MKV_STYLED_HEADER.len(),
            limit_bytes: 16
        }
    );
}

#[test]
fn header_with_invalid_utf8_is_repaired_and_flagged() {
    let mut bytes = b"[V4+ Styles]\nStyle: Name".to_vec();
    bytes.push(0xff);
    bytes.extend_from_slice(b",Arial,20\n");
    let (script, report) = parse_ass_script(&bytes, &ASS_LIMITS).unwrap();
    assert!(report.had_replacement_characters);
    assert!(script.lookup_style_strict("Name\u{fffd}").is_some());
}

#[test]
fn hard_overrides_are_detected_in_the_raw_text_even_when_the_tag_is_malformed() {
    let text = format!(
        "[Events]\n{EVENT_FORMAT}\n\
         Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{{\\pos(1)}}Broken pos\n\
         Dialogue: 0,0:00:03.00,0:00:04.00,Default,,0,0,0,,{{\\b1}}Plain\n"
    );
    let loaded = load(&text);
    let events = loaded.document().events();
    assert_eq!(events.len(), 2);
    // `\pos(1)` разбор отбросил (нужно два числа), но libass считает реплику «явной».
    assert_eq!(cue_of(&events[0]).hard_overrides, AssHardOverrides::Present);
    assert_eq!(cue_of(&events[1]).hard_overrides, AssHardOverrides::Absent);
}

#[test]
fn sidecar_header_over_the_limit_is_rejected_but_a_long_events_section_is_fine() {
    let header = "[Script Info]\nScriptType: v4.00+\n";
    let events = format!(
        "[Events]\n{EVENT_FORMAT}\n\
         Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Text\n"
    );
    let limits_for = |max_header_bytes: usize| {
        ROOMY_LIMITS.with_ass_limits(AssParseLimits {
            max_header_bytes,
            ..ASS_LIMITS
        })
    };
    let padded = format!("{header}{}{events}", "; padding\n".repeat(100));
    let header_len = header.len() + "; padding\n".len() * 100;

    // Ровно по лимиту: принимается; на байт меньше: типизированная ошибка с размерами.
    assert!(try_load(&padded, &limits_for(header_len)).is_ok());
    assert_eq!(
        try_load(&padded, &limits_for(header_len - 1)).unwrap_err(),
        SubtitleLoadError::AssHeaderTooLarge {
            actual_bytes: header_len,
            limit_bytes: header_len - 1,
        }
    );
    // Много событий при маленьком заголовке лимит заголовка не трогают.
    let many_events = format!(
        "{header}{events}{}",
        "Dialogue: 0,0:00:03.00,0:00:04.00,Default,,0,0,0,,More\n".repeat(200)
    );
    assert!(try_load(&many_events, &limits_for(header.len())).is_ok());
}
