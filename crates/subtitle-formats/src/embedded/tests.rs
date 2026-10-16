//! Тесты разбора пакетов контейнера: каждый формат проверяется по наблюдаемому результату
//! (текст, переводы строк, стили, слой, виды ошибок).

use subtitle_core::{
    FontSlant, FontWeight, ScreenAnchor, SpanContent, SpanStyle, SubtitleColor, SubtitleLayer,
    TextUnderline,
};

use super::*;

const ROOMY: EmbeddedCueLimits = EmbeddedCueLimits::new(4096, 2048);

fn parse(payload: &[u8], format: EmbeddedCueFormat) -> EmbeddedCueOutcome {
    parse_embedded_cue(payload, format, &ROOMY).expect("payload must parse")
}

fn expect_cue(outcome: EmbeddedCueOutcome) -> EmbeddedCue {
    match outcome {
        EmbeddedCueOutcome::Cue(cue) => cue,
        EmbeddedCueOutcome::Empty => panic!("expected a cue, got Empty"),
    }
}

/// (текст, стиль) всех текстовых спанов; перевод строки — `("\n", default)`.
fn flat_spans(cue: &EmbeddedCue) -> Vec<(String, SpanStyle)> {
    cue.cue
        .text
        .spans()
        .iter()
        .map(|span| match &span.content {
            SpanContent::Text(text) => (text.clone(), span.style),
            SpanContent::LineBreak => ("\n".to_owned(), SpanStyle::default()),
        })
        .collect()
}

// ---------- SRT ----------

#[test]
fn srt_text_keeps_markup_styles_and_line_breaks() {
    let cue = expect_cue(parse(
        b"Hello <b>bold</b>\r\n<i>second</i> line",
        EmbeddedCueFormat::SrtText,
    ));
    assert_eq!(cue.cue.text.plain_text(), "Hello bold\nsecond line");
    let spans = flat_spans(&cue);
    assert_eq!(spans[1].0, "bold");
    assert_eq!(spans[1].1.weight, FontWeight::Bold);
    assert_eq!(spans[3].0, "second");
    assert_eq!(spans[3].1.slant, FontSlant::Italic);
    assert_eq!(spans[0].1, SpanStyle::default());
    assert!(!cue.had_replacement_characters);
}

#[test]
fn srt_text_anchor_block_sets_placement_and_is_stripped() {
    let cue = expect_cue(parse(b"{\\an8}Top text", EmbeddedCueFormat::SrtText));
    assert_eq!(cue.cue.text.plain_text(), "Top text");
    assert_eq!(cue.cue.placement.anchor, Some(ScreenAnchor::TopCenter));
}

#[test]
fn srt_text_invalid_utf8_is_repaired_and_reported() {
    let cue = expect_cue(parse(b"ab\xFFcd", EmbeddedCueFormat::SrtText));
    assert_eq!(cue.cue.text.plain_text(), "ab\u{FFFD}cd");
    assert!(cue.had_replacement_characters);
}

#[test]
fn srt_text_blank_payload_is_empty_outcome() {
    assert_eq!(
        parse(b" \r\n ", EmbeddedCueFormat::SrtText),
        EmbeddedCueOutcome::Empty
    );
}

// ---------- WebVTT ----------

#[test]
fn webvtt_cue_text_parses_tags_entities_and_default_placement() {
    let cue = expect_cue(parse(
        b"<i>Hi</i> &amp; bye\nsecond",
        EmbeddedCueFormat::WebVttCueText,
    ));
    assert_eq!(cue.cue.text.plain_text(), "Hi & bye\nsecond");
    assert_eq!(flat_spans(&cue)[0].1.slant, FontSlant::Italic);
    assert_eq!(cue.cue.placement, subtitle_core::CuePlacement::default());
    assert_eq!(cue.layer, SubtitleLayer::DEFAULT);
}

#[test]
fn webvtt_cue_text_karaoke_timestamps_do_not_delay_text() {
    let cue = expect_cue(parse(
        b"one <00:00:01.000>two",
        EmbeddedCueFormat::WebVttCueText,
    ));
    assert!(
        cue.cue
            .text
            .spans()
            .iter()
            .all(|span| span.reveal_at.is_none())
    );
    assert_eq!(cue.cue.text.plain_text(), "one two");
}

// ---------- ASS / SSA ----------

#[test]
fn ass_line_takes_only_text_field_with_commas_and_strips_overrides() {
    let line = b"12,0,Default,,0,0,0,,{\\i1}Hello, {\\b1}world{\\b0}\\Nsecond\\nthird\\hx";
    let cue = expect_cue(parse(line, EmbeddedCueFormat::AssEventLine));
    assert_eq!(
        cue.cue.text.plain_text(),
        "Hello, world\nsecond\nthird\u{a0}x"
    );
    assert!(
        flat_spans(&cue)
            .iter()
            .all(|(_, style)| *style == SpanStyle::default())
    );
}

