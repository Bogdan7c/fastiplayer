//! Сквозные тесты поиска файлов субтитров на реальном дереве каталогов.
// Хелперы тестов строят дерево каталогов; падение на ошибке ФС здесь — штатный способ провалить тест.
#![allow(clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use subtitle_core::SubtitleFormat;
use subtitle_sidecar::{
    SidecarLocation, SidecarSearchError, SidecarSearchLimits, SidecarSearchOutcome,
    SidecarSubtitleFormat, find_sidecar_subtitles,
};

const GENEROUS_LIMITS: SidecarSearchLimits = SidecarSearchLimits {
    max_directory_entries_scanned_per_directory: 1000,
    max_candidates: 100,
    max_subdirectory_depth: 2,
};

fn never_cancelled() -> bool {
    false
}

/// Создаёт файлы (и каталоги) относительно `root`.
fn touch_all(root: &Path, relative_paths: &[&str]) {
    for relative in relative_paths {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, b"x").expect("write file");
    }
}

fn search(root: &Path, limits: &SidecarSearchLimits) -> SidecarSearchOutcome {
    find_sidecar_subtitles(&root.join("Movie.mkv"), limits, &never_cancelled).expect("search")
}

/// Пути кандидатов относительно `root`, в порядке результата.
fn relative_paths(root: &Path, outcome: &SidecarSearchOutcome) -> Vec<String> {
    outcome
        .candidates
        .iter()
        .map(|candidate| {
            candidate
                .path
                .strip_prefix(root)
                .expect("inside root")
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[test]
fn owner_list_is_found_with_languages_and_unrelated_files_are_ignored() {
    let dir = tempfile::tempdir().expect("tempdir");
    touch_all(
        dir.path(),
        &[
            "Movie.mkv",
            "Movie.srt",
            "Movie.ru.srt",
            "Subs/Movie.en.srt",
            "Subs/Movie/2_English.srt",
            "Other.srt",
            "Movie2.srt",
            "Extras/Movie.srt",
            "Subs/Other/x.srt",
            "Movie.txt",
        ],
    );
    let outcome = search(dir.path(), &GENEROUS_LIMITS);
    assert_eq!(
        relative_paths(dir.path(), &outcome),
        [
            "Movie.srt",
            "Movie.ru.srt",
            "Subs/Movie.en.srt",
            "Subs/Movie/2_English.srt"
        ]
    );
    let languages: Vec<Option<&str>> = outcome
        .candidates
        .iter()
        .map(|c| c.language.as_ref().map(|l| l.as_str()))
        .collect();
    assert_eq!(languages, [None, Some("ru"), Some("en"), Some("en")]);
    let locations: Vec<SidecarLocation> = outcome.candidates.iter().map(|c| c.location).collect();
    assert_eq!(
        locations,
        [
            SidecarLocation::VideoDirectory,
            SidecarLocation::VideoDirectory,
            SidecarLocation::SubtitleDirectory,
            SidecarLocation::MovieNamedDirectory
        ]
    );
    assert!(!outcome.report.is_truncated());
    assert!(outcome.report.unreadable_directories.is_empty());
}

#[test]
fn extension_and_stem_case_are_ignored_and_formats_are_mapped() {
    let dir = tempfile::tempdir().expect("tempdir");
    touch_all(
        dir.path(),
        &[
            "movie.SRT",
            "Movie.en.vtt",
            "Movie.English.ass",
            "Movie.old.ssa",
        ],
    );
    let outcome = search(dir.path(), &GENEROUS_LIMITS);
    let found: Vec<(SidecarSubtitleFormat, bool)> = outcome
        .candidates
        .iter()
        .map(|c| (c.format, c.format.is_renderable_now()))
        .collect();
    assert_eq!(
        relative_paths(dir.path(), &outcome),
        [
            "movie.SRT",
            "Movie.English.ass",
            "Movie.en.vtt",
            "Movie.old.ssa"
        ]
    );
    assert_eq!(
        found,
        [
            (SidecarSubtitleFormat::Srt, true),
            (SidecarSubtitleFormat::Ass, true),
            (SidecarSubtitleFormat::WebVtt, true),
            (SidecarSubtitleFormat::Ssa, true),
        ]
    );
    assert_eq!(
        outcome.candidates[1].format.to_subtitle_format(),
        SubtitleFormat::Ass
    );
    assert_eq!(
        outcome.candidates[1].language.as_ref().map(|l| l.as_str()),
        Some("en")
    );
}

#[test]
fn forced_hearing_impaired_and_title_hints_are_recorded() {
    let dir = tempfile::tempdir().expect("tempdir");
    touch_all(dir.path(), &["Movie [Forced].srt", "Movie.pt-BR.SDH.srt"]);
    let outcome = search(dir.path(), &GENEROUS_LIMITS);
    let forced = &outcome.candidates[0];
    assert_eq!(
        forced.path.file_name().and_then(|n| n.to_str()),
        Some("Movie [Forced].srt")
    );
    assert!(forced.disposition.is_forced);
    let sdh = &outcome.candidates[1];
    assert!(sdh.is_hearing_impaired);
    assert!(!sdh.disposition.is_forced);
    assert_eq!(sdh.language.as_ref().map(|l| l.as_str()), Some("pt-BR"));
}

#[test]
fn ordering_follows_groups_then_file_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    touch_all(
        dir.path(),
        &[
            "Movie.zz.srt",
            "Movie.aa.srt",
            "Movie.srt",
            "SUBTITLES/Movie.b.srt",
            "Subs/Movie.a.srt",
            "Subs/movie/1.srt",
            "Subs/MOVIE/0.srt",
        ],
    );
    let outcome = search(dir.path(), &GENEROUS_LIMITS);
    assert_eq!(
        relative_paths(dir.path(), &outcome),
        [
            "Movie.srt",
            "Movie.aa.srt",
            "Movie.zz.srt",
            "Subs/Movie.a.srt",
            "SUBTITLES/Movie.b.srt",
            "Subs/MOVIE/0.srt",
            "Subs/movie/1.srt",
        ]
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_file_name_is_found_with_identical_path_bytes() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let raw_name = OsStr::from_bytes(b"Movie.\xFF\xFE.srt");
    let raw_path = dir.path().join(raw_name);
    fs::write(&raw_path, b"x").expect("write non-utf8 file");
    let outcome = search(dir.path(), &GENEROUS_LIMITS);
    assert_eq!(outcome.candidates.len(), 1);
    assert_eq!(
        outcome.candidates[0].path.as_os_str().as_bytes(),
        raw_path.as_os_str().as_bytes()
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_video_name_matches_by_exact_prefix() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let video = dir.path().join(OsStr::from_bytes(b"Mo\xFFvie.mkv"));
    fs::write(
        dir.path().join(OsStr::from_bytes(b"Mo\xFFvie.ru.srt")),
        b"x",
    )
    .expect("write");
    fs::write(dir.path().join("Movie.ru.srt"), b"x").expect("write");
    let outcome =
        find_sidecar_subtitles(&video, &GENEROUS_LIMITS, &never_cancelled).expect("search");
    assert_eq!(outcome.candidates.len(), 1);
    assert_eq!(
        outcome.candidates[0].language.as_ref().map(|l| l.as_str()),
        Some("ru")
    );
}

#[test]
fn candidate_limit_keeps_best_ordered_and_reports_dropped() {
    let dir = tempfile::tempdir().expect("tempdir");
    touch_all(
        dir.path(),
        &[
            "Movie.srt",
            "Movie.a.srt",
            "Movie.b.srt",
            "Subs/Movie.c.srt",
        ],
    );
    let limits = SidecarSearchLimits {
        max_candidates: 2,
        ..GENEROUS_LIMITS
    };
    let outcome = search(dir.path(), &limits);
    assert_eq!(
        relative_paths(dir.path(), &outcome),
        ["Movie.srt", "Movie.a.srt"]
    );
    assert_eq!(outcome.report.candidates_dropped_by_limit, 2);
    assert!(outcome.report.is_truncated());
}

#[test]
fn entry_limit_stops_scanning_directory_and_is_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    for index in 0..10 {
        touch_all(dir.path(), &[&format!("Movie.{index}.srt")]);
    }
    let limits = SidecarSearchLimits {
        max_directory_entries_scanned_per_directory: 4,
        ..GENEROUS_LIMITS
    };
    let outcome = search(dir.path(), &limits);
    assert_eq!(outcome.candidates.len(), 4);
    assert_eq!(outcome.report.entries_scanned, 4);
    assert_eq!(outcome.report.directories_truncated_by_entry_limit, 1);
    assert!(outcome.report.is_truncated());
}

#[test]
fn depth_limit_skips_subdirectories_and_counts_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    touch_all(
        dir.path(),
        &["Movie.srt", "Subs/Movie.en.srt", "Subs/Movie/1.srt"],
    );

    let video_dir_only = SidecarSearchLimits {
        max_subdirectory_depth: 0,
        ..GENEROUS_LIMITS
    };
    let outcome = search(dir.path(), &video_dir_only);
    assert_eq!(relative_paths(dir.path(), &outcome), ["Movie.srt"]);
    assert_eq!(outcome.report.subdirectories_skipped_by_depth_limit, 1);

    let without_movie_directory = SidecarSearchLimits {
        max_subdirectory_depth: 1,
        ..GENEROUS_LIMITS
    };
    let outcome = search(dir.path(), &without_movie_directory);
    assert_eq!(
        relative_paths(dir.path(), &outcome),
        ["Movie.srt", "Subs/Movie.en.srt"]
    );
    assert_eq!(outcome.report.subdirectories_skipped_by_depth_limit, 1);
}

#[test]
fn cancellation_returns_cancelled_between_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    touch_all(dir.path(), &["Movie.srt", "Movie.a.srt", "Movie.b.srt"]);
    let checks = AtomicUsize::new(0);
    let cancel_after_two_checks = || checks.fetch_add(1, Ordering::SeqCst) >= 2;
    let result = find_sidecar_subtitles(
        &dir.path().join("Movie.mkv"),
        &GENEROUS_LIMITS,
        &cancel_after_two_checks,
    );
    assert_eq!(result.unwrap_err(), SidecarSearchError::Cancelled);

    let result = find_sidecar_subtitles(&dir.path().join("Movie.mkv"), &GENEROUS_LIMITS, &|| true);
    assert_eq!(result.unwrap_err(), SidecarSearchError::Cancelled);
}

#[test]
fn missing_video_directory_is_reported_not_fatal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let video = dir.path().join("absent").join("Movie.mkv");
    let outcome =
        find_sidecar_subtitles(&video, &GENEROUS_LIMITS, &never_cancelled).expect("search");
    assert!(outcome.candidates.is_empty());
    assert_eq!(outcome.report.unreadable_directories.len(), 1);
    assert_eq!(
        outcome.report.unreadable_directories[0].error_kind,
        std::io::ErrorKind::NotFound
    );
}

