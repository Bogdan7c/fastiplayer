//! Оркестрация поиска: обход каталогов, отбор по правилам имён, упорядочивание.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::error::SidecarSearchError;
use crate::language_hint::parse_name_hints;
use crate::limits::SidecarSearchLimits;
use crate::matching::{
    StemMatch, is_movie_named_directory, is_subtitle_directory_name, match_video_stem,
};
use crate::outcome::{
    SidecarLocation, SidecarSearchOutcome, SidecarSearchReport, SidecarSubtitleCandidate,
    SidecarSubtitleFormat,
};
use crate::scan::{EntryKind, ScannedEntry, scan_directory};

/// Глубина каталога `Subs`-подобной папки относительно папки видео.
const SUBTITLE_DIRECTORY_DEPTH: u8 = 1;
/// Глубина каталога `Subs/<Movie>/`.
const MOVIE_NAMED_DIRECTORY_DEPTH: u8 = 2;

/// Группа порядка: чем меньше, тем раньше в результате.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum OrderGroup {
    ExactNameNextToVideo,
    OtherNextToVideo,
    InSubtitleFolder,
    InMovieFolder,
}

/// Кандидат вместе с ключами упорядочивания (вне публичной структуры).
struct RankedCandidate {
    group: OrderGroup,
    file_name: OsString,
    candidate: SidecarSubtitleCandidate,
}

