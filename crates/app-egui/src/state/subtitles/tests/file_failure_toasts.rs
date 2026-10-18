//! Отказ файла, который пользователь добавил сам (бросок, «Load file…»), даёт ровно один
//! toast с причиной; отказ автоматически найденного файла toast-а не даёт (только серую
//! подсказку в меню). Настоящий владелец и настоящая фоновая загрузка, доставка — в настоящий
//! центр уведомлений.

use std::process::Command;
use std::time::Instant;

use fastiplayer_config::SubtitlesConfig;

use super::super::menu_view::{
    SubtitleFileProblem, SubtitleTrackAvailability, SubtitleUnavailableReason,
};
use super::super::user_file_failures::deliver_user_file_failures;
use super::track_selection::{movie_with, settle};
use super::*;
use crate::state::notifications::{NotificationCenter, OpenProgress};
use crate::subtitle_message::unavailable_reason_text;
use crate::ui::animation::UiMotion;

/// Тексты toast-ов центра в порядке показа.
fn toast_texts(center: &mut NotificationCenter, now: Instant) -> Vec<String> {
    center
        .frame(OpenProgress::Idle, UiMotion::Standard, now)
        .toasts
        .iter()
        .map(|toast| toast.message.to_string())
        .collect()
}

/// Добавляет `path` как пользователь, ждёт загрузку и возвращает показанные toast-ы.
fn toasts_after_user_adds(owner: &mut SubtitleOwner, path: &Path) -> Vec<String> {
    owner.attach_file(path);
    settle(owner);
    let failures = owner.take_user_file_failures();
    let mut center = NotificationCenter::default();
    let now = Instant::now();
    deliver_user_file_failures(&mut center, &failures, now);
    toast_texts(&mut center, now)
}

/// Владелец с открытым видео без субтитров рядом.
fn owner_with_video() -> (SubtitleOwner, tempfile::TempDir) {
    let (directory, video) = movie_with(&[]);
    let mut owner = owner();
    begin_and_wait(&mut owner, instance(1), &video);
    (owner, directory)
}

#[test]
fn missing_file_gives_one_cannot_open_toast_with_the_file_name_and_no_directory() {
    let (mut owner, directory) = owner_with_video();
    let missing = directory.path().join("Gone.srt");

    let toasts = toasts_after_user_adds(&mut owner, &missing);

    assert_eq!(
        toasts,
        ["Could not open \"Gone.srt\". Check that the file exists and you can read it."]
    );
    let directory_text = directory.path().to_string_lossy().into_owned();
    assert!(!toasts[0].contains(&directory_text), "no directory path");
}

#[test]
fn fifo_gives_the_cannot_open_toast_instead_of_hanging() {
    let (mut owner, directory) = owner_with_video();
    let fifo = directory.path().join("Pipe.srt");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo")
            .success()
    );

    let toasts = toasts_after_user_adds(&mut owner, &fifo);

    assert_eq!(
        toasts,
        ["Could not open \"Pipe.srt\". Check that the file exists and you can read it."]
    );
}

#[test]
fn oversized_file_gives_the_too_large_toast() {
    let (mut owner, directory) = owner_with_video();
    owner.apply_config(&SubtitlesConfig {
        max_file_size_mb: 1,
        ..enabled_config()
    });
    let big = directory.path().join("Huge.srt");
    std::fs::write(&big, "x".repeat(2 * 1024 * 1024)).expect("write big file");

    let toasts = toasts_after_user_adds(&mut owner, &big);

    assert_eq!(toasts, ["\"Huge.srt\" is too large to use as subtitles."]);
}

#[test]
fn unparsable_and_empty_files_give_the_cannot_parse_toast() {
    let (mut owner, directory) = owner_with_video();
    let not_vtt = directory.path().join("Broken.vtt");
    std::fs::write(&not_vtt, "this is not webvtt").expect("write");
    let no_cues = directory.path().join("Empty.srt");
    std::fs::write(&no_cues, "just some text without any timing\n").expect("write");
    let expected = |name: &str| {
        format!(
            "No subtitles could be read from \"{name}\". The file may be damaged or in an unsupported format."
        )
    };

    assert_eq!(
        toasts_after_user_adds(&mut owner, &not_vtt),
        [expected("Broken.vtt")]
    );
    assert_eq!(
        toasts_after_user_adds(&mut owner, &no_cues),
        [expected("Empty.srt")]
    );
}

#[test]
fn a_file_that_loads_fine_gives_no_toast() {
    let (mut owner, directory) = owner_with_video();
    let good = directory.path().join("Good.srt");
    std::fs::write(&good, TWO_CUE_SRT).expect("write");

    assert!(toasts_after_user_adds(&mut owner, &good).is_empty());
}

#[test]
fn failures_are_handed_out_once() {
    let (mut owner, directory) = owner_with_video();
    owner.attach_file(&directory.path().join("Gone.srt"));
    settle(&mut owner);

    assert_eq!(owner.take_user_file_failures().len(), 1);
    assert!(owner.take_user_file_failures().is_empty());
}

#[test]
fn failing_auto_discovered_sidecar_gives_no_toast_but_a_menu_hint() {
    let (directory, video) = movie_with(&[("Movie.srt", "no cues in here at all\n")]);
    let mut owner = owner();
    begin_and_wait(&mut owner, instance(1), &video);
    settle(&mut owner);

    assert!(
        owner.take_user_file_failures().is_empty(),
        "automatic discovery must stay quiet"
    );
    let view = owner.menu_view();
    assert_eq!(
        view.tracks[0].availability,
        SubtitleTrackAvailability::Unavailable(SubtitleUnavailableReason::LoadFailed(
            SubtitleFileProblem::CannotParse
        ))
    );
    drop(directory);
}

#[test]
fn menu_hints_distinguish_the_reasons_without_paths() {
    let hint = |problem| unavailable_reason_text(&SubtitleUnavailableReason::LoadFailed(problem));
    assert_eq!(
        hint(SubtitleFileProblem::CannotOpen),
        "This subtitle file could not be opened."
    );
    assert_eq!(
        hint(SubtitleFileProblem::TooLarge),
        "This subtitle file is too large."
    );
    assert_eq!(
        hint(SubtitleFileProblem::CannotParse),
        "No subtitles could be read from this file."
    );
    assert_eq!(
        hint(SubtitleFileProblem::Other),
        "This subtitle file could not be read."
    );
}
