//! Подготовка источника для пересборки: вне UI-потока, с отменой и человеческими ошибками.

use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;

/// Ждёт итог фонового потока, опрашивая его так же, как это делает UI на кадрах.
fn wait_for_finish<T: Send + 'static>(job: &mut SettingsMediaPrepareJob<T>) -> Result<T, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match job.poll() {
            PrepareJobPoll::Finished(result) => return result,
            PrepareJobPoll::Running => {
                assert!(Instant::now() < deadline, "background");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

/// Минимальный валидный PCM WAV (моно, 8 кГц, 16 бит, 0,1 с тишины).
fn tiny_pcm_wav() -> Vec<u8> {
    let sample_rate: u32 = 8_000;
    let data_len: u32 = 1_600;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.extend(std::iter::repeat_n(0u8, data_len as usize));
    bytes
}

#[test]
fn preparation_runs_on_a_background_thread_and_ui_poll_does_not_block() {
    let caller_thread = std::thread::current().id();
    let (release_sender, release_receiver) = mpsc::channel::<()>();
    let mut job = SettingsMediaPrepareJob::spawn_task(move |_cancel| {
        // Задача «долго готовится»: пока UI не отпустит её, она стоит.
        release_receiver
            .recv()
            .map_err(|error| format!("release lost: {error}"))?;
        Ok(std::thread::current().id())
    })
    .expect("The flow of preparation should start.");

    // Пока подготовка идёт, опрос возвращается сразу и сообщает «ещё идёт».
    let poll_started = Instant::now();
    assert!(matches!(job.poll(), PrepareJobPoll::Running));
    assert!(
        poll_started.elapsed() < Duration::from_millis(200),
        "The survey should not wait for preparation"
    );

    release_sender.send(()).expect("let go");
    let worker_thread =
        wait_for_finish(&mut job).expect("preparation must be completed with success");
    assert_ne!(
        worker_thread, caller_thread,
        "The preparation should not be on the caller stream (UI)"
    );
}

#[test]
fn preparation_error_text_reaches_the_ui_poll_unchanged() {
    let mut job = SettingsMediaPrepareJob::<u8>::spawn_task(|_cancel| {
        Err("Failed to apply: source unavailable".to_string())
    })
    .expect("The flow of preparation should start.");

    assert_eq!(
        wait_for_finish(&mut job),
        Err("Failed to apply: source unavailable".to_string())
    );
}

#[test]
fn panicking_preparation_becomes_a_human_error_instead_of_silence() {
    let mut job = SettingsMediaPrepareJob::<u8>::spawn_task(|_cancel| {
        panic!("Preparation falls deliberately (test)");
    })
    .expect("The flow of preparation should start.");

    let result = wait_for_finish(&mut job);

    assert_eq!(
        result,
        Err(crate::settings_rebuild_message::install_rebuild_failed_message())
    );
}

#[test]
fn cancel_reaches_the_running_preparation_and_stops_it() {
    let (observed_sender, observed_receiver) = mpsc::channel::<bool>();
    let mut job = SettingsMediaPrepareJob::<u8>::spawn_task(move |cancel| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cancel.is_cancelled() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let _sent = observed_sender.send(cancel.is_cancelled());
        Ok(0)
    })
    .expect("The flow of preparation should start.");

    job.cancel();

    assert_eq!(
        observed_receiver.recv_timeout(Duration::from_secs(5)),
        Ok(true),
        "background task should see the cancellation"
    );
}

#[test]
fn missing_local_file_gives_the_human_rebuild_message() {
    let directory = tempfile::tempdir().expect("time-folder");
    let result = prepare_rebuild_source(
        RebuildPrepareRequest {
            source: RebuildSourceInput::LocalFile {
                path: directory.path().join("no-such-clip.wav"),
                demux: PlayerDemuxConfig::default(),
            },
            preferred_video_codecs: Vec::new(),
        },
        &SourcePreparationCancel::new(),
    );

    assert_eq!(
        result.err(),
        Some(crate::settings_rebuild_message::local_rebuild_failed_message())
    );
}

#[test]
fn real_local_file_is_prepared_in_the_background_job() {
    let mut fixture = tempfile::Builder::new()
        .suffix(".wav")
        .tempfile()
        .expect("temporary");
    fixture.write_all(&tiny_pcm_wav()).expect("WAV");
    let request = RebuildPrepareRequest {
        source: RebuildSourceInput::LocalFile {
            path: fixture.path().to_path_buf(),
            demux: PlayerDemuxConfig::default(),
        },
        preferred_video_codecs: Vec::new(),
    };
    let mut job = SettingsMediaPrepareJob::spawn(request).expect("start-up");

    let prepared = wait_for_finish(&mut job);

    assert!(
        prepared.is_ok(),
        "The present file must be prepared: {:?}",
        prepared.err()
    );
}

#[test]
fn token_of_a_delivered_result_is_not_cancelled_when_the_job_is_dropped() {
    // Токен принадлежит подготовленному источнику (его seek-поток живёт под ним):
    // после выдачи результата сброс job-а не должен его отменять.
    let mut job = SettingsMediaPrepareJob::spawn_task(|cancel| Ok(cancel.clone()))
        .expect("The flow of preparation should start.");
    let source_token = wait_for_finish(&mut job).expect("preparation");

    drop(job);

    assert!(
        !source_token.is_cancelled(),
        "drop of a completed job would break the rewinding of the installed media"
    );
}

#[test]
fn dropping_an_unfinished_job_cancels_its_preparation() {
    let (token_sender, token_receiver) = mpsc::channel::<SourcePreparationCancel>();
    let (release_sender, release_receiver) = mpsc::channel::<()>();
    let job = SettingsMediaPrepareJob::<u8>::spawn_task(move |cancel| {
        let _sent = token_sender.send(cancel.clone());
        let _released = release_receiver.recv_timeout(Duration::from_secs(5));
        Ok(0)
    })
    .expect("The flow of preparation should start.");
    let task_token = token_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("task must begin");

    drop(job);
    let _unblock = release_sender.send(());

    assert!(
        task_token.is_cancelled(),
        "Incomplete preparation should be canceled when resetting the job."
    );
}
