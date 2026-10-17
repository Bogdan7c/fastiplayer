//! Определение текстового формата по содержимому (расширению файла доверять нельзя).

use crate::{ass, srt, webvtt};

/// Текстовые форматы, которые умеет разбирать этот crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TextSubtitleFormat {
    Srt,
    WebVtt,
    /// ASS и SSA (версия определяется содержимым сценария).
    Ass,
}

/// Сколько первых строк первого блока проверяем на строку времени:
/// SRT — это либо «время», либо «номер, время».
const SRT_TIMING_LINE_SEARCH_DEPTH: usize = 2;

/// Определяет формат по уже декодированному тексту. `None` — формат не распознан.
///
/// WebVTT: текст начинается с `WEBVTT`, за которым конец текста, пробел, табуляция или перевод строки.
/// ASS/SSA: первая непустая строка (после необязательного BOM и пробелов) — `[Script Info]`
/// без учёта регистра.
/// SRT: первый непустой блок содержит строку времени `start --> end` с валидными
/// SRT-таймстемпами в одной из его первых двух строк.
pub fn sniff_text_subtitle_format(text: &str) -> Option<TextSubtitleFormat> {
    // Сигнатура WebVTT однозначна, поэтому проверяется первой (в его заголовке
    // может оказаться строка со стрелкой, похожая на SRT).
    if webvtt::has_webvtt_signature(text) {
        return Some(TextSubtitleFormat::WebVtt);
    }
    if ass::has_ass_signature(text) {
        return Some(TextSubtitleFormat::Ass);
    }
    let first_block_lines = text
        .lines()
        .skip_while(|line| line.trim().is_empty())
        .take_while(|line| !line.trim().is_empty())
        .take(SRT_TIMING_LINE_SEARCH_DEPTH);
    let has_srt_timing_line = first_block_lines
        .into_iter()
        .any(|line| srt::parse_timing_line(line).is_ok());
    has_srt_timing_line.then_some(TextSubtitleFormat::Srt)
}
