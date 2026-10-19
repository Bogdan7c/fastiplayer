//! Особые файлы (FIFO, каталог) под видом субтитров не должны подвешивать фоновую задачу:
//! чтение FIFO без писателя блокируется навсегда и держало бы поток (и выход приложения).

use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use subtitle_sidecar::describe_subtitle_file;

use super::super::sidecar_job::{SidecarJobOutcome, SubtitleLoadFailure, run_sidecar_job};
use super::*;

/// Сколько максимум ждём ответ задачи: обычный отказ приходит за миллисекунды.
const PROMPT_ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

/// Выполняет `LoadFile` для `path` в отдельном потоке и ждёт итог не дольше таймаута.
/// `None` — задача зависла (поток при этом остаётся брошенным).
fn load_file_with_deadline(path: &Path) -> Option<SidecarJobOutcome> {
    let candidate = describe_subtitle_file(path).expect("a .srt name is a subtitle candidate");
    let request = SidecarJobRequest {
        task: SidecarJobTask::LoadFile { candidate },
        fonts: FontLibraryPlan::ReuseExisting,
        limits: default_load_limits(),
    };
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        // Получатель мог уйти по таймауту: тогда отправлять уже некому, это не ошибка.
        let _ = sender.send(run_sidecar_job(&request, &|| false));
    });
    receiver.recv_timeout(PROMPT_ANSWER_TIMEOUT).ok()
}

#[test]
fn fifo_named_like_a_subtitle_fails_promptly_instead_of_hanging() {
    let directory = tempfile::tempdir().expect("tempdir");
    let fifo = directory.path().join("Pipe.srt");
    let status = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo must be available on this platform");
    assert!(status.success(), "mkfifo failed");

    let outcome = load_file_with_deadline(&fifo).expect("loading a FIFO must not hang");

    assert!(
        matches!(
            outcome,
            SidecarJobOutcome::Failed(SubtitleLoadFailure::NotARegularFile)
        ),
        "a FIFO is reported as not a regular file"
    );
}

#[test]
fn directory_named_like_a_subtitle_is_reported_as_not_a_regular_file() {
    let directory = tempfile::tempdir().expect("tempdir");
    let folder = directory.path().join("Folder.srt");
    std::fs::create_dir(&folder).expect("create dir");

    let outcome = load_file_with_deadline(&folder).expect("must answer");

    assert!(matches!(
        outcome,
        SidecarJobOutcome::Failed(SubtitleLoadFailure::NotARegularFile)
    ));
}

#[test]
fn symlink_to_a_regular_subtitle_still_loads() {
    let directory = tempfile::tempdir().expect("tempdir");
    let real = directory.path().join("Real.srt");
    std::fs::write(&real, TWO_CUE_SRT).expect("write srt");
    let link = directory.path().join("Link.srt");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    let outcome = load_file_with_deadline(&link).expect("must answer");

    let SidecarJobOutcome::FileLoaded { track, .. } = outcome else {
        panic!("a symlink to a regular file must load");
    };
    assert_eq!(track.timeline.len(), 2);
}
