//! Повтор записи «чёрного ящика» плеера (сессия 09b2) в источник: запись из каталога
//! (`stream.json` с `at_ms` у шагов + `NNN.json`/`NNN.bin`) доходит до кадров так же,
//! как исходная фикстура.
//!
//! Писатель записи живёт в процессе браузерного режима (`app-egui`), этот crate от него
//! не зависит; здесь запись собирается по описанию формата, а «записанное = присланное»
//! и повтор в настоящем процессе проверяют тесты `app-egui`
//! (`tests/browser_session_process/replay_recording.rs`).
//!
//! Игнорируемый тест берёт настоящую запись:
//! `FASTIPLAYER_REPLAY_RECORDING=/путь/к/session-1 cargo test -p pushed-media-source --test recorded_directory_to_frame -- --ignored --nocapture`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::path::Path;
use std::time::Duration;

use player_core::PlaybackState;
use support::media::protocol_fixtures_dir;
use support::playback::{open, play};
use support::recorded::{replay, replay_directory};

const FIXTURE: &str = "two-source-buffers-mid-box-cuts";

/// Копирует фикстуру в `target` в формате записи: каждому шагу — своё `at_ms`, в конце
/// страница закрывается (`tab_gone`), как в записи настоящего сеанса.
fn write_recording_like_the_black_box(target: &Path) {
    let source = protocol_fixtures_dir().join("streams").join(FIXTURE);
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(source.join("stream.json")).unwrap()).unwrap();
    let mut steps = Vec::new();
    for (index, step) in manifest["steps"].as_array().unwrap().iter().enumerate() {
        let (key, file) = match (step["text"].as_str(), step["binary"].as_str()) {
            (Some(file), None) => ("text", file),
            (None, Some(file)) => ("binary", file),
            _ => panic!("fixture step is text or binary"),
        };
        std::fs::copy(source.join(file), target.join(file)).unwrap();
        steps.push(serde_json::json!({ key: file, "at_ms": index * 40 }));
    }
    let tab_gone = "999.json";
    std::fs::write(
        target.join(tab_gone),
        r#"{"type":"tab_gone","reason":"closed"}"#,
    )
    .unwrap();
    steps.push(serde_json::json!({ "text": tab_gone, "at_ms": steps.len() * 40 }));
    std::fs::write(
        target.join("stream.json"),
        serde_json::to_vec(&serde_json::json!({ "steps": steps })).unwrap(),
    )
    .unwrap();
}

#[test]
fn recording_directory_with_arrival_times_plays_to_the_same_frames_as_the_fixture() {
    let recording = tempfile::tempdir().unwrap();
    write_recording_like_the_black_box(recording.path());

    let mut from_fixture = replay(FIXTURE);
    let fixture_report = play(open(&mut from_fixture.opener), |_| {});
    let mut from_recording = replay_directory(recording.path());
    let recording_report = play(open(&mut from_recording.opener), |_| {});

    assert!(
        recording_report.errors.is_empty(),
        "{:?}",
        recording_report.errors
    );
    assert_eq!(recording_report.final_state, PlaybackState::Ended);
    // Те же десять кадров с теми же временами, что и у исходной фикстуры.
    let times: Vec<Duration> = recording_report
        .frames
        .iter()
        .map(|frame| frame.pts)
        .collect();
    let fixture_times: Vec<Duration> = fixture_report
        .frames
        .iter()
        .map(|frame| frame.pts)
        .collect();
    assert_eq!(times.len(), 10);
    assert_eq!(times, fixture_times);
    assert!(recording_report.audio_duration() >= Duration::from_millis(400));
}

/// Настоящая запись с сайта: доходит ли она до кадров в источнике (печатает итог).
#[test]
#[ignore = "needs a real recording: FASTIPLAYER_REPLAY_RECORDING=<session directory>"]
fn real_recording_plays_to_frames() {
    let directory = std::env::var_os("FASTIPLAYER_REPLAY_RECORDING")
        .expect("set FASTIPLAYER_REPLAY_RECORDING to a recording directory");
    let mut stream = replay_directory(Path::new(&directory));
    // Живая запись обрывается без `endOfStream`: заканчиваем поток сами, иначе плеер
    // честно ждёт продолжения.
    stream.writer.end_of_stream().expect("end of stream");
    let report = play(open(&mut stream.opener), |_| {});
    println!(
        "presented={} errors={:?} state={:?} audio={:?}",
        report.presented_count,
        report.errors,
        report.final_state,
        report.audio_duration()
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.presented_count > 0, "no frame reached the renderer");
}