#[cfg(unix)]
#[test]
fn unreadable_subdirectory_is_skipped_and_reported() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    touch_all(dir.path(), &["Movie.srt", "Subs/Movie.en.srt"]);
    let subs = dir.path().join("Subs");
    fs::set_permissions(&subs, fs::Permissions::from_mode(0o000)).expect("chmod");
    // От root права не действуют: тогда проверять нечего.
    let readable_anyway = fs::read_dir(&subs).is_ok();
    let outcome = search(dir.path(), &GENEROUS_LIMITS);
    fs::set_permissions(&subs, fs::Permissions::from_mode(0o755)).expect("restore chmod");
    if readable_anyway {
        return;
    }
    assert_eq!(relative_paths(dir.path(), &outcome), ["Movie.srt"]);
    assert_eq!(outcome.report.unreadable_directories.len(), 1);
    assert_eq!(outcome.report.unreadable_directories[0].path, subs);
    assert_eq!(
        outcome.report.unreadable_directories[0].error_kind,
        std::io::ErrorKind::PermissionDenied
    );
}

#[test]
fn video_path_without_parent_or_stem_is_a_typed_error() {
    let limits = GENEROUS_LIMITS;
    assert_eq!(
        find_sidecar_subtitles(Path::new("/"), &limits, &never_cancelled).unwrap_err(),
        SidecarSearchError::VideoPathHasNoParent
    );
    assert_eq!(
        find_sidecar_subtitles(Path::new(""), &limits, &never_cancelled).unwrap_err(),
        SidecarSearchError::VideoPathHasNoParent
    );
    assert_eq!(
        find_sidecar_subtitles(Path::new("/media/.."), &limits, &never_cancelled).unwrap_err(),
        SidecarSearchError::VideoPathHasNoFileStem
    );
}

#[test]
fn video_file_itself_is_never_a_candidate() {
    let dir = tempfile::tempdir().expect("tempdir");
    touch_all(dir.path(), &["Movie.srt"]);
    // Видео-«файл» с субтитровым расширением не должен найти сам себя.
    let video: PathBuf = dir.path().join("Movie.srt");
    let outcome =
        find_sidecar_subtitles(&video, &GENEROUS_LIMITS, &never_cancelled).expect("search");
    assert!(outcome.candidates.is_empty());
}
