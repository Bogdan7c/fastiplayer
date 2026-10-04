//! Сквозные тесты текстов ошибок: настоящий файл во временном каталоге → настоящая
//! подготовка `prepare_local_open` → классификация → текст для пользователя.
//!
//! Родительский каталог намеренно называется `private-parent-dir`: каждый тест проверяет,
//! что имя файла в тексте есть, а имя папки — нет (решение владельца 1а).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use fastiplayer_config::PlayerDemuxConfig;
use source_core::CancellationToken;

use super::{
    local_open_failure_message, local_open_failure_row_summary, local_open_preparing_message,
};
use crate::media_open::local::tests::{mpeg_ts_h264_aac_bytes, pcm_wav_bytes};
use crate::media_open::{LocalOpenFailureOutcome, LocalOpenFailureReason, prepare_local_open};

const PRIVATE_PARENT_DIR: &str = "private-parent-dir";

/// Временный каталог с «приватным» родителем, в котором лежат тестовые файлы.
struct PrivateMediaDirectory {
    _root: tempfile::TempDir,
    parent: PathBuf,
}

impl PrivateMediaDirectory {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temp directory");
        let parent = root.path().join(PRIVATE_PARENT_DIR);
        fs::create_dir(&parent).expect("create private parent directory");
        Self {
            _root: root,
            parent,
        }
    }

    fn file_with_bytes(&self, file_name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.parent.join(file_name);
        fs::write(&path, bytes).expect("write media fixture");
        path
    }
}

/// Прогоняет путь через настоящую подготовку и возвращает итог для пользователя.
fn prepare_and_classify(path: &Path) -> LocalOpenFailureOutcome {
    match prepare_local_open(
        path,
        &PlayerDemuxConfig::default(),
        None,
        CancellationToken::never_cancelled(),
        || false,
    ) {
        Ok(_) => panic!("fixture {path:?} должен не открыться"),
        Err(error) => error.user_outcome(),
    }
}

/// Проверяет причину и итоговый текст одного случая из таблицы владельца.
fn assert_failure_text(
    path: &Path,
    expected_reason: LocalOpenFailureReason,
    expected_message: &str,
) {
    assert_eq!(
        prepare_and_classify(path),
        LocalOpenFailureOutcome::Failed(expected_reason),
        "причина для {path:?}"
    );
    let message = local_open_failure_message(path, expected_reason);
    assert_eq!(message, expected_message);
    assert!(
        !message.contains(PRIVATE_PARENT_DIR),
        "текст не должен раскрывать родительский каталог: {message}"
    );
}

#[test]
fn missing_file_reports_not_found_with_filename_only() {
    let directory = PrivateMediaDirectory::new();
    let path = directory.parent.join("missing-clip.mkv");

    assert_failure_text(
        &path,
        LocalOpenFailureReason::FileNotFound,
        "Не удалось открыть «missing-clip.mkv»: файл не найден",
    );
}

#[cfg(unix)]
#[test]
fn unreadable_file_reports_access_denied() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = PrivateMediaDirectory::new();
    let path = directory.file_with_bytes("locked-clip.wav", &pcm_wav_bytes());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod 000");
    if fs::File::open(&path).is_ok() {
        // Под root права не ограничивают чтение — сценарий невоспроизводим.
        eprintln!("пропуск: процесс читает файл с правами 000 (вероятно, root)");
        return;
    }

    assert_failure_text(
        &path,
        LocalOpenFailureReason::AccessDenied,
        "Не удалось открыть «locked-clip.wav»: нет доступа к файлу",
    );
}

#[test]
fn directory_reports_folder_not_file() {
    let directory = PrivateMediaDirectory::new();
    let path = directory.parent.join("folder-named-like-clip.mkv");
    fs::create_dir(&path).expect("create directory target");

    assert_failure_text(
        &path,
        LocalOpenFailureReason::IsDirectory,
        "Не удалось открыть «folder-named-like-clip.mkv»: это папка, а не файл",
    );
}

#[test]
fn zero_byte_file_reports_empty_file() {
    let directory = PrivateMediaDirectory::new();
    let path = directory.file_with_bytes("empty-clip.mp4", &[]);

    assert_failure_text(
        &path,
        LocalOpenFailureReason::EmptyFile,
        "Не удалось открыть «empty-clip.mp4»: файл пустой",
    );
}

#[test]
fn text_garbage_reports_unrecognized_format() {
    let directory = PrivateMediaDirectory::new();
    let garbage = b"this is plain text, definitely not a media container\n".repeat(64);
    let path = directory.file_with_bytes("notes-renamed.mkv", &garbage);

    assert_failure_text(
        &path,
        LocalOpenFailureReason::UnrecognizedFormat,
        "Не удалось открыть «notes-renamed.mkv»: формат файла не распознан или не поддерживается",
    );
}

