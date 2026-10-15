//! Тело фоновой задачи загрузки субтитров: поиск файла рядом -> выбор -> чтение -> разбор.
//!
//! Это чистая функция без потоков и без знания об `AppState`: поток и доставку результата
//! обеспечивает `job_runner`, а здесь только последовательность шагов с проверками отмены
//! между ними. Благодаря этому тесты запускают её синхронно.
//!
//! Правила (решения владельца, сессия 03):
//! - из найденных кандидатов берётся ПЕРВЫЙ, который можно показать сейчас
//!   (`is_renderable_now`); ASS/SSA пропускаются (до сессий 08–10), это пишется в debug-лог;
//! - если выбранный файл не читается или не разбирается, дорожка помечается недоступной
//!   (типизированная причина), следующий кандидат НЕ пробуется: выбор дорожек — сессия 04;
//! - в логи не попадают ни текст реплик, ни пути каталогов (имя файла — только на debug).

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use subtitle_core::{SubtitleLanguage, SubtitleTimeline};
use subtitle_formats::{
    LegacyEncodingHint, SubtitleLoadError, TextSubtitleFormat, load_text_subtitle,
};
use subtitle_raster::{FontLibraryError, SubtitleFontLibrary};
use subtitle_sidecar::{SidecarSearchError, SidecarSubtitleFormat, find_sidecar_subtitles};

use super::limits::{
    FONT_LOOKUP_CACHE_LIMITS, SIDECAR_SEARCH_LIMITS, SUBTITLE_FILE_MAX_BYTES, SUBTITLE_PARSE_LIMITS,
};

/// Нужна ли задаче новая библиотека шрифтов.
///
/// Библиотека с системными шрифтами строится ~90 мс (на холодном диске дольше), поэтому
/// только в фоне и только один раз за жизнь растеризатора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FontLibraryPlan {
    /// Растеризатор уже существует и владеет библиотекой: задача шрифты не трогает.
    ReuseExisting,
    /// Встроенные шрифты + сканирование системных (продакшен).
    BuildWithSystemFonts,
    /// Только встроенные шрифты: быстро и независимо от шрифтов машины (тесты).
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "нужен тестам: не зависит от шрифтов машины")
    )]
    BuildEmbeddedOnly,
}

/// Что нужно сделать задаче.
#[derive(Debug, Clone)]
pub(super) struct SidecarJobRequest {
    /// Путь установленного локального видео; рядом с ним ищутся субтитры.
    pub(super) video_path: PathBuf,
    /// Нужна ли библиотека шрифтов.
    pub(super) fonts: FontLibraryPlan,
}

/// Почему дорожка недоступна. Воспроизведение видео при этом не затрагивается.
///
/// `Display` английский и без путей/текста: его можно писать в лог и (в сессии 04)
/// показывать человеку.
#[derive(Debug, thiserror::Error)]
pub(super) enum SubtitleLoadFailure {
    /// Поиск рядом с видео невозможен (у пути нет каталога или имени).
    #[error("subtitle search failed: {0}")]
    SearchFailed(SidecarSearchError),
    /// Файл больше лимита и не читался.
    #[error("subtitle file is larger than the {limit_bytes} byte limit")]
    FileTooLarge {
        /// Действующий лимит в байтах.
        limit_bytes: usize,
    },
    /// Файл не удалось открыть или прочитать.
    #[error("subtitle file could not be read ({kind:?})")]
    ReadFailed {
        /// Вид ошибки ввода-вывода (без пути).
        kind: io::ErrorKind,
    },
    /// Файл прочитан, но не разобран (слишком много реплик, не WebVTT и т.п.).
    #[error("subtitle file could not be parsed: {0}")]
    ParseFailed(SubtitleLoadError),
    /// Не удалось подготовить библиотеку шрифтов.
    #[error("subtitle fonts could not be prepared: {0}")]
    FontLibraryFailed(FontLibraryError),
    /// Не удалось запустить фоновый поток.
    #[error("subtitle loader thread could not be started ({kind:?})")]
    WorkerStartFailed {
        /// Вид ошибки ввода-вывода при создании потока.
        kind: io::ErrorKind,
    },
    /// Поток завершился без результата (паника или потерянный ящик).
    #[error("subtitle loader stopped without a result")]
    WorkerLost,
}

/// Загруженная дорожка и (при необходимости) новая библиотека шрифтов.
pub(super) struct LoadedSidecarTrack {
    /// Реплики дорожки.
    pub(super) timeline: SubtitleTimeline,
    /// Библиотека для создания растеризатора; `None` при `FontLibraryPlan::ReuseExisting`.
    pub(super) fonts: Option<SubtitleFontLibrary>,
    /// Язык дорожки по имени файла (`Movie.ja.srt`); `None` — не указан.
    pub(super) language: Option<SubtitleLanguage>,
    /// Формат файла (для логов и будущего меню).
    pub(super) format: SidecarSubtitleFormat,
    /// Имя файла без каталога: только для debug-лога.
    pub(super) file_name: Option<OsString>,
}

/// Итог задачи.
pub(super) enum SidecarJobOutcome {
    /// Рядом нет файла, который можно показать сейчас. Это не ошибка.
    NoRenderableCandidate {
        /// Сколько найденных кандидатов пропущено из-за формата (ASS/SSA).
        skipped_unsupported_format: usize,
    },
    /// Дорожка загружена.
    Loaded(Box<LoadedSidecarTrack>),
    /// Дорожка недоступна по типизированной причине.
    Failed(SubtitleLoadFailure),
    /// Задачу отменили (сменился media); результат нужно выбросить.
    Cancelled,
}