#[test]
fn ass_line_exposes_layer() {
    let cue = expect_cue(parse(
        b"0,7,Default,,0,0,0,,text",
        EmbeddedCueFormat::AssEventLine,
    ));
    assert_eq!(cue.layer, SubtitleLayer::new(7));
}

#[test]
fn ssa_marked_field_in_layer_slot_falls_back_to_default_layer() {
    let cue = expect_cue(parse(
        b"0,Marked=0,Default,,0,0,0,,text",
        EmbeddedCueFormat::AssEventLine,
    ));
    assert_eq!(cue.layer, SubtitleLayer::DEFAULT);
    assert_eq!(cue.cue.text.plain_text(), "text");
}

#[test]
fn ass_line_unclosed_brace_is_literal_and_unknown_escape_kept() {
    let cue = expect_cue(parse(
        b"0,0,D,,0,0,0,,a {b \\q",
        EmbeddedCueFormat::AssEventLine,
    ));
    assert_eq!(cue.cue.text.plain_text(), "a {b \\q");
}

#[test]
fn ass_line_with_too_few_fields_is_malformed() {
    let error = parse_embedded_cue(b"0,0,Default,text", EmbeddedCueFormat::AssEventLine, &ROOMY)
        .expect_err("must fail");
    assert_eq!(
        error,
        EmbeddedCueError::Malformed(EmbeddedCueMalformation::AssTooFewFields {
            expected: 9,
            found: 4
        })
    );
}

#[test]
fn ass_line_with_only_override_tags_is_empty() {
    assert_eq!(
        parse(b"0,0,D,,0,0,0,,{\\an8}", EmbeddedCueFormat::AssEventLine),
        EmbeddedCueOutcome::Empty
    );
}

// ---------- mov_text ----------

fn mov_sample(text: &[u8], boxes: &[u8]) -> Vec<u8> {
    let mut sample = (text.len() as u16).to_be_bytes().to_vec();
    sample.extend_from_slice(text);
    sample.extend_from_slice(boxes);
    sample
}

fn style_record(start: u16, end: u16, face: u8, rgba: [u8; 4]) -> Vec<u8> {
    let mut record = Vec::new();
    record.extend_from_slice(&start.to_be_bytes());
    record.extend_from_slice(&end.to_be_bytes());
    record.extend_from_slice(&1u16.to_be_bytes()); // fontID
    record.push(face);
    record.push(18); // fontSize
    record.extend_from_slice(&rgba);
    record
}

fn styl_box(records: &[Vec<u8>]) -> Vec<u8> {
    let mut body = (records.len() as u16).to_be_bytes().to_vec();
    for record in records {
        body.extend_from_slice(record);
    }
    let mut boxed = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    boxed.extend_from_slice(b"styl");
    boxed.extend_from_slice(&body);
    boxed
}

#[test]
fn mov_text_plain_utf8_without_boxes() {
    let cue = expect_cue(parse(
        &mov_sample("Привет\nмир".as_bytes(), &[]),
        EmbeddedCueFormat::MovText,
    ));
    assert_eq!(cue.cue.text.plain_text(), "Привет\nмир");
    assert!(!cue.had_invalid_styling);
}

#[test]
fn mov_text_styl_applies_bold_italic_underline_and_color_by_characters() {
    // «Привет мир»: кириллица двухбайтная, смещения обязаны считаться в символах.
    let styl = styl_box(&[
        style_record(0, 6, 0x01 | 0x04, [255, 255, 255, 255]),
        style_record(7, 10, 0x02, [255, 0, 0, 255]),
    ]);
    let cue = expect_cue(parse(
        &mov_sample("Привет мир".as_bytes(), &styl),
        EmbeddedCueFormat::MovText,
    ));
    let spans = flat_spans(&cue);
    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].0, "Привет");
    assert_eq!(spans[0].1.weight, FontWeight::Bold);
    assert_eq!(spans[0].1.underline, TextUnderline::Single);
    assert_eq!(spans[0].1.slant, FontSlant::Normal);
    // Белый 3GPP по умолчанию не считается авторским цветом.
    assert_eq!(spans[0].1.color, None);
    assert_eq!(spans[1], (" ".to_owned(), SpanStyle::default()));
    assert_eq!(spans[2].0, "мир");
    assert_eq!(spans[2].1.slant, FontSlant::Italic);
    assert_eq!(spans[2].1.color, Some(SubtitleColor::opaque(255, 0, 0)));
    assert_eq!(spans[2].1.weight, FontWeight::Normal);
}

#[test]
fn mov_text_style_range_past_text_is_clamped_and_inverted_range_ignored() {
    let styl = styl_box(&[
        style_record(2, 999, 0x01, [255; 4]),
        style_record(1, 1, 0x02, [255; 4]),
    ]);
    let cue = expect_cue(parse(
        &mov_sample(b"abcd", &styl),
        EmbeddedCueFormat::MovText,
    ));
    let spans = flat_spans(&cue);
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0], ("ab".to_owned(), SpanStyle::default()));
    assert_eq!(spans[1].0, "cd");
    assert_eq!(spans[1].1.weight, FontWeight::Bold);
    assert_eq!(spans[1].1.slant, FontSlant::Normal);
}

