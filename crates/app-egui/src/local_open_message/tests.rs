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
        media_source_open::local_media::AttachmentReadLimits::default(),
        None,
        CancellationToken::never_cancelled(),
        || false,
    ) {
        Ok(_) => panic!("fixture {path:?} should not open."),
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
        "The text should not disclose the parent catalog: {message}"
    );
}

#[test]
fn missing_file_reports_not_found_with_filename_only() {
    let directory = PrivateMediaDirectory::new();
    let path = directory.parent.join("missing-clip.mkv");

    assert_failure_text(
        &path,
        LocalOpenFailureReason::FileNotFound,
        "Could not open “missing-clip.mkv”: file not found",
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
        eprintln!("skipped: process can read a file with mode 000 (probably root)");
        return;
    }

    assert_failure_text(
        &path,
        LocalOpenFailureReason::AccessDenied,
        "Could not open “locked-clip.wav”: access to the file is denied",
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
        "Could not open “folder-named-like-clip.mkv”: this is a folder, not a file",
    );
}

#[test]
fn zero_byte_file_reports_empty_file() {
    let directory = PrivateMediaDirectory::new();
    let path = directory.file_with_bytes("empty-clip.mp4", &[]);

    assert_failure_text(
        &path,
        LocalOpenFailureReason::EmptyFile,
        "Could not open “empty-clip.mp4”: the file is empty",
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
        "Could not open “notes-renamed.mkv”: file format is unrecognized or unsupported",
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
        "Could not open “photo.mp4”: file format is unrecognized or unsupported",
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
        "Could not open “cut-recording.ts”: the file is corrupted or truncated",
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
        "Could not open “cut-video.mp4”: the file is corrupted or truncated",
    );
}

/// Путь без имени (`/`) не раскрывается: вместо него нейтральная метка.
#[test]
fn path_without_filename_uses_neutral_label() {
    assert_failure_text(
        Path::new("/"),
        LocalOpenFailureReason::IsDirectory,
        "Could not open “(unnamed)”: this is a folder, not a file",
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
        "Could not open “missing-\u{fffd}-clip.mkv”: file not found",
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
        media_source_open::local_media::AttachmentReadLimits::default(),
        None,
        CancellationToken::never_cancelled(),
        || true,
    ) {
        Ok(_) => panic!("Abandoned preparation should not be successful"),
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
        "File not found"
    );
    for summary in &summaries {
        let first_letter = summary.chars().next().expect("sketchy");
        assert!(first_letter.is_uppercase(), "{summary}");
    }
}

#[test]
fn preparing_message_names_file_without_parent_directory() {
    let path = Path::new("/home/private-parent-dir/clip.mkv");

    assert_eq!(local_open_preparing_message(path), "Opening “clip.mkv”…");
}
