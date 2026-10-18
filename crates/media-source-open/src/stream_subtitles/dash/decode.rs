//! Разбор документов сегмента DASH (TTML/IMSC, WebVTT) и сдвиг событий на шкалу плеера.

use std::num::NonZeroUsize;

use subtitle_core::{SubtitleEvent, SubtitleTime, SubtitleTimeShift};
use subtitle_formats::{
    LegacyEncodingHint, LoadedTextSubtitle, SubtitleLoadError, SubtitleParseLimits,
    TextSubtitleFormat, TtmlSampleWindow, load_text_subtitle, load_ttml_sample,
};
use web_media_dash::{DashTextDocument, DashTextDocumentFormat, DashTextFetchedSegment};

use crate::stream_subtitles::error::StreamSubtitleError;

/// Структурный предел числа реплик в одном документе: защита от враждебного файла.
const MAX_CUES_PER_DOCUMENT: usize = 10_000;
/// Сколько проблем разбора запоминает отчёт (сами проблемы ленте не нужны).
const MAX_REPORTED_PARSE_ISSUES: usize = 8;

/// Разбирает документы сегмента и переводит события на шкалу плеера.
///
/// Если документов несколько (`stpp`: по одному на сэмпл), битый документ пропускается,
/// пока разобрался хотя бы один; если не разобрался ни один - `Malformed`. Пустой сегмент
/// (ни одного документа) - не ошибка: реплик в этом отрезке просто нет.
pub(super) fn decode_documents(
    fetched: &DashTextFetchedSegment,
    max_bytes: NonZeroUsize,
) -> Result<Vec<SubtitleEvent>, StreamSubtitleError> {
    let limits = SubtitleParseLimits::new(
        max_bytes.get(),
        MAX_CUES_PER_DOCUMENT,
        MAX_REPORTED_PARSE_ISSUES,
    );
    let shift = SubtitleTimeShift::from_micros(fetched.player_shift_micros);
    let mut events = Vec::new();
    let mut parsed_documents = 0_usize;
    let mut last_error = None;
    for document in &fetched.documents {
        match load_document(document, &limits) {
            Ok(loaded) => {
                let (document_events, _regions, _report) = loaded.into_document().into_parts();
                for event in &document_events {
                    events.push(
                        event
                            .shifted_by(shift)
                            .map_err(|_| StreamSubtitleError::Malformed)?,
                    );
                }
                parsed_documents += 1;
            }
            Err(error) => last_error = Some(map_load_error(&error)),
        }
    }
    match (parsed_documents, last_error) {
        (0, Some(error)) => Err(error),
        _ => Ok(events),
    }
}

/// Разбирает один документ; TTML из сэмпла (`stpp`) получает окно действия сэмпла.
fn load_document(
    document: &DashTextDocument,
    limits: &SubtitleParseLimits,
) -> Result<LoadedTextSubtitle, SubtitleLoadError> {
    match (document.format, document.sample_interval) {
        (DashTextDocumentFormat::Ttml, Some(interval)) => {
            let window = TtmlSampleWindow::new(
                SubtitleTime::from_micros(interval.start_micros),
                SubtitleTime::from_micros(interval.end_micros),
            );
            match window {
                Some(window) => {
                    load_ttml_sample(&document.bytes, LegacyEncodingHint::Generic, limits, window)
                }
                // Интервал вырожденный: документ нигде не действует, но не битый.
                None => load_text_subtitle(
                    &document.bytes,
                    TextSubtitleFormat::Ttml,
                    LegacyEncodingHint::Generic,
                    limits,
                ),
            }
        }
        (DashTextDocumentFormat::Ttml, None) => load_text_subtitle(
            &document.bytes,
            TextSubtitleFormat::Ttml,
            LegacyEncodingHint::Generic,
            limits,
        ),
        (DashTextDocumentFormat::WebVtt, _) => load_text_subtitle(
            &document.bytes,
            TextSubtitleFormat::WebVtt,
            LegacyEncodingHint::Generic,
            limits,
        ),
    }
}

fn map_load_error(error: &SubtitleLoadError) -> StreamSubtitleError {
    match error {
        SubtitleLoadError::InputTooLarge { limit_bytes, .. } => StreamSubtitleError::TooLarge {
            limit_bytes: *limit_bytes,
        },
        _ => StreamSubtitleError::Malformed,
    }
}