#[test]
fn mov_text_utf16_with_bom_is_decoded_and_styled_by_characters() {
    let mut text = vec![0xFE, 0xFF];
    for unit in "a😀b".encode_utf16() {
        text.extend_from_slice(&unit.to_be_bytes());
    }
    // Эмодзи — один символ (две кодовые единицы UTF-16): стиль на символ №1.
    let styl = styl_box(&[style_record(1, 2, 0x01, [255; 4])]);
    let cue = expect_cue(parse(&mov_sample(&text, &styl), EmbeddedCueFormat::MovText));
    let spans = flat_spans(&cue);
    assert_eq!(cue.cue.text.plain_text(), "a😀b");
    assert_eq!(spans[1].0, "😀");
    assert_eq!(spans[1].1.weight, FontWeight::Bold);
    assert_eq!(spans[2].1.weight, FontWeight::Normal);
}

#[test]
fn mov_text_utf16_odd_length_is_invalid_encoding() {
    let error = parse_embedded_cue(
        &mov_sample(&[0xFE, 0xFF, 0x00, 0x61, 0x00], &[]),
        EmbeddedCueFormat::MovText,
        &ROOMY,
    )
    .expect_err("must fail");
    assert_eq!(error, EmbeddedCueError::InvalidEncoding);
}

#[test]
fn mov_text_zero_length_is_empty_outcome_not_error() {
    assert_eq!(
        parse(&[0, 0], EmbeddedCueFormat::MovText),
        EmbeddedCueOutcome::Empty
    );
}

#[test]
fn mov_text_malformed_prefix_and_declared_length() {
    assert_eq!(
        parse_embedded_cue(&[0], EmbeddedCueFormat::MovText, &ROOMY),
        Err(EmbeddedCueError::Malformed(
            EmbeddedCueMalformation::MovTextMissingLength
        ))
    );
    assert_eq!(
        parse_embedded_cue(&[0, 9, b'a'], EmbeddedCueFormat::MovText, &ROOMY),
        Err(EmbeddedCueError::Malformed(
            EmbeddedCueMalformation::MovTextLengthExceedsPayload {
                declared: 9,
                available: 1
            }
        ))
    );
}

#[test]
fn mov_text_truncated_styl_keeps_text_and_reports_invalid_styling() {
    let mut styl = styl_box(&[style_record(0, 2, 0x01, [255; 4])]);
    styl.truncate(styl.len() - 3);
    let cue = expect_cue(parse(&mov_sample(b"ab", &styl), EmbeddedCueFormat::MovText));
    assert_eq!(cue.cue.text.plain_text(), "ab");
    assert!(cue.had_invalid_styling);
    assert_eq!(flat_spans(&cue)[0].1, SpanStyle::default());
}

#[test]
fn mov_text_unknown_boxes_are_skipped_before_styl() {
    let mut boxes = vec![0, 0, 0, 12];
    boxes.extend_from_slice(b"hlit");
    boxes.extend_from_slice(&[0, 0, 0, 1]);
    boxes.extend_from_slice(&styl_box(&[style_record(0, 1, 0x02, [255; 4])]));
    let cue = expect_cue(parse(
        &mov_sample(b"ab", &boxes),
        EmbeddedCueFormat::MovText,
    ));
    assert!(!cue.had_invalid_styling);
    assert_eq!(flat_spans(&cue)[0].1.slant, FontSlant::Italic);
}

// ---------- лимиты ----------

#[test]
fn payload_over_byte_limit_is_rejected_before_decoding() {
    let limits = EmbeddedCueLimits::new(4, 100);
    for format in [
        EmbeddedCueFormat::SrtText,
        EmbeddedCueFormat::WebVttCueText,
        EmbeddedCueFormat::AssEventLine,
        EmbeddedCueFormat::MovText,
    ] {
        assert_eq!(
            parse_embedded_cue(b"12345", format, &limits),
            Err(EmbeddedCueError::PayloadTooLarge {
                actual_bytes: 5,
                limit_bytes: 4
            })
        );
    }
}

#[test]
fn text_over_char_limit_counts_characters_not_bytes() {
    let limits = EmbeddedCueLimits::new(100, 3);
    // 3 символа (6 байт) помещаются, 4 — нет.
    assert!(parse_embedded_cue("ппп".as_bytes(), EmbeddedCueFormat::SrtText, &limits).is_ok());
    assert_eq!(
        parse_embedded_cue("пппп".as_bytes(), EmbeddedCueFormat::SrtText, &limits),
        Err(EmbeddedCueError::TextTooLong {
            actual_chars: 4,
            limit_chars: 3
        })
    );
    assert_eq!(
        parse_embedded_cue(
            &mov_sample("пппп".as_bytes(), &[]),
            EmbeddedCueFormat::MovText,
            &limits
        ),
        Err(EmbeddedCueError::TextTooLong {
            actual_chars: 4,
            limit_chars: 3
        })
    );
}
