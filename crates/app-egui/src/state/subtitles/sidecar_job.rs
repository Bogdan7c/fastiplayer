//! Тело фоновой задачи субтитров. Три вида работы (см. [`SidecarJobTask`]):
//!
//! 1. `DiscoverNextToVideo` — найти файлы рядом с видео, выполнить автовыбор и прочитать
//!    ТОЛЬКО одну дорожку (выбранную или принудительную). Остальные остаются в списке
//!    непрочитанными.
//! 2. `LoadFile` — прочитать одну явно указанную дорожку (выбор в меню, брошенный файл).
//!
//! 3. `BuildFontsOnly` — только построить библиотеку шрифтов (образец в режиме правки).
//!
//! Это чистая функция без потоков и без знания об `AppState`: поток и доставку результата
//! обеспечивает `job_runner`, а здесь только последовательность шагов с проверками отмены
//! между ними. Благодаря этому тесты запускают её синхронно.
//!
//! Библиотека шрифтов строится только когда задача действительно что-то загрузила (media без
//! субтитров не платит ~90 мс), и только если растеризатора ещё нет (`FontLibraryPlan`).
//!
//! В логи не попадают ни текст реплик, ни пути каталогов (имя файла — только на debug).

use std::io;
use std::path::PathBuf;

use subtitle_formats::SubtitleLoadError;
use subtitle_raster::{FontLibraryError, SubtitleFontLibrary};
use subtitle_sidecar::{
    SidecarSearchError, SidecarSubtitleCandidate, SidecarSubtitleFormat, describe_subtitle_file,
    find_sidecar_subtitles,
};

use super::auto_select::{
    AutoSelection, PreferredLanguages, TrackOrigin, TrackTraits, auto_select,
};
use super::file_loader::{FileLoadError, LoadedSidecarTrack, load_candidate_timeline};
use super::limits::LoadLimits;

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

/// Какую дорожку читать сразу после поиска.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreloadTarget {
    /// Субтитры включены: читаем выбранную автовыбором дорожку.
    Selected,
    /// Субтитры выключены: читаем только принудительную дорожку нужного языка (если есть).
    ForcedFallback,
    /// Ничего не читаем: выбор уже сделан пользователем (его дорожку грузит другая задача).
    Nothing,
}

/// Что именно делает задача.
#[derive(Debug, Clone)]
pub(super) enum SidecarJobTask {
    /// Поиск рядом с видео, автовыбор и чтение одной дорожки.
    DiscoverNextToVideo {
        /// Путь установленного локального видео; `None` — рядом искать нечего (источник не
        /// локальный), задача только проверяет `remembered_file`.
        video_path: Option<PathBuf>,
        /// Предпочитаемые языки (уже с учётом языка системы).
        preferred_languages: PreferredLanguages,
        /// Какую дорожку читать сразу.
        preload: PreloadTarget,
        /// Файл субтитров из памяти выбора «для этого видео»: существует ли он сейчас.
        /// Проверка идёт здесь, в фоне, а не в потоке интерфейса.
        remembered_file: Option<PathBuf>,
    },
    /// Чтение одного файла.
    LoadFile {
        /// Файл (из поиска или выбранный пользователем).
        candidate: SidecarSubtitleCandidate,
    },
    /// Только построить библиотеку шрифтов (для образца в режиме правки, когда ни одна дорожка
    /// ещё не читалась и растеризатора нет).
    BuildFontsOnly,
}

/// Что нужно сделать задаче.
#[derive(Debug, Clone)]
pub(super) struct SidecarJobRequest {
    /// Вид работы.
    pub(super) task: SidecarJobTask,
    /// Нужна ли библиотека шрифтов.
    pub(super) fonts: FontLibraryPlan,
    /// Лимиты на момент запуска (смена настроек задачу не затрагивает).
    pub(super) limits: LoadLimits,
}

/// Почему дорожка недоступна. Воспроизведение видео при этом не затрагивается.
///
/// `Display` английский и без путей/текста: его можно писать в лог и показывать человеку.
#[derive(Debug, thiserror::Error)]
pub(super) enum SubtitleLoadFailure {
    /// Поиск рядом с видео невозможен (у пути нет каталога или имени).
    #[error("subtitle search failed: {0}")]
    SearchFailed(SidecarSearchError),
    /// Формат найден, но показ ещё не поддержан.
    #[error("subtitle format {format:?} is not supported yet")]
    FormatNotSupportedYet {
        /// Формат файла.
        format: SidecarSubtitleFormat,
    },
    /// Встроенная дорожка с кодеком, который пока не показываем (PGS, VobSub, DVB и прочие).
    #[error("embedded subtitle codec is not supported yet")]
    EmbeddedCodecNotSupportedYet,
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
    /// Не удалось запустить поток-рисовальщик субтитров.
    #[error("subtitle rendering is unavailable: its thread could not be started ({kind:?})")]
    RenderWorkerStartFailed {
        /// Вид ошибки ввода-вывода при создании потока.
        kind: io::ErrorKind,
    },
    /// Поток-рисовальщик остановился сам (паника).
    #[error("subtitle rendering stopped unexpectedly")]
    RenderWorkerLost,
}

