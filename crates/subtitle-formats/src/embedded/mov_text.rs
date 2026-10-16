//! MP4 `tx3g` / mov_text (3GPP TS 26.245, раздел 5.17): образец (sample) = текст + блоки.
//!
//! Раскладка (всё big-endian): `u16` длина текста, сам текст, затем необязательные блоки
//! `u32 size, [u8;4] type, ...`. Текст — UTF-8, либо UTF-16 при метке порядка байтов
//! `FE FF` (BE) / `FF FE` (LE; в стандарте только BE, LE принимаем как послабление).
//! Нулевая длина — маркер «пустого промежутка» (исход `Empty`, не ошибка).
//!
//! Блок `styl`: `u16` число записей, каждая 12 байт —
//! `u16 startChar, u16 endChar, u16 fontID, u8 faceStyleFlags, u8 fontSize, [u8;4] RGBA`.
//!
//! РЕШЕНИЕ (смещения): `startChar`/`endChar` — в СИМВОЛАХ (Unicode scalar values) декодированного
//! текста, `endChar` не включается; так читает стандарт для UTF-16 и так же поступает ffmpeg для
//! UTF-8 (считает не байты, а символы). Диапазон за пределами текста обрезается по длине текста.
//!
//! РЕШЕНИЕ (цвет): непрозрачный белый `FFFFFFFF` — значение 3GPP «по умолчанию», его пишут
//! почти все кодировщики; применять его как авторский цвет значило бы перебивать выбор
//! пользователя на любом тёмном фоне, поэтому он трактуется как «цвет не задан».
//! Прочие остальные блоки (`hlit`, `tbox`, `krok`...) игнорируются.

use encoding_rs::{UTF_16BE, UTF_16LE};
use subtitle_core::{
    CuePlacement, FontSlant, FontWeight, SpanStyle, SubtitleColor, SubtitleLayer, TextSpan,
    TextUnderline,
};

use super::text_lines::{check_char_limit, decode_utf8_lossy, finish_cue, trim_blank_edges};
use super::{EmbeddedCueError, EmbeddedCueLimits, EmbeddedCueMalformation, EmbeddedCueOutcome};

const LENGTH_PREFIX_BYTES: usize = 2;
const UTF16_BE_BOM: [u8; 2] = [0xFE, 0xFF];
const UTF16_LE_BOM: [u8; 2] = [0xFF, 0xFE];
/// Заголовок блока: размер (4) + тип (4).
const BOX_HEADER_BYTES: usize = 8;
const STYL_BOX_TYPE: &[u8; 4] = b"styl";
/// Размер записи стиля в `styl`.
const STYLE_RECORD_BYTES: usize = 12;
/// Смещение числа записей внутри тела `styl`.
const STYL_COUNT_BYTES: usize = 2;
const FACE_BOLD: u8 = 0x01;
const FACE_ITALIC: u8 = 0x02;
const FACE_UNDERLINE: u8 = 0x04;
const DEFAULT_WHITE: SubtitleColor = SubtitleColor::opaque(0xFF, 0xFF, 0xFF);

/// Одна запись `styl`, уже переведённая в наш стиль.
struct StyleRecord {
    start_char: usize,
    end_char: usize,
    style: SpanStyle,
}

pub(super) fn parse_mov_text_sample(
    sample: &[u8],
    limits: &EmbeddedCueLimits,
) -> Result<EmbeddedCueOutcome, EmbeddedCueError> {
    let Some(prefix) = sample.get(..LENGTH_PREFIX_BYTES) else {
        return Err(EmbeddedCueError::Malformed(
            EmbeddedCueMalformation::MovTextMissingLength,
        ));
    };
    let declared = usize::from(u16::from_be_bytes([prefix[0], prefix[1]]));
    let body = sample.get(LENGTH_PREFIX_BYTES..).unwrap_or_default();
    let Some(text_bytes) = body.get(..declared) else {
        return Err(EmbeddedCueError::Malformed(
            EmbeddedCueMalformation::MovTextLengthExceedsPayload {
                declared,
                available: body.len(),
            },
        ));
    };
    if declared == 0 {
        return Ok(EmbeddedCueOutcome::Empty);
    }
    let boxes = body.get(declared..).unwrap_or_default();
    let (text, had_replacement_characters) = decode_text(text_bytes)?;
    check_char_limit(&text, limits)?;
    let (records, had_invalid_styling) = parse_style_boxes(boxes);
    let spans = build_spans(&text, &records);
    Ok(finish_cue(
        trim_blank_edges(spans),
        CuePlacement::default(),
        SubtitleLayer::DEFAULT,
        had_replacement_characters,
        had_invalid_styling,
    ))
}

/// UTF-16 при BOM, иначе UTF-8 с заменой. Возвращает текст без BOM и флаг замен.
fn decode_text(text_bytes: &[u8]) -> Result<(String, bool), EmbeddedCueError> {
    let utf16 = if text_bytes.starts_with(&UTF16_BE_BOM) {
        Some((UTF_16BE, UTF16_BE_BOM.len()))
    } else if text_bytes.starts_with(&UTF16_LE_BOM) {
        Some((UTF_16LE, UTF16_LE_BOM.len()))
    } else {
        None
    };
    match utf16 {
        Some((encoding, bom_length)) => {
            let payload = text_bytes.get(bom_length..).unwrap_or_default();
            encoding
                .decode_without_bom_handling_and_without_replacement(payload)
                .map(|text| (text.into_owned(), false))
                .ok_or(EmbeddedCueError::InvalidEncoding)
        }
        None => {
            let decoded = decode_utf8_lossy(text_bytes);
            Ok((decoded.text, decoded.had_replacement_characters))
        }
    }
}

