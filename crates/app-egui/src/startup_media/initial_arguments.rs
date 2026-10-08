//! Классификация media-аргументов командной строки после получения process lease.
//!
//! Вынесено из `startup_media.rs` (модуль у предела размера) вместе с новой логикой
//! нескольких аргументов (сессия 13), чтобы у классификации был один владелец.
//!
//! Правила:
//! - 0 аргументов — ничего не открываем;
//! - 1 аргумент — прежнее поведение без изменений (`resolve_initial_media_argument`);
//! - ≥2 аргумента — каждый классифицируется тем же классификатором одного аргумента, затем
//!   действуют правила drag & drop (решение владельца 6 сессии 12): есть локальные файлы —
//!   открываются только они в заданном порядке; иначе первый плейлист; иначе первая ссылка.
//!   Остальное пропускается с информационным уведомлением.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use fastiplayer_config::AppConfig;
use playlist_core::MAX_PLAYLIST_ITEMS;
use tracing::info;

use super::{InitialMedia, is_recognized_startup_playlist_path};
use crate::startup_arguments_message::{
    SeveralArgumentsWinnerKind, SkippedStartupArguments, several_arguments_skipped_notice,
};
use crate::url_service_adapter::{
    StartupUrlClassification, StartupUrlLocator, classify_startup_url,
};

/// Итог классификации одного аргумента: вид цели либо безопасная ошибка ссылки.
enum ClassifiedStartupArgument {
    /// Локальный путь (обычный media-файл), байт-в-байт как передан.
    LocalMedia(PathBuf),
    /// Локальный файл с распознанным расширением плейлиста.
    Playlist(PathBuf),
    /// Ссылка, принятая service adapter-ом.
    Url(StartupUrlLocator),
    /// Ссылка, которую плеер открыть не может; текст уже без секретов.
    RejectedUrl(String),
}

/// Классифицирует один аргумент — единственный владелец правил «путь / плейлист / ссылка».
///
/// Только валидный UTF-8 может быть URL. Native non-UTF-8 значение без lossy
/// преобразования остаётся локальным `PathBuf`.
fn classify_startup_argument(
    argument: OsString,
    app_config: &AppConfig,
) -> ClassifiedStartupArgument {
    let utf8_argument = match argument.into_string() {
        Ok(argument) => argument,
        Err(native_argument) => return classify_local_path(PathBuf::from(native_argument)),
    };

    match classify_startup_url(&utf8_argument) {
        StartupUrlClassification::NotUrl => {}
        StartupUrlClassification::Supported(locator) => {
            info!(source = %locator.safe_label(), "CLI аргумент принят URL service adapter-ом");
            if let Err(safe_error) = locator.validate_config(app_config) {
                return ClassifiedStartupArgument::RejectedUrl(safe_error);
            }
            return ClassifiedStartupArgument::Url(locator);
        }
        StartupUrlClassification::Unsupported { reason } => {
            return ClassifiedStartupArgument::RejectedUrl(reason.safe_error());
        }
    }

    // Всё остальное считаем локальным путём, как работало раньше.
    classify_local_path(PathBuf::from(utf8_argument))
}

/// Локальный путь: плейлист по расширению, иначе обычный media-файл.
fn classify_local_path(path: PathBuf) -> ClassifiedStartupArgument {
    if is_recognized_startup_playlist_path(&path) {
        ClassifiedStartupArgument::Playlist(path)
    } else {
        ClassifiedStartupArgument::LocalMedia(path)
    }
}

/// Классифицирует единственный media intent после получения process lease.
///
/// Поведение одного аргумента зафиксировано регрессионными тестами `startup_media/tests.rs`.
pub(crate) fn resolve_initial_media_argument(
    argument: Option<OsString>,
    app_config: &AppConfig,
) -> (Option<InitialMedia>, Option<String>) {
    let Some(argument) = argument else {
        return (None, None);
    };
    match classify_startup_argument(argument, app_config) {
        ClassifiedStartupArgument::LocalMedia(path) => (Some(InitialMedia::File(path)), None),
        ClassifiedStartupArgument::Playlist(path) => (Some(InitialMedia::Playlist(path)), None),
        ClassifiedStartupArgument::Url(locator) => (Some(InitialMedia::Url(locator)), None),
        ClassifiedStartupArgument::RejectedUrl(safe_error) => (None, Some(safe_error)),
    }
}

/// Классифицирует все media-аргументы командной строки (ноль, один или несколько).
///
/// Возвращает цель для startup flow и безопасную ошибку для существующего UI path.
pub(crate) fn resolve_initial_media_arguments(
    arguments: Vec<OsString>,
    app_config: &AppConfig,
) -> (Option<InitialMedia>, Option<String>) {
    if arguments.len() < 2 {
        // 0 или 1 аргумент: ровно прежний путь, без новых правил.
        return resolve_initial_media_argument(arguments.into_iter().next(), app_config);
    }
    let classified = arguments
        .into_iter()
        .map(|argument| classify_startup_argument(argument, app_config))
        .collect();
    select_among_several(classified, MAX_PLAYLIST_ITEMS)
}