/// Дорожка, прочитанная сразу после поиска (или причина, почему не вышло).
pub(super) struct PreloadedTrack {
    /// Номер дорожки во вернувшемся списке кандидатов.
    pub(super) candidate_index: usize,
    /// Результат чтения.
    pub(super) result: Result<LoadedSidecarTrack, SubtitleLoadFailure>,
}

/// Итог поиска.
pub(super) struct DiscoveryOutcome {
    /// Все найденные кандидаты в порядке предпочтения поиска.
    pub(super) candidates: Vec<SidecarSubtitleCandidate>,
    /// Результат автовыбора (индексы в `candidates`). Владелец повторяет выбор над полным
    /// списком (файлы + встроенные), поле читают тесты.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "владелец считает итоговый выбор заново по всему списку"
        )
    )]
    pub(super) selection: AutoSelection,
    /// Прочитанная дорожка (если автовыбор что-то выбрал для `PreloadTarget`).
    pub(super) preloaded: Option<PreloadedTrack>,
    /// Файл из памяти выбора, который существует, имеет поддерживаемое расширение и не найден
    /// поиском рядом с видео (брошенный/выбранный пользователем файл из другой папки).
    /// `None` — файла нет (удалён) либо он уже среди `candidates`.
    pub(super) remembered_file: Option<SidecarSubtitleCandidate>,
    /// Новая библиотека шрифтов; `None` при `ReuseExisting` или если ничего не читалось.
    pub(super) fonts: Option<SubtitleFontLibrary>,
}

/// Итог задачи.
pub(super) enum SidecarJobOutcome {
    /// Поиск выполнен (кандидатов может не быть: это не ошибка).
    Discovered(Box<DiscoveryOutcome>),
    /// Явный файл загружен.
    FileLoaded {
        /// Дорожка.
        track: Box<LoadedSidecarTrack>,
        /// Новая библиотека шрифтов, если она требовалась.
        fonts: Option<SubtitleFontLibrary>,
    },
    /// Библиотека шрифтов построена (`None` - растеризатор уже есть, строить не пришлось).
    FontsBuilt(Option<SubtitleFontLibrary>),
    /// Задача не дала результата по типизированной причине (для `LoadFile` — дорожка
    /// недоступна; для поиска — поиск невозможен).
    Failed(SubtitleLoadFailure),
    /// Задачу отменили (сменился media или выбор); результат нужно выбросить.
    Cancelled,
}

/// Выполняет задачу целиком. `is_cancelled` опрашивается между шагами и внутри поиска.
pub(super) fn run_sidecar_job(
    request: &SidecarJobRequest,
    is_cancelled: &dyn Fn() -> bool,
) -> SidecarJobOutcome {
    match &request.task {
        SidecarJobTask::DiscoverNextToVideo {
            video_path,
            preferred_languages,
            preload,
            remembered_file,
        } => run_discovery(
            request,
            DiscoveryTarget {
                video_path: video_path.as_deref(),
                remembered_file: remembered_file.as_deref(),
            },
            preferred_languages,
            *preload,
            is_cancelled,
        ),
        SidecarJobTask::LoadFile { candidate } => run_file_load(request, candidate, is_cancelled),
        SidecarJobTask::BuildFontsOnly => match build_fonts_unless_cancelled(request, is_cancelled)
        {
            FontStep::Built(fonts) => SidecarJobOutcome::FontsBuilt(fonts),
            FontStep::Cancelled => SidecarJobOutcome::Cancelled,
            FontStep::Failed(failure) => SidecarJobOutcome::Failed(failure),
        },
    }
}

/// Что именно искать задаче поиска.
struct DiscoveryTarget<'a> {
    /// Видео, рядом с которым ищем файлы (`None` — не ищем).
    video_path: Option<&'a std::path::Path>,
    /// Файл из памяти выбора, существование которого надо проверить.
    remembered_file: Option<&'a std::path::Path>,
}

