//! Тесты обхода папки на реальной файловой системе (временные каталоги).

use std::ffi::OsString;
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use source_core::CancellationToken;

use super::{
    FolderWalkError, FolderWalkLimits, FolderWalkOutcome, FolderWalkRequest, FolderWalkTruncation,
    walk_media_folder,
};

static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

/// Уникальный временный каталог, который удаляется при падении теста тоже.
struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fastiplayer-folder-walk-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("тестовая папка создаётся");
        Self { path }
    }

    fn file(&self, relative: &str) -> PathBuf {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("родитель файла создаётся");
        }
        fs::write(&path, []).expect("файл создаётся");
        path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn limits(max_files: usize, max_depth: usize) -> FolderWalkLimits {
    FolderWalkLimits::new(
        NonZeroUsize::new(max_files).expect("лимит файлов ненулевой"),
        max_depth,
    )
}

/// Правило теста: media — всё, кроме `.txt`.
fn accept_non_text(path: &Path) -> bool {
    path.extension().is_none_or(|extension| extension != "txt")
}

fn walk(root: &Path, limits: FolderWalkLimits) -> FolderWalkOutcome {
    walk_media_folder(FolderWalkRequest::new(
        root,
        limits,
        &CancellationToken::new(),
        accept_non_text,
    ))
    .expect("обход успешен")
}

/// Пути результата относительно корня — удобно сравнивать порядок.
fn relative(root: &Path, outcome: &FolderWalkOutcome) -> Vec<PathBuf> {
    outcome
        .files
        .iter()
        .map(|path| path.strip_prefix(root).expect("внутри корня").to_path_buf())
        .collect()
}

fn paths(items: &[&str]) -> Vec<PathBuf> {
    items.iter().map(PathBuf::from).collect()
}

/// Файлы папки идут до подпапок, всё в natural-порядке, подпапки в глубину.
#[test]
fn files_come_before_subfolders_in_natural_depth_first_order() {
    let dir = TestDirectory::new("order");
    dir.file("10.mkv");
    dir.file("2.mkv");
    dir.file("b/z.mkv");
    dir.file("b/a/deep.mkv");
    dir.file("a/1.mkv");
    dir.file("a/x/inner.mkv");

    let outcome = walk(&dir.path, limits(100, 8));

    assert_eq!(
        relative(&dir.path, &outcome),
        paths(&[
            "2.mkv",
            "10.mkv",
            "a/1.mkv",
            "a/x/inner.mkv",
            "b/z.mkv",
            "b/a/deep.mkv",
        ])
    );
    assert_eq!(outcome.truncation, None);
}

/// Не-media (по правилу владельца), скрытые файлы и скрытые папки не попадают в результат.
#[test]
fn rejected_and_hidden_entries_are_skipped() {
    let dir = TestDirectory::new("hidden");
    dir.file("movie.mkv");
    dir.file("notes.txt");
    dir.file(".secret.mkv");
    dir.file(".hidden_dir/inside.mkv");
    dir.file("visible/ok.mkv");

    let outcome = walk(&dir.path, limits(100, 8));

    assert_eq!(
        relative(&dir.path, &outcome),
        paths(&["movie.mkv", "visible/ok.mkv"])
    );
}

/// Лимит файлов оставляет ПЕРВЫЕ N по порядку обхода и помечает усечение.
#[test]
fn file_limit_keeps_first_files_and_reports_truncation() {
    let dir = TestDirectory::new("file-limit");
    dir.file("1.mkv");
    dir.file("2.mkv");
    dir.file("3.mkv");
    dir.file("sub/4.mkv");

    let outcome = walk(&dir.path, limits(2, 8));

    assert_eq!(relative(&dir.path, &outcome), paths(&["1.mkv", "2.mkv"]));
    assert_eq!(outcome.truncation, Some(FolderWalkTruncation::FileLimit));
}

/// Ровно N файлов — это полный результат, а не усечение.
#[test]
fn exactly_limit_files_is_not_truncated() {
    let dir = TestDirectory::new("exact");
    dir.file("1.mkv");
    dir.file("sub/2.mkv");

    let outcome = walk(&dir.path, limits(2, 8));

    assert_eq!(outcome.files.len(), 2);
    assert_eq!(outcome.truncation, None);
}

/// Глубже лимита не заходим; факт пропуска помечается, файлы доступных уровней остаются.
#[test]
fn depth_limit_skips_deeper_folders_and_reports_truncation() {
    let dir = TestDirectory::new("depth");
    dir.file("root.mkv");
    dir.file("a/one.mkv");
    dir.file("a/b/two.mkv");

    let outcome = walk(&dir.path, limits(100, 1));

    assert_eq!(
        relative(&dir.path, &outcome),
        paths(&["root.mkv", "a/one.mkv"])
    );
    assert_eq!(outcome.truncation, Some(FolderWalkTruncation::DepthLimit));
}