#[test]
fn png_picture_renamed_to_mp4_reports_unrecognized_format() {
    let directory = PrivateMediaDirectory::new();
    let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00"
        .to_vec();
    png.resize(1024, 0);
    let path = directory.file_with_bytes("photo.mp4", &png);

    assert_failure_text(
        &path,
        LocalOpenFailureReason::UnrecognizedFormat,
        "Не удалось открыть «photo.mp4»: формат файла не распознан или не поддерживается",
    );
}

#[test]
fn truncated_transport_stream_reports_damaged_file() {
    let directory = PrivateMediaDirectory::new();
    let mut truncated = mpeg_ts_h264_aac_bytes();
    truncated.truncate(188 * 2 + 17);
    let path = directory.file_with_bytes("cut-recording.ts", &truncated);

    assert_failure_text(
        &path,
        LocalOpenFailureReason::DamagedOrTruncated,
        "Не удалось открыть «cut-recording.ts»: файл повреждён или обрезан",
    );
}

#[test]
fn truncated_mp4_reports_damaged_file() {
    let directory = PrivateMediaDirectory::new();
    // Валидный `ftyp`, после которого обрывается `moov`: контейнер узнаётся по сигнатуре,
    // но разборщик ломается на содержимом.
    let mut truncated_mp4 = Vec::new();
    truncated_mp4.extend_from_slice(&24_u32.to_be_bytes());
    truncated_mp4.extend_from_slice(b"ftypisom\x00\x00\x02\x00isomavc1");
    truncated_mp4.extend_from_slice(&4096_u32.to_be_bytes());
    truncated_mp4.extend_from_slice(b"moov\x00\x00\x00\x6cmvhd");
    let path = directory.file_with_bytes("cut-video.mp4", &truncated_mp4);

    assert_failure_text(
        &path,
        LocalOpenFailureReason::DamagedOrTruncated,
        "Не удалось открыть «cut-video.mp4»: файл повреждён или обрезан",
    );
}

/// Путь без имени (`/`) не раскрывается: вместо него нейтральная метка.
#[test]
fn path_without_filename_uses_neutral_label() {
    assert_failure_text(
        Path::new("/"),
        LocalOpenFailureReason::IsDirectory,
        "Не удалось открыть «(без имени)»: это папка, а не файл",
    );
}

/// Имя не в UTF-8 строится lossy через настоящий prepare-путь, без паники.
#[cfg(unix)]
#[test]
fn non_utf8_filename_message_is_lossy_without_panic() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let directory = PrivateMediaDirectory::new();
    let path = directory
        .parent
        .join(OsStr::from_bytes(b"missing-\xff-clip.mkv"));

    assert_failure_text(
        &path,
        LocalOpenFailureReason::FileNotFound,
        "Не удалось открыть «missing-\u{fffd}-clip.mkv»: файл не найден",
    );
}

/// Отмена — не ошибка: подготовка возвращает отдельный итог, а не причину для показа.
#[test]
fn cancelled_preparation_is_not_user_visible_failure() {
    let directory = PrivateMediaDirectory::new();
    let path = directory.file_with_bytes("cancelled-clip.wav", &pcm_wav_bytes());

    let outcome = match prepare_local_open(
        &path,
        &PlayerDemuxConfig::default(),
        None,
        CancellationToken::never_cancelled(),
        || true,
    ) {
        Ok(_) => panic!("отменённая подготовка не должна завершиться успехом"),
        Err(error) => error.user_outcome(),
    };

    assert_eq!(outcome, LocalOpenFailureOutcome::Cancelled);
}

/// Каждая причина имеет свою формулировку — иначе пользователь снова видит «одно и то же».
#[test]
fn every_reason_has_distinct_row_summary_starting_with_capital_letter() {
    let reasons = [
        LocalOpenFailureReason::FileNotFound,
        LocalOpenFailureReason::AccessDenied,
        LocalOpenFailureReason::IsDirectory,
        LocalOpenFailureReason::EmptyFile,
        LocalOpenFailureReason::UnrecognizedFormat,
        LocalOpenFailureReason::DamagedOrTruncated,
        LocalOpenFailureReason::ReadFailed,
        LocalOpenFailureReason::ReadTimedOut,
        LocalOpenFailureReason::NoAudioOrVideo,
        LocalOpenFailureReason::ChangedDuringOpen,
        LocalOpenFailureReason::InternalError,
    ];

    let summaries: Vec<String> = reasons
        .iter()
        .map(|reason| local_open_failure_row_summary(*reason))
        .collect();
    let distinct: HashSet<&String> = summaries.iter().collect();

    assert_eq!(distinct.len(), reasons.len(), "{summaries:?}");
    assert_eq!(
        local_open_failure_row_summary(LocalOpenFailureReason::FileNotFound),
        "Файл не найден"
    );
    for summary in &summaries {
        let first_letter = summary.chars().next().expect("summary не пустой");
        assert!(first_letter.is_uppercase(), "{summary}");
    }
}

#[test]
fn preparing_message_names_file_without_parent_directory() {
    let path = Path::new("/home/private-parent-dir/clip.mkv");

    assert_eq!(local_open_preparing_message(path), "Открываем «clip.mkv»…");
}