/// Выполняет задачу целиком. `is_cancelled` опрашивается между шагами и внутри поиска.
pub(super) fn run_sidecar_job(
    request: &SidecarJobRequest,
    is_cancelled: &dyn Fn() -> bool,
) -> SidecarJobOutcome {
    let search =
        match find_sidecar_subtitles(&request.video_path, &SIDECAR_SEARCH_LIMITS, is_cancelled) {
            Ok(outcome) => outcome,
            Err(SidecarSearchError::Cancelled) => return SidecarJobOutcome::Cancelled,
            Err(error) => {
                return SidecarJobOutcome::Failed(SubtitleLoadFailure::SearchFailed(error));
            }
        };
    if search.report.is_truncated() || !search.report.unreadable_directories.is_empty() {
        // Пути каталогов в лог не пишем: только факт и счётчики.
        tracing::debug!(
            truncated = search.report.is_truncated(),
            unreadable_directories = search.report.unreadable_directories.len(),
            "Subtitle search was incomplete"
        );
    }

    let Some(candidate) = search
        .candidates
        .iter()
        .find(|candidate| text_format_of(candidate.format).is_some())
    else {
        let skipped_unsupported_format = search.candidates.len();
        if skipped_unsupported_format > 0 {
            tracing::debug!(
                skipped_unsupported_format,
                "Subtitle files found, but none is in a format that can be shown yet"
            );
        }
        return SidecarJobOutcome::NoRenderableCandidate {
            skipped_unsupported_format,
        };
    };
    let Some(text_format) = text_format_of(candidate.format) else {
        // Недостижимо: `find` выше уже отобрал кандидата с текстовым форматом. Явная ветка
        // вместо unwrap, чтобы нарушение инварианта не превращалось в панику.
        return SidecarJobOutcome::NoRenderableCandidate {
            skipped_unsupported_format: search.candidates.len(),
        };
    };

    if is_cancelled() {
        return SidecarJobOutcome::Cancelled;
    }
    let bytes = match read_file_with_limit(&candidate.path, SUBTITLE_FILE_MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(failure) => return SidecarJobOutcome::Failed(failure),
    };

    if is_cancelled() {
        return SidecarJobOutcome::Cancelled;
    }
    let document = match load_text_subtitle(
        &bytes,
        text_format,
        encoding_hint_for(candidate.language.as_ref()),
        &SUBTITLE_PARSE_LIMITS,
    ) {
        Ok(loaded) => loaded.into_document(),
        Err(error) => return SidecarJobOutcome::Failed(SubtitleLoadFailure::ParseFailed(error)),
    };
    // Освобождаем сырые байты до долгой операции со шрифтами.
    drop(bytes);
    let (events, _regions, _report) = document.into_parts();
    let (timeline, _build_report) = SubtitleTimeline::from_events(events);

    if is_cancelled() {
        return SidecarJobOutcome::Cancelled;
    }
    let fonts = match build_font_library(request.fonts) {
        Ok(fonts) => fonts,
        Err(error) => {
            return SidecarJobOutcome::Failed(SubtitleLoadFailure::FontLibraryFailed(error));
        }
    };

    SidecarJobOutcome::Loaded(Box::new(LoadedSidecarTrack {
        timeline,
        fonts,
        language: candidate.language.clone(),
        format: candidate.format,
        file_name: candidate.path.file_name().map(OsString::from),
    }))
}

/// Текстовый формат парсера для формата файла; `None` — показать нельзя (ASS/SSA и будущие).
///
/// Совмещаем два условия: кандидат должен быть «показываемым сейчас» по оценке поиска и
/// иметь парсер здесь, иначе новый формат без парсера молча привёл бы к ошибке на загрузке.
fn text_format_of(format: SidecarSubtitleFormat) -> Option<TextSubtitleFormat> {
    if !format.is_renderable_now() {
        return None;
    }
    match format {
        SidecarSubtitleFormat::Srt => Some(TextSubtitleFormat::Srt),
        SidecarSubtitleFormat::WebVtt => Some(TextSubtitleFormat::WebVtt),
        _ => None,
    }
}

/// Подсказка кодировки для файлов без BOM и не в UTF-8.
///
/// Кириллические языки и «язык не указан» (самый частый случай `Movie.srt`) получают
/// кириллическую подсказку (продуктовое решение: Fastiplayer русскоязычный); явно указанный
/// другой язык не навязывает кириллицу. Переедет в конфиг вместе с языками (сессия 04).
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

/// Строит библиотеку шрифтов по плану.
fn build_font_library(
    plan: FontLibraryPlan,
) -> Result<Option<SubtitleFontLibrary>, FontLibraryError> {
    match plan {
        FontLibraryPlan::ReuseExisting => Ok(None),
        FontLibraryPlan::BuildEmbeddedOnly => {
            SubtitleFontLibrary::with_embedded_fonts(FONT_LOOKUP_CACHE_LIMITS).map(Some)
        }
        FontLibraryPlan::BuildWithSystemFonts => {
            let mut library = SubtitleFontLibrary::with_embedded_fonts(FONT_LOOKUP_CACHE_LIMITS)?;
            let report = library.load_system_fonts();
            tracing::debug!(status = ?report.status, "System fonts prepared for subtitles");
            Ok(Some(library))
        }
    }
}
