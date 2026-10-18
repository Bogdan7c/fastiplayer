//! Выбор текстового формата файла субтитров: содержимое важнее расширения.
//!
//! Файлы часто переименовывают (ASS под именем `Movie.srt`, SRT как `.ass`). Если слушаться
//! только расширения, подходящий разборщик не получит свой формат и вернёт пустую дорожку
//! (или откажется с «не WebVTT»). Поэтому по первым байтам определяем формат сами
//! (`subtitle_formats::sniff_text_subtitle_format`); расширение остаётся запасным вариантом,
//! когда содержимое ничего однозначного не говорит.

use subtitle_formats::{
    LegacyEncodingHint, TextSubtitleFormat, decode_subtitle_bytes, sniff_text_subtitle_format,
};

/// Сколько первых байт достаточно для опознания формата: сигнатуры ASS/WebVTT стоят в самом
/// начале, а таймстемп SRT — в первых двух строках первого блока. Меньший префикс не
/// заставляет декодировать (и копировать) файл в сотни мегабайт ещё раз.
const FORMAT_SNIFF_PREFIX_BYTES: usize = 8 * 1024;

/// Формат, по которому надо разбирать файл: определённый по содержимому, иначе из расширения.
///
/// Подменяем расширение только форматами, которые умеют файлы рядом с видео (SRT, WebVTT, ASS):
/// другое опознание (TTML, srv3) для файла с таким именем — не повод менять разборщик.
pub(super) fn resolve_text_format(
    format_from_extension: TextSubtitleFormat,
    bytes: &[u8],
    encoding_hint: LegacyEncodingHint,
) -> TextSubtitleFormat {
    let Some(format_from_content) = sniff_sidecar_format(bytes, encoding_hint) else {
        return format_from_extension;
    };
    if format_from_content != format_from_extension {
        // Имя файла (и тем более каталог) в лог не пишем: только сам факт подмены.
        tracing::debug!(
            from_extension = ?format_from_extension,
            from_content = ?format_from_content,
            "Subtitle content overrides the file extension"
        );
    }
    format_from_content
}

/// Формат по началу файла; `None` — содержимое не опознано как формат файла субтитров.
fn sniff_sidecar_format(
    bytes: &[u8],
    encoding_hint: LegacyEncodingHint,
) -> Option<TextSubtitleFormat> {
    // Четная граница: обрезанная посреди символа UTF-16 пара байт испортила бы весь префикс.
    let prefix_len = bytes.len().min(FORMAT_SNIFF_PREFIX_BYTES) & !1;
    let prefix = bytes.get(..prefix_len).unwrap_or(bytes);
    let prefix_text = decode_subtitle_bytes(prefix, encoding_hint).text;
    match sniff_text_subtitle_format(&prefix_text)? {
        recognized @ (TextSubtitleFormat::Srt
        | TextSubtitleFormat::WebVtt
        | TextSubtitleFormat::Ass) => Some(recognized),
        // TTML/srv3 в файле рядом с видео не поддержаны: оставляем решение расширению.
        _ => None,
    }
}
