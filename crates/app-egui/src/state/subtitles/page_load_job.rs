//! Тело фоновой задачи «разобрать документ субтитров страницы» (мост, сессия 13b1).
//!
//! Чистая последовательность шагов без потоков и `AppState` (как `web_load_job`): поток,
//! доставку результата и отмену обеспечивает `job_runner`, поэтому тесты зовут её синхронно.
//!
//! Шаги: лимит размера → кодировка по языку → формат (содержимое главнее подсказки) →
//! разбор → таймлайн (пустой — отказ) → библиотека шрифтов. Между шагами проверяется отмена.
//!
//! Правило формата (решение 270): подсказка экстеншена — не обещание. Если начало документа
//! уверенно опознаётся (`sniff_text_subtitle_format`), побеждает содержимое; иначе берётся
//! подсказка; подсказки нет (`unknown`) и содержимое не опознано — честный отказ
//! [`SubtitleLoadFailure::UnrecognizedFormat`]. Лимиты — те же, что у файлов (`[subtitles]`).
//!
//! В логи не попадают ни байты, ни текст реплик, ни подпись страницы.

use subtitle_core::{SubtitleLanguage, SubtitleTimeline};
use subtitle_formats::{LegacyEncodingHint, TextSubtitleFormat};

use super::file_loader::{encoding_hint_for, parse_text_timeline, sniff_text_format_of_bytes};
use super::sidecar_job::{
    FontStep, SidecarJobOutcome, SidecarJobRequest, SubtitleLoadFailure,
    build_fonts_unless_cancelled,
};
use crate::page_subtitles::{PageSubtitleDocument, PageSubtitleFormatHint};

/// Разобранный документ страницы.
pub(super) struct ParsedPageTrack {
    /// Реплики дорожки.
    pub(super) timeline: SubtitleTimeline,
    /// Каким форматом документ на самом деле разобран (для журнала).
    pub(super) format: TextSubtitleFormat,
}

/// Выполняет задачу целиком. `is_cancelled` опрашивается между шагами.
pub(super) fn run_page_track_parse(
    request: &SidecarJobRequest,
    document: &PageSubtitleDocument,
    is_cancelled: &dyn Fn() -> bool,
) -> SidecarJobOutcome {
    if is_cancelled() {
        return SidecarJobOutcome::Cancelled;
    }
    let parsed = match parse_page_document(document, request) {
        Ok(parsed) => parsed,
        Err(failure) => return SidecarJobOutcome::Failed(failure),
    };
    match build_fonts_unless_cancelled(request, is_cancelled) {
        FontStep::Built(fonts) => SidecarJobOutcome::PageTrackParsed {
            track: Box::new(parsed),
            fonts,
        },
        FontStep::Cancelled => SidecarJobOutcome::Cancelled,
        FontStep::Failed(failure) => SidecarJobOutcome::Failed(failure),
    }
}

/// Разбор без шрифтов: лимит, кодировка, формат, таймлайн.
fn parse_page_document(
    document: &PageSubtitleDocument,
    request: &SidecarJobRequest,
) -> Result<ParsedPageTrack, SubtitleLoadFailure> {
    // Лимит файла субтитров из `[subtitles]`: протокол пускает до 8 МиБ, настройка может
    // быть строже — действует она.
    let limit_bytes = request.limits.file_max_bytes;
    if document.bytes.len() > limit_bytes {
        return Err(SubtitleLoadFailure::FileTooLarge { limit_bytes });
    }
    let language = document
        .language
        .tag()
        .and_then(|tag| SubtitleLanguage::parse(tag).ok());
    // Старая кодировка без BOM (cp1251 у `ru`) угадывается по языку, как у файлов рядом.
    let encoding_hint = encoding_hint_for(language.as_ref());
    let format = resolve_page_format(&document.bytes, document.format_hint, encoding_hint)?;
    let timeline = parse_text_timeline(
        &document.bytes,
        format,
        encoding_hint,
        &request.limits.parse,
    )?;
    if timeline.is_empty() {
        return Err(SubtitleLoadFailure::NoCuesFound);
    }
    Ok(ParsedPageTrack { timeline, format })
}

/// Формат разбора: опознанное содержимое, иначе подсказка, иначе отказ.
fn resolve_page_format(
    bytes: &[u8],
    hint: PageSubtitleFormatHint,
    encoding_hint: LegacyEncodingHint,
) -> Result<TextSubtitleFormat, SubtitleLoadFailure> {
    let from_content = sniff_text_format_of_bytes(bytes, encoding_hint);
    let from_hint = text_format_of_hint(hint);
    if let (Some(content), Some(hinted)) = (from_content, from_hint)
        && content != hinted
    {
        tracing::debug!(
            hint = hint.log_label(),
            from_content = ?content,
            "Page subtitle content overrides the format hint"
        );
    }
    from_content
        .or(from_hint)
        .ok_or(SubtitleLoadFailure::UnrecognizedFormat)
}

/// Формат разборщика по подсказке; `None` — подсказки нет.
const fn text_format_of_hint(hint: PageSubtitleFormatHint) -> Option<TextSubtitleFormat> {
    match hint {
        PageSubtitleFormatHint::WebVtt => Some(TextSubtitleFormat::WebVtt),
        PageSubtitleFormatHint::Srt => Some(TextSubtitleFormat::Srt),
        PageSubtitleFormatHint::Ttml => Some(TextSubtitleFormat::Ttml),
        PageSubtitleFormatHint::Srv3 => Some(TextSubtitleFormat::Srv3),
        PageSubtitleFormatHint::Json3 => Some(TextSubtitleFormat::Json3),
        PageSubtitleFormatHint::Unknown => None,
    }
}

/// Стабильная метка формата для журнала.
pub(super) const fn format_log_label(format: TextSubtitleFormat) -> &'static str {
    match format {
        TextSubtitleFormat::Srt => "srt",
        TextSubtitleFormat::WebVtt => "web_vtt",
        TextSubtitleFormat::Ass => "ass",
        TextSubtitleFormat::Srv3 => "srv3",
        TextSubtitleFormat::Ttml => "ttml",
        TextSubtitleFormat::Json3 => "json3",
        // Формат добавили в `subtitle-formats`, а метку для журнала ещё нет.
        _ => "other",
    }
}

#[cfg(test)]
#[path = "page_load_job_tests.rs"]
mod tests;
