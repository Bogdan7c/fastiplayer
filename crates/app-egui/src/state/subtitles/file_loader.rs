//! Чтение и разбор одного файла субтитров (шаг фоновой задачи, без потоков и без `AppState`).
//!
//! Правила: файл читается не больше лимита (больший — типизированный отказ), кодировка и
//! формат определяются `subtitle-formats`, ошибки не глотаются, а превращаются в
//! [`SubtitleLoadFailure`]. Между шагами проверяется отмена. В логи не попадают ни текст
//! реплик, ни пути каталогов.

use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use subtitle_core::{SubtitleLanguage, SubtitleTimeline};
use subtitle_formats::{
    LegacyEncodingHint, SubtitleParseLimits, TextSubtitleFormat, load_text_subtitle,
};
use subtitle_sidecar::{SidecarSubtitleCandidate, SidecarSubtitleFormat};

use self::format_detection::resolve_text_format;
use super::limits::LoadLimits;
use super::sidecar_job::SubtitleLoadFailure;

mod format_detection;

/// Загруженный таймлайн и сведения о файле.
pub(super) struct LoadedSidecarTrack {
    /// Реплики дорожки.
    pub(super) timeline: SubtitleTimeline,
    /// Язык дорожки по имени файла (`Movie.ja.srt`); `None` — не указан.
    pub(super) language: Option<SubtitleLanguage>,
    /// Формат файла (для логов).
    pub(super) format: SidecarSubtitleFormat,
    /// Имя файла без каталога: только для debug-лога.
    pub(super) file_name: Option<OsString>,
}

/// Почему шаг не дал таймлайн.
pub(super) enum FileLoadError {
    /// Задачу отменили (сменилось media или выбор).
    Cancelled,
    /// Файл недоступен по типизированной причине.
    Failed(SubtitleLoadFailure),
}

/// Читает и разбирает файл кандидата. `is_cancelled` опрашивается между шагами.
pub(super) fn load_candidate_timeline(
    candidate: &SidecarSubtitleCandidate,
    limits: &LoadLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<LoadedSidecarTrack, FileLoadError> {
    let Some(format_from_extension) = text_format_of(candidate.format) else {
        return Err(FileLoadError::Failed(
            SubtitleLoadFailure::FormatNotSupportedYet {
                format: candidate.format,
            },
        ));
    };

    if is_cancelled() {
        return Err(FileLoadError::Cancelled);
    }
    let bytes = read_file_with_limit(&candidate.path, limits.file_max_bytes)
        .map_err(FileLoadError::Failed)?;

    if is_cancelled() {
        return Err(FileLoadError::Cancelled);
    }
    let encoding_hint = encoding_hint_for(candidate.language.as_ref());
    // Формат решает содержимое; расширение — запасной вариант (переименованные файлы).
    let text_format = resolve_text_format(format_from_extension, &bytes, encoding_hint);
    let timeline = parse_text_timeline(&bytes, text_format, encoding_hint, &limits.parse)
        .map_err(FileLoadError::Failed)?;
    // Освобождаем сырые байты до остальной работы.
    drop(bytes);

    Ok(LoadedSidecarTrack {
        timeline,
        language: candidate.language.clone(),
        format: candidate.format,
        file_name: candidate.path.file_name().map(OsString::from),
    })
}

/// Разбирает байты текстового формата в таймлайн. Общий шаг файла и скачанной web-дорожки:
/// ошибки разбора превращаются в типизированную причину, а не глотаются.
pub(super) fn parse_text_timeline(
    bytes: &[u8],
    text_format: TextSubtitleFormat,
    encoding_hint: LegacyEncodingHint,
    parse_limits: &SubtitleParseLimits,
) -> Result<SubtitleTimeline, SubtitleLoadFailure> {
    let document = load_text_subtitle(bytes, text_format, encoding_hint, parse_limits)
        .map_err(SubtitleLoadFailure::ParseFailed)?
        .into_document();
    let (events, _regions, _report) = document.into_parts();
    let (timeline, _build_report) = SubtitleTimeline::from_events(events);
    Ok(timeline)
}

/// Текстовый формат парсера для формата файла; `None` — показать нельзя (будущие форматы без разбора).
///
/// Совмещаем два условия: формат должен быть «показываемым сейчас» по оценке поиска и иметь
/// парсер здесь, иначе новый формат без парсера молча привёл бы к ошибке на загрузке.
pub(super) fn text_format_of(format: SidecarSubtitleFormat) -> Option<TextSubtitleFormat> {
    if !format.is_renderable_now() {
        return None;
    }
    match format {
        SidecarSubtitleFormat::Srt => Some(TextSubtitleFormat::Srt),
        SidecarSubtitleFormat::WebVtt => Some(TextSubtitleFormat::WebVtt),
        // ASS и SSA разбирает один разборщик: версию он определяет по содержимому.
        SidecarSubtitleFormat::Ass | SidecarSubtitleFormat::Ssa => Some(TextSubtitleFormat::Ass),
        _ => None,
    }
}

/// Подсказка кодировки для файлов без BOM и не в UTF-8.
///
/// Кириллические языки и «язык не указан» (самый частый случай `Movie.srt`) получают
/// кириллическую подсказку (продуктовое решение: Fastiplayer русскоязычный); явно указанный
/// другой язык не навязывает кириллицу.
pub(super) fn encoding_hint_for(language: Option<&SubtitleLanguage>) -> LegacyEncodingHint {
    const CYRILLIC_LANGUAGES: [&str; 7] = ["ru", "uk", "be", "bg", "sr", "mk", "kk"];
    match language {
        None => LegacyEncodingHint::Cyrillic,
        Some(language) if CYRILLIC_LANGUAGES.contains(&language.primary_language()) => {
            LegacyEncodingHint::Cyrillic
        }
        Some(_) => LegacyEncodingHint::Generic,
    }
}

/// Читает файл целиком, но не больше `limit_bytes`: больший файл — типизированный отказ.
///
/// Читаем через `take(limit + 1)`, а не доверяем `metadata().len()`: файл мог вырасти
/// между проверкой и чтением, а у особых файлов длина вообще не отражает содержимое.
pub(super) fn read_file_with_limit(
    path: &Path,
    limit_bytes: usize,
) -> Result<Vec<u8>, SubtitleLoadFailure> {
    let file =
        File::open(path).map_err(|error| SubtitleLoadFailure::ReadFailed { kind: error.kind() })?;
    let read_limit = u64::try_from(limit_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| SubtitleLoadFailure::ReadFailed { kind: error.kind() })?;
    if bytes.len() > limit_bytes {
        return Err(SubtitleLoadFailure::FileTooLarge { limit_bytes });
    }
    Ok(bytes)
}