/// Обходит блоки после текста и собирает записи из `styl`.
/// Второе значение `true`, если блок оборван/искажён (стили до порчи сохраняются).
fn parse_style_boxes(mut boxes: &[u8]) -> (Vec<StyleRecord>, bool) {
    let mut records = Vec::new();
    while !boxes.is_empty() {
        let Some(header) = boxes.get(..BOX_HEADER_BYTES) else {
            return (records, true);
        };
        let size = usize::try_from(u32::from_be_bytes([
            header[0], header[1], header[2], header[3],
        ]))
        .unwrap_or(usize::MAX);
        let Some(whole_box) = boxes.get(..size).filter(|_| size >= BOX_HEADER_BYTES) else {
            return (records, true);
        };
        if header.get(4..BOX_HEADER_BYTES) == Some(STYL_BOX_TYPE.as_slice()) {
            let body = whole_box.get(BOX_HEADER_BYTES..).unwrap_or_default();
            match parse_styl_body(body) {
                Some(parsed) => records.extend(parsed),
                None => return (records, true),
            }
        }
        boxes = boxes.get(size..).unwrap_or_default();
    }
    (records, false)
}

/// Тело `styl` -> записи; `None`, если число записей не помещается в тело.
fn parse_styl_body(body: &[u8]) -> Option<Vec<StyleRecord>> {
    let count_bytes = body.get(..STYL_COUNT_BYTES)?;
    let count = usize::from(u16::from_be_bytes([count_bytes[0], count_bytes[1]]));
    let entries = body.get(STYL_COUNT_BYTES..)?;
    let entries = entries.get(..count.checked_mul(STYLE_RECORD_BYTES)?)?;
    let (records, _) = entries.as_chunks::<STYLE_RECORD_BYTES>();
    Some(records.iter().map(read_record).collect())
}

/// Одна запись ровно по 12 байт (гарантировано типом массива).
fn read_record(entry: &[u8; STYLE_RECORD_BYTES]) -> StyleRecord {
    let byte = |index: usize| entry.get(index).copied().unwrap_or_default();
    let word = |index: usize| usize::from(u16::from_be_bytes([byte(index), byte(index + 1)]));
    let face = byte(6);
    let color = SubtitleColor {
        red: byte(8),
        green: byte(9),
        blue: byte(10),
        alpha: byte(11),
    };
    StyleRecord {
        start_char: word(0),
        end_char: word(2),
        style: SpanStyle {
            weight: if face & FACE_BOLD != 0 {
                FontWeight::Bold
            } else {
                FontWeight::Normal
            },
            slant: if face & FACE_ITALIC != 0 {
                FontSlant::Italic
            } else {
                FontSlant::Normal
            },
            underline: if face & FACE_UNDERLINE != 0 {
                TextUnderline::Single
            } else {
                TextUnderline::None
            },
            color: (color != DEFAULT_WHITE).then_some(color),
            ..SpanStyle::default()
        },
    }
}

/// Стиль символа с номером `index`: флаги складываются (OR), цвет берётся у последней записи.
fn style_at(records: &[StyleRecord], index: usize) -> SpanStyle {
    let mut style = SpanStyle::default();
    for record in records
        .iter()
        .filter(|record| record.start_char <= index && index < record.end_char)
    {
        if record.style.weight == FontWeight::Bold {
            style.weight = FontWeight::Bold;
        }
        if record.style.slant == FontSlant::Italic {
            style.slant = FontSlant::Italic;
        }
        if record.style.underline == TextUnderline::Single {
            style.underline = TextUnderline::Single;
        }
        if record.style.color.is_some() {
            style.color = record.style.color;
        }
    }
    style
}

/// Посимвольно применяет стили, склеивая соседние символы одного стиля в спан.
fn build_spans(text: &str, records: &[StyleRecord]) -> Vec<TextSpan> {
    let mut spans = Vec::new();
    let mut pending = String::new();
    let mut pending_style = SpanStyle::default();
    let mut chars = text.chars().enumerate().peekable();
    while let Some((index, ch)) = chars.next() {
        let is_break = match ch {
            '\n' => true,
            // CRLF — один перевод строки; одиночный CR тоже перевод.
            '\r' => chars.peek().map(|(_, next)| *next) != Some('\n'),
            _ => false,
        };
        if ch == '\r' && !is_break {
            continue;
        }
        let style = style_at(records, index);
        if is_break || style != pending_style {
            flush(&mut spans, &mut pending, pending_style);
            pending_style = style;
        }
        if is_break {
            spans.push(TextSpan::line_break());
        } else {
            pending.push(ch);
        }
    }
    flush(&mut spans, &mut pending, pending_style);
    spans
}

fn flush(spans: &mut Vec<TextSpan>, pending: &mut String, style: SpanStyle) {
    if !pending.is_empty() {
        spans.push(TextSpan::text(std::mem::take(pending), style));
    }
}
