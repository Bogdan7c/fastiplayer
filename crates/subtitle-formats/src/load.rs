//! Точка входа: байты файла -> декодирование -> разбор формата.

use crate::ass::file::parse_ass_document;
use crate::document::ParsedSubtitleDocument;
use crate::encoding::{DetectedSubtitleEncoding, LegacyEncodingHint, decode_subtitle_bytes};
use crate::error::SubtitleLoadError;
use crate::limits::SubtitleParseLimits;
use crate::sniff::TextSubtitleFormat;
use crate::srt::parse_srt;
use crate::srv3::parse_srv3;
use crate::webvtt::parse_webvtt;

/// Загруженный файл субтитров вместе со сведениями о декодировании.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedTextSubtitle {
    document: ParsedSubtitleDocument,
    encoding: DetectedSubtitleEncoding,
    had_replacement_characters: bool,
}

impl LoadedTextSubtitle {
    pub fn document(&self) -> &ParsedSubtitleDocument {
        &self.document
    }

    /// Какая кодировка использована (для подсказки «прочитано как windows-1251»).
    pub fn encoding(&self) -> DetectedSubtitleEncoding {
        self.encoding
    }

    /// При декодировании часть байтов была заменена на U+FFFD.
    pub fn had_replacement_characters(&self) -> bool {
        self.had_replacement_characters
    }

    pub fn into_document(self) -> ParsedSubtitleDocument {
        self.document
    }
}

/// Загружает текстовые субтитры заданного формата.
///
/// Для `TextSubtitleFormat::Ass` в `limits` обязаны быть лимиты ASS
/// ([`SubtitleParseLimits::with_ass_limits`]); события результата — `SubtitleContent::Ass`,
/// а сценарий доступен через `ParsedSubtitleDocument::ass_script`.
///
/// Размер входа проверяется ДО декодирования: враждебный файл не должен успеть
/// выделить память под текст. Проблемы отдельных реплик идут в отчёт документа.
pub fn load_text_subtitle(
    bytes: &[u8],
    format: TextSubtitleFormat,
    hint: LegacyEncodingHint,
    limits: &SubtitleParseLimits,
) -> Result<LoadedTextSubtitle, SubtitleLoadError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(SubtitleLoadError::InputTooLarge {
            actual_bytes: bytes.len(),
            limit_bytes: limits.max_input_bytes,
        });
    }
    let decoded = decode_subtitle_bytes(bytes, hint);
    let document = match format {
        TextSubtitleFormat::Srt => parse_srt(&decoded.text, limits)?,
        TextSubtitleFormat::WebVtt => parse_webvtt(&decoded.text, limits)?,
        TextSubtitleFormat::Srv3 => parse_srv3(&decoded.text, limits)?,
        TextSubtitleFormat::Ass => {
            let ass_limits = limits
                .ass_limits()
                .ok_or(SubtitleLoadError::AssLimitsMissing)?;
            parse_ass_document(&decoded.text, limits, ass_limits)?
        }
    };
    Ok(LoadedTextSubtitle {
        document,
        encoding: decoded.encoding,
        had_replacement_characters: decoded.had_replacement_characters,
    })
}