fn run_discovery(
    request: &SidecarJobRequest,
    target: DiscoveryTarget<'_>,
    preferred_languages: &PreferredLanguages,
    preload: PreloadTarget,
    is_cancelled: &dyn Fn() -> bool,
) -> SidecarJobOutcome {
    let candidates = match target.video_path {
        Some(video_path) => {
            let search =
                match find_sidecar_subtitles(video_path, &request.limits.search, is_cancelled) {
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
            search.candidates
        }
        None => Vec::new(),
    };
    let remembered_file = target
        .remembered_file
        .and_then(|path| describe_existing_remembered_file(path, &candidates));

    let traits: Vec<TrackTraits<'_>> = candidates
        .iter()
        .map(|candidate| TrackTraits {
            origin: TrackOrigin::Sidecar,
            is_default: false,
            language: candidate.language.as_ref(),
            is_forced: candidate.disposition.is_forced,
            is_hearing_impaired: candidate.is_hearing_impaired,
            is_renderable: candidate.format.is_renderable_now(),
            has_per_object_forced_flags: false,
        })
        .collect();
    let selection = auto_select(&traits, preferred_languages);
    drop(traits);

    let preload_index = match preload {
        PreloadTarget::Selected => selection.for_enabled,
        PreloadTarget::ForcedFallback => selection.forced_fallback,
        PreloadTarget::Nothing => None,
    };
    let Some(candidate_index) = preload_index else {
        return SidecarJobOutcome::Discovered(Box::new(DiscoveryOutcome {
            candidates,
            selection,
            preloaded: None,
            remembered_file,
            fonts: None,
        }));
    };
    // Индекс пришёл из `auto_select` по тому же списку; `get` вместо индексации на случай
    // нарушения этого инварианта.
    let Some(candidate) = candidates.get(candidate_index) else {
        return SidecarJobOutcome::Discovered(Box::new(DiscoveryOutcome {
            candidates,
            selection,
            preloaded: None,
            remembered_file,
            fonts: None,
        }));
    };

    let result = match load_candidate_timeline(candidate, &request.limits, is_cancelled) {
        Ok(track) => Ok(track),
        Err(FileLoadError::Cancelled) => return SidecarJobOutcome::Cancelled,
        Err(FileLoadError::Failed(failure)) => Err(failure),
    };
    // Шрифты нужны, только если есть что показывать.
    let (result, fonts) = match result {
        Ok(track) => match build_fonts_unless_cancelled(request, is_cancelled) {
            FontStep::Built(fonts) => (Ok(track), fonts),
            FontStep::Cancelled => return SidecarJobOutcome::Cancelled,
            FontStep::Failed(failure) => (Err(failure), None),
        },
        Err(failure) => (Err(failure), None),
    };
    SidecarJobOutcome::Discovered(Box::new(DiscoveryOutcome {
        candidates,
        selection,
        preloaded: Some(PreloadedTrack {
            candidate_index,
            result,
        }),
        remembered_file,
        fonts,
    }))
}

/// Описание запомненного файла, если он ещё существует и не найден поиском рядом с видео.
///
/// Отсутствие файла (удалён, диск не подключён) — штатный исход, а не ошибка: память выбора
/// молча уступает автовыбору. Причину не логируем: путь в лог не пишем, а факт не интересен.
fn describe_existing_remembered_file(
    path: &std::path::Path,
    discovered: &[SidecarSubtitleCandidate],
) -> Option<SidecarSubtitleCandidate> {
    if discovered.iter().any(|candidate| candidate.path == path) {
        return None;
    }
    if !path.is_file() {
        return None;
    }
    describe_subtitle_file(path)
}

fn run_file_load(
    request: &SidecarJobRequest,
    candidate: &SidecarSubtitleCandidate,
    is_cancelled: &dyn Fn() -> bool,
) -> SidecarJobOutcome {
    let track = match load_candidate_timeline(candidate, &request.limits, is_cancelled) {
        Ok(track) => track,
        Err(FileLoadError::Cancelled) => return SidecarJobOutcome::Cancelled,
        Err(FileLoadError::Failed(failure)) => return SidecarJobOutcome::Failed(failure),
    };
    match build_fonts_unless_cancelled(request, is_cancelled) {
        FontStep::Built(fonts) => SidecarJobOutcome::FileLoaded {
            track: Box::new(track),
            fonts,
        },
        FontStep::Cancelled => SidecarJobOutcome::Cancelled,
        FontStep::Failed(failure) => SidecarJobOutcome::Failed(failure),
    }
}

/// Итог шага «библиотека шрифтов».
#[expect(
    clippy::large_enum_variant,
    reason = "значение живёт мгновение и один раз переезжает через mailbox; Box только усложнил бы доступ"
)]
enum FontStep {
    Built(Option<SubtitleFontLibrary>),
    Cancelled,
    Failed(SubtitleLoadFailure),
}

fn build_fonts_unless_cancelled(
    request: &SidecarJobRequest,
    is_cancelled: &dyn Fn() -> bool,
) -> FontStep {
    if is_cancelled() {
        return FontStep::Cancelled;
    }
    match build_font_library(request.fonts, &request.limits) {
        Ok(fonts) => FontStep::Built(fonts),
        Err(error) => FontStep::Failed(SubtitleLoadFailure::FontLibraryFailed(error)),
    }
}

/// Строит библиотеку шрифтов по плану.
fn build_font_library(
    plan: FontLibraryPlan,
    limits: &LoadLimits,
) -> Result<Option<SubtitleFontLibrary>, FontLibraryError> {
    match plan {
        FontLibraryPlan::ReuseExisting => Ok(None),
        FontLibraryPlan::BuildEmbeddedOnly => {
            SubtitleFontLibrary::with_embedded_fonts(limits.font_lookup_cache).map(Some)
        }
        FontLibraryPlan::BuildWithSystemFonts => {
            let mut library = SubtitleFontLibrary::with_embedded_fonts(limits.font_lookup_cache)?;
            let report = library.load_system_fonts();
            tracing::debug!(status = ?report.status, "System fonts prepared for subtitles");
            Ok(Some(library))
        }
    }
}