/// Выбирает победителя среди ≥2 классифицированных аргументов (правила drag & drop).
///
/// `queue_capacity` — вместимость очереди: лишние локальные файлы не открываются и
/// попадают в уведомление (параметр нужен тестам, production передаёт лимит очереди).
fn select_among_several(
    classified: Vec<ClassifiedStartupArgument>,
    queue_capacity: usize,
) -> (Option<InitialMedia>, Option<String>) {
    let mut local_files = Vec::new();
    let mut playlists = Vec::new();
    // Ссылки храним в исходном порядке вместе с ошибкой: решает именно первая ссылка.
    let mut links = Vec::new();
    for argument in classified {
        match argument {
            ClassifiedStartupArgument::LocalMedia(path) => local_files.push(path),
            ClassifiedStartupArgument::Playlist(path) => playlists.push(path),
            ClassifiedStartupArgument::Url(locator) => links.push(Ok(locator)),
            ClassifiedStartupArgument::RejectedUrl(safe_error) => links.push(Err(safe_error)),
        }
    }

    let mut skipped = SkippedStartupArguments {
        playlists: playlists.len(),
        links: links.len(),
        files_over_queue_limit: 0,
    };
    let (winner_kind, winner) = if !local_files.is_empty() {
        skipped.files_over_queue_limit = local_files.len().saturating_sub(queue_capacity);
        local_files.truncate(queue_capacity);
        let Some(files) = StartupLocalFiles::from_ordered(local_files) else {
            // Недостижимо при ненулевой вместимости очереди; честно ничего не открываем.
            return (None, None);
        };
        (
            SeveralArgumentsWinnerKind::LocalFiles,
            SeveralArgumentsWinner::LocalFiles(files),
        )
    } else if let Some(first_playlist) = playlists.into_iter().next() {
        skipped.playlists = skipped.playlists.saturating_sub(1);
        (
            SeveralArgumentsWinnerKind::FirstPlaylist,
            SeveralArgumentsWinner::Playlist(first_playlist),
        )
    } else {
        let mut links = links.into_iter();
        // ≥2 аргумента без файлов и плейлистов — значит, все они ссылки.
        let Some(first_link) = links.next() else {
            return (None, None);
        };
        skipped.links = skipped.links.saturating_sub(1);
        match first_link {
            Ok(locator) => (
                SeveralArgumentsWinnerKind::FirstLink,
                SeveralArgumentsWinner::Url(locator),
            ),
            // Первую ссылку открыть нельзя: прежний безопасный путь ошибки, как у одной ссылки.
            Err(safe_error) => return (None, Some(safe_error)),
        }
    };
    let skipped_notice = several_arguments_skipped_notice(winner_kind, skipped, queue_capacity);
    (
        Some(InitialMedia::Several(SeveralInitialArguments {
            winner,
            skipped_notice,
        })),
        None,
    )
}

/// ≥2 аргумента командной строки: выбранный победитель и текст о пропущенном.
pub(crate) struct SeveralInitialArguments {
    /// Что открыть.
    winner: SeveralArgumentsWinner,
    /// Информационное уведомление о пропущенных аргументах; `None` — ничего не пропущено.
    skipped_notice: Option<String>,
}

/// Победитель среди нескольких аргументов.
enum SeveralArgumentsWinner {
    /// Один или больше локальных файлов в порядке командной строки.
    LocalFiles(StartupLocalFiles),
    /// Первый плейлист (локальных файлов не было).
    Playlist(PathBuf),
    /// Первая ссылка (не было ни файлов, ни плейлистов).
    Url(StartupUrlLocator),
}

/// Непустой набор локальных файлов: первый открывается, остальные встают за ним.
struct StartupLocalFiles {
    first: PathBuf,
    rest: Vec<PathBuf>,
}

impl StartupLocalFiles {
    /// Непустота закреплена типом: `None` только для пустого списка.
    fn from_ordered(paths: Vec<PathBuf>) -> Option<Self> {
        let mut paths = paths.into_iter();
        let first = paths.next()?;
        Some(Self {
            first,
            rest: paths.collect(),
        })
    }
}

/// Набор аргументов, развёрнутый в одну цель существующего startup flow.
pub(super) struct SingleStartupTarget {
    /// `File`, `Playlist` или `Url` — те же цели, что у одного аргумента.
    pub(super) target: InitialMedia,
    /// Локальные файлы, которые встанут после первого после его `Installed`.
    pub(super) follow_up_files: Vec<PathBuf>,
    /// Уведомление о пропущенных аргументах.
    pub(super) skipped_notice: Option<String>,
}

impl SeveralInitialArguments {
    /// Разворачивает набор: первая цель идёт прежним путём одного аргумента.
    pub(super) fn into_single_target(self) -> SingleStartupTarget {
        let (target, follow_up_files) = match self.winner {
            SeveralArgumentsWinner::LocalFiles(files) => {
                (InitialMedia::File(files.first), files.rest)
            }
            SeveralArgumentsWinner::Playlist(path) => (InitialMedia::Playlist(path), Vec::new()),
            SeveralArgumentsWinner::Url(locator) => (InitialMedia::Url(locator), Vec::new()),
        };
        SingleStartupTarget {
            target,
            follow_up_files,
            skipped_notice: self.skipped_notice,
        }
    }
}

impl fmt::Debug for SeveralInitialArguments {
    /// Пути и ссылки в `Debug` не попадают (они могут оказаться в логе).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (winner, file_count) = match &self.winner {
            SeveralArgumentsWinner::LocalFiles(files) => ("local-files", 1 + files.rest.len()),
            SeveralArgumentsWinner::Playlist(_) => ("playlist", 0),
            SeveralArgumentsWinner::Url(_) => ("url", 0),
        };
        formatter
            .debug_struct("SeveralInitialArguments")
            .field("winner", &winner)
            .field("file_count", &file_count)
            .field("has_skipped_notice", &self.skipped_notice.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests;