/// Ищет файлы субтитров рядом с видео по правилам решения владельца 13.
///
/// `is_cancelled` вызывается между записями каталогов; `true` даёт `Err(Cancelled)`.
/// Нечитаемые каталоги и превышение лимитов — не ошибки, они описаны в `report`.
pub fn find_sidecar_subtitles(
    video_path: &Path,
    limits: &SidecarSearchLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<SidecarSearchOutcome, SidecarSearchError> {
    let video_directory = video_directory_of(video_path)?;
    let video_stem = video_path
        .file_stem()
        .ok_or(SidecarSearchError::VideoPathHasNoFileStem)?;
    let video_file_name = video_path
        .file_name()
        .ok_or(SidecarSearchError::VideoPathHasNoFileStem)?;

    let mut report = SidecarSearchReport::default();
    let mut ranked = Vec::new();

    // Уровень 0: папка видео.
    let video_dir_entries = scan_directory(&video_directory, limits, is_cancelled, &mut report)?;
    let mut subtitle_directories = Vec::new();
    for entry in &video_dir_entries {
        match entry.kind {
            EntryKind::File if entry.name != video_file_name => {
                if let Some(found) =
                    rank_prefixed_file(entry, video_stem, VideoSide::VideoDirectory)
                {
                    ranked.push(found);
                }
            }
            EntryKind::Directory if is_subtitle_directory_name(&entry.name) => {
                subtitle_directories.push(entry.path.clone());
            }
            _ => {}
        }
    }

    // Уровень 1: Subs-подобные папки.
    let mut movie_named_directories = Vec::new();
    if limits.max_subdirectory_depth >= SUBTITLE_DIRECTORY_DEPTH {
        for directory in &subtitle_directories {
            let entries = scan_directory(directory, limits, is_cancelled, &mut report)?;
            for entry in &entries {
                match entry.kind {
                    EntryKind::File => {
                        if let Some(found) =
                            rank_prefixed_file(entry, video_stem, VideoSide::SubtitleDirectory)
                        {
                            ranked.push(found);
                        }
                    }
                    EntryKind::Directory if is_movie_named_directory(&entry.name, video_stem) => {
                        movie_named_directories.push(entry.path.clone());
                    }
                    EntryKind::Directory => {}
                }
            }
        }
    } else {
        report.subdirectories_skipped_by_depth_limit += subtitle_directories.len();
    }

    // Уровень 2: Subs/<Movie>/ — подходит любой файл субтитров.
    if limits.max_subdirectory_depth >= MOVIE_NAMED_DIRECTORY_DEPTH {
        for directory in &movie_named_directories {
            let entries = scan_directory(directory, limits, is_cancelled, &mut report)?;
            ranked.extend(entries.iter().filter_map(rank_any_file_in_movie_directory));
        }
    } else {
        report.subdirectories_skipped_by_depth_limit += movie_named_directories.len();
    }

    Ok(finish(ranked, limits, report))
}

/// Каталог видео; для относительного пути без каталога это текущий (`.`).
fn video_directory_of(video_path: &Path) -> Result<PathBuf, SidecarSearchError> {
    match video_path.parent() {
        None => Err(SidecarSearchError::VideoPathHasNoParent),
        Some(parent) if parent.as_os_str().is_empty() => Ok(PathBuf::from(".")),
        Some(parent) => Ok(parent.to_path_buf()),
    }
}

/// Упорядочивает кандидатов, применяет `max_candidates` и фиксирует отброшенных.
fn finish(
    mut ranked: Vec<RankedCandidate>,
    limits: &SidecarSearchLimits,
    mut report: SidecarSearchReport,
) -> SidecarSearchOutcome {
    // Стабильная сортировка: при равных ключах сохраняется порядок обхода каталогов.
    ranked.sort_by(|left, right| {
        left.group
            .cmp(&right.group)
            .then_with(|| left.file_name.cmp(&right.file_name))
    });
    if ranked.len() > limits.max_candidates {
        report.candidates_dropped_by_limit = ranked.len() - limits.max_candidates;
        ranked.truncate(limits.max_candidates);
    }
    SidecarSearchOutcome {
        candidates: ranked.into_iter().map(|item| item.candidate).collect(),
        report,
    }
}

/// Где лежит файл при правилах «имя начинается с основы видео».
#[derive(Debug, Clone, Copy)]
enum VideoSide {
    VideoDirectory,
    SubtitleDirectory,
}

/// Файл в папке видео или `Subs`: нужен префикс основы видео + разделитель.
fn rank_prefixed_file(
    entry: &ScannedEntry,
    video_stem: &OsStr,
    side: VideoSide,
) -> Option<RankedCandidate> {
    let format = subtitle_format_of(&entry.name)?;
    let file_stem = Path::new(&entry.name).file_stem()?;
    let (group, location, suffix) = match (match_video_stem(file_stem, video_stem), side) {
        (StemMatch::NoMatch, _) => return None,
        (StemMatch::Exact, VideoSide::VideoDirectory) => (
            OrderGroup::ExactNameNextToVideo,
            SidecarLocation::VideoDirectory,
            String::new(),
        ),
        (StemMatch::WithSuffix(suffix), VideoSide::VideoDirectory) => (
            OrderGroup::OtherNextToVideo,
            SidecarLocation::VideoDirectory,
            suffix,
        ),
        (StemMatch::Exact, VideoSide::SubtitleDirectory) => (
            OrderGroup::InSubtitleFolder,
            SidecarLocation::SubtitleDirectory,
            String::new(),
        ),
        (StemMatch::WithSuffix(suffix), VideoSide::SubtitleDirectory) => (
            OrderGroup::InSubtitleFolder,
            SidecarLocation::SubtitleDirectory,
            suffix,
        ),
    };
    Some(build_candidate(entry, format, group, location, &suffix))
}

/// Файл в `Subs/<Movie>/`: любое имя; подсказки берутся из всей основы имени.
fn rank_any_file_in_movie_directory(entry: &ScannedEntry) -> Option<RankedCandidate> {
    if entry.kind != EntryKind::File {
        return None;
    }
    let format = subtitle_format_of(&entry.name)?;
    let whole_stem = Path::new(&entry.name).file_stem()?.to_string_lossy();
    Some(build_candidate(
        entry,
        format,
        OrderGroup::InMovieFolder,
        SidecarLocation::MovieNamedDirectory,
        &whole_stem,
    ))
}

fn build_candidate(
    entry: &ScannedEntry,
    format: SidecarSubtitleFormat,
    group: OrderGroup,
    location: SidecarLocation,
    hint_source: &str,
) -> RankedCandidate {
    let hints = parse_name_hints(hint_source);
    RankedCandidate {
        group,
        file_name: entry.name.clone(),
        candidate: SidecarSubtitleCandidate {
            path: entry.path.clone(),
            format,
            location,
            language: hints.language,
            disposition: hints.disposition,
            is_hearing_impaired: hints.is_hearing_impaired,
            title: hints.title,
        },
    }
}

/// Формат по расширению; имя без расширения или с не-UTF-8 расширением — не субтитры.
fn subtitle_format_of(file_name: &OsStr) -> Option<SidecarSubtitleFormat> {
    let extension = Path::new(file_name).extension()?.to_str()?;
    SidecarSubtitleFormat::from_extension(extension)
}