/// Глубина 0 — только сама папка; без подпапок усечения по глубине нет.
#[test]
fn depth_zero_walks_only_the_root_folder() {
    let dir = TestDirectory::new("depth-zero");
    dir.file("root.mkv");
    let flat = walk(&dir.path, limits(100, 0));
    assert_eq!(relative(&dir.path, &flat), paths(&["root.mkv"]));
    assert_eq!(flat.truncation, None);

    dir.file("sub/hidden-by-depth.mkv");
    let with_sub = walk(&dir.path, limits(100, 0));
    assert_eq!(relative(&dir.path, &with_sub), paths(&["root.mkv"]));
    assert_eq!(with_sub.truncation, Some(FolderWalkTruncation::DepthLimit));
}

/// Пробелы в именах и не-UTF-8 имена сохраняются байт-в-байт.
#[cfg(unix)]
#[test]
fn spaces_and_non_utf8_names_are_preserved_exactly() {
    use std::os::unix::ffi::OsStringExt;

    let dir = TestDirectory::new("names");
    dir.file("with space.mkv");
    let raw_name = OsString::from_vec(vec![b'b', 0xFF, 0xFE, b'.', b'm', b'k', b'v']);
    let raw_path = dir.path.join(&raw_name);
    fs::write(&raw_path, []).expect("файл с не-UTF-8 именем создаётся");

    let outcome = walk(&dir.path, limits(100, 8));

    assert!(outcome.files.contains(&dir.path.join("with space.mkv")));
    assert!(outcome.files.contains(&raw_path));
    assert_eq!(outcome.files.len(), 2);
}

/// Символьная ссылка на предка не создаёт цикл; ссылка на файл считается файлом.
#[cfg(unix)]
#[test]
fn symlinked_directories_are_not_followed_but_file_links_are() {
    let dir = TestDirectory::new("symlink");
    let real_file = dir.file("real/movie.mkv");
    std::os::unix::fs::symlink(&dir.path, dir.path.join("real/loop"))
        .expect("ссылка-цикл создаётся");
    std::os::unix::fs::symlink(&real_file, dir.path.join("link.mkv"))
        .expect("ссылка на файл создаётся");

    let outcome = walk(&dir.path, limits(100, 32));

    assert_eq!(
        relative(&dir.path, &outcome),
        paths(&["link.mkv", "real/movie.mkv"])
    );
    assert_eq!(outcome.truncation, None);
}

/// Нечитаемая подпапка пропускается и считается; остальные файлы возвращаются.
#[cfg(unix)]
#[test]
fn unreadable_subfolder_is_skipped_and_counted() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDirectory::new("unreadable");
    dir.file("a.mkv");
    dir.file("locked/inside.mkv");
    let locked = dir.path.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("права сняты");
    // Под root права не действуют: тогда сценарий неприменим.
    if fs::read_dir(&locked).is_ok() {
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("права возвращены");
        return;
    }

    let outcome = walk(&dir.path, limits(100, 8));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("права возвращены");

    assert_eq!(relative(&dir.path, &outcome), paths(&["a.mkv"]));
    assert_eq!(outcome.unreadable_directories, 1);
}

/// Отсутствующий корень — типизированная ошибка чтения, а не пустой успех.
#[test]
fn missing_root_is_a_typed_read_error() {
    let dir = TestDirectory::new("missing");
    let missing = dir.path.join("nope");

    let result = walk_media_folder(FolderWalkRequest::new(
        &missing,
        limits(10, 8),
        &CancellationToken::new(),
        accept_non_text,
    ));

    assert!(matches!(
        result,
        Err(FolderWalkError::ReadRoot(std::io::ErrorKind::NotFound))
    ));
}

/// Уже отменённый токен останавливает обход без результата.
#[test]
fn cancelled_token_stops_the_walk_without_partial_result() {
    let dir = TestDirectory::new("cancel");
    dir.file("a.mkv");
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let result = walk_media_folder(FolderWalkRequest::new(
        &dir.path,
        limits(10, 8),
        &cancellation,
        accept_non_text,
    ));

    assert!(matches!(result, Err(FolderWalkError::Cancelled)));
}

/// Пустая папка — успешный пустой результат (решение о сообщении принимает приложение).
#[test]
fn empty_folder_yields_empty_outcome() {
    let dir = TestDirectory::new("empty");

    let outcome = walk(&dir.path, limits(10, 8));

    assert!(outcome.files.is_empty());
    assert_eq!(outcome.truncation, None);
}
