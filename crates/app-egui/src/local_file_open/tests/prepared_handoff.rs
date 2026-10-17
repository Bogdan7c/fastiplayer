//! Передача подготовленного локального media не должна отменять его чтение.
//!
//! Регрессия (живая приёмка AC-3, 2026-10-17): `drain_local_file_open_job` забирает
//! `Prepared` и сразу дропает job; Drop отменял токен источника, который MPEG-TS и сырой
//! AC-3 demuxer хранят весь playback и проверяют на каждом чтении. Локальный `.ts`/`.ac3`
//! останавливался сразу после старта с `MPEG-TS operation cancelled`.
//!
//! Тесты идут сквозным путём: настоящий preparation job → настоящий `PlayerWorker` с
//! production audio decoder-ом → фейковый audio output, который считает дошедший PCM.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use player_core::{
    FrameCounters, PlaybackState, PlayerSnapshot, PlayerWorker, PlayerWorkerConfig, PreparedMedia,
};

use super::super::{LocalFileOpenJob, LocalFileOpenResult};
use super::capturing_audio_output::{CapturedAudio, CapturingAudioOutputFactory};
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::media_open::{PreparationCancelCause, PreparationCancelOutcome};
use crate::process_shutdown::{ProcessOwnerShutdownOutcome, ShutdownDeadline};

/// Предел ожидания подготовки и воспроизведения коротких фикстур.
const SCENARIO_DEADLINE: Duration = Duration::from_secs(20);

/// Сэмплов в одном AC-3 syncframe-е (6 блоков × 256).
const AC3_SAMPLES_PER_FRAME: usize = 1536;

/// Доля PCM фикстуры, которая обязана дойти до output-а (запас на priming декодера).
const MINIMUM_DELIVERED_SHARE_PERCENT: usize = 90;

/// Локальная фикстура и её точный объём звука (по ffprobe: duration × 48 кГц).
struct LocalFixture {
    path: PathBuf,
    expected_pcm_frames: usize,
    /// Держит временный каталог собранной фикстуры до конца теста.
    _scratch_directory: Option<tempfile::TempDir>,
}

/// DVB MPEG-TS, только AC-3 mono 48 кГц: 0.384 с = 12 syncframe-ов.
fn mpeg_ts_ac3_fixture() -> LocalFixture {
    LocalFixture {
        path: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../mpeg-ts-demux/tests/fixtures/ac3/audio-only-ac3-dvb-mono.ts"),
        expected_pcm_frames: 12 * AC3_SAMPLES_PER_FRAME,
        _scratch_directory: None,
    }
}

/// Syncframe-ов в `tones-stereo.ac3` (0.512 с stereo 48 кГц, ffprobe).
const TONES_STEREO_SYNCFRAMES: usize = 16;

/// Сколько раз склеить фикстуру, чтобы файл был больше окна открытия demuxer-а.
///
/// 12 КБ × 32 ≈ 384 КБ > `Ac3ElementaryDemuxOptions::initial_scan` (256 КиБ): иначе весь
/// файл читается на открытии, и чтений во время playback (где виден токен) просто нет.
const RAW_AC3_REPETITIONS: usize = 32;

/// Сырой AC-3 stereo 48 кГц, больше окна открытия: склейка целых syncframe-ов.
fn raw_ac3_fixture() -> LocalFixture {
    let source_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../audio/tests/fixtures/ac3/tones-stereo.ac3");
    let syncframes = std::fs::read(&source_path).expect("read raw AC-3 fixture");
    let scratch_directory = tempfile::tempdir().expect("temp directory");
    let path = scratch_directory.path().join("long-tones-stereo.ac3");
    std::fs::write(&path, syncframes.repeat(RAW_AC3_REPETITIONS)).expect("write long AC-3");
    LocalFixture {
        path,
        expected_pcm_frames: RAW_AC3_REPETITIONS * TONES_STEREO_SYNCFRAMES * AC3_SAMPLES_PER_FRAME,
        _scratch_directory: Some(scratch_directory),
    }
}

/// Что делает владелец с job-ом после того, как забрал `Prepared`.
#[derive(Clone, Copy)]
enum AfterHandoff {
    /// Как `drain_local_file_open_job`: job просто выбрасывается.
    DropJob,
    /// Поздний Stop и process shutdown по уже отдавшему результат job-у, затем Drop.
    StopThenShutdownThenDrop,
}

/// Запускает реальную подготовку и ждёт итог через production `drain()`.
fn prepare_until_completion(path: PathBuf) -> (LocalFileOpenJob, LocalFileOpenResult) {
    let mut job = LocalFileOpenJob::spawn_preparation(
        path,
        fastiplayer_config::PlayerDemuxConfig::default(),
        media_source_open::local_media::AttachmentReadLimits::default(),
        AppWakePort::disconnected(AppWakeOwner::LocalFileOpen),
    )
    .expect("spawn preparation job");
    let deadline = Instant::now() + SCENARIO_DEADLINE;
    loop {
        if let Some(completion) = job.drain().completion {
            return (job, completion);
        }
        assert!(
            Instant::now() < deadline,
            "preparation did not finish in time"
        );
        std::thread::yield_now();
    }
}

/// Готовит фикстуру job-ом и обращается с job-ом так, как велит сценарий.
fn prepare_and_hand_off(fixture: &LocalFixture, after_handoff: AfterHandoff) -> PreparedMedia {
    let (mut job, completion) = prepare_until_completion(fixture.path.clone());
    let LocalFileOpenResult::Prepared { prepared } = completion else {
        panic!(
            "fixture {} must prepare successfully",
            fixture.path.display()
        );
    };
    match after_handoff {
        AfterHandoff::DropJob => {}
        AfterHandoff::StopThenShutdownThenDrop => {
            // Job формально ещё «preparation»: Stop отвечает Cancelled, но играть уже
            // отданному media это не мешает.
            assert_eq!(
                job.cancel_preparation(PreparationCancelCause::UserStop),
                PreparationCancelOutcome::Cancelled
            );
            assert_eq!(
                job.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
                ProcessOwnerShutdownOutcome::Completed
            );
        }
    }
    drop(job);
    prepared.prepared_media
}

/// Играет media в настоящем worker-е до конца и возвращает финальный snapshot и PCM.
fn play_to_terminal_state(prepared_media: PreparedMedia) -> (PlayerSnapshot, CapturedAudio) {
    let output_factory = Arc::new(CapturingAudioOutputFactory::default());
    let worker_config = PlayerWorkerConfig::default()
        .with_audio_decoder_factory(Arc::new(audio::ProductionAudioDecoderFactory::default()))
        .with_audio_output_factory(output_factory.clone())
        .with_audio_tempo_processor_factory(Arc::new(
            audio_signalsmith::SignalsmithTempoProcessorFactory,
        ));
    let mut worker = PlayerWorker::spawn(worker_config).expect("player worker starts");
    worker
        .load_prepared_media(prepared_media, /* autoplay */ true)
        .expect("worker accepts prepared media");

    let deadline = Instant::now() + SCENARIO_DEADLINE;
    let final_snapshot = loop {
        let snapshot = worker.latest_snapshot(FrameCounters::default());
        if matches!(
            snapshot.playback_state,
            PlaybackState::Ended | PlaybackState::Failed
        ) {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "playback did not finish in time: {:?}",
            snapshot.playback_state
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    (final_snapshot, output_factory.captured())
}

/// Общий oracle: файл доиграл до конца без ошибки, и почти весь его звук дошёл до output-а.
fn assert_played_through(fixture: &LocalFixture, after_handoff: AfterHandoff) {
    let prepared_media = prepare_and_hand_off(fixture, after_handoff);
    let (snapshot, captured) = play_to_terminal_state(prepared_media);

    assert_eq!(
        snapshot.playback_state,
        PlaybackState::Ended,
        "playback must reach the end, last error: {:?}",
        snapshot.last_error
    );
    assert!(snapshot.last_error.is_none(), "{:?}", snapshot.last_error);
    let minimum_frames = fixture.expected_pcm_frames * MINIMUM_DELIVERED_SHARE_PERCENT / 100;
    assert!(
        captured.pcm_frames >= minimum_frames,
        "only {} of {} PCM frames reached the output",
        captured.pcm_frames,
        fixture.expected_pcm_frames
    );
    assert!(captured.peak_amplitude > 0.01, "decoded audio is silent");
}

/// Локальный `.ts` с AC-3, открытый кнопкой Open, доигрывает после выбрасывания job-а.
#[test]
fn mpeg_ts_prepared_by_open_job_plays_through_after_job_is_dropped() {
    assert_played_through(&mpeg_ts_ac3_fixture(), AfterHandoff::DropJob);
}

/// Сырой `.ac3`, открытый кнопкой Open, доигрывает после выбрасывания job-а.
#[test]
fn raw_ac3_prepared_by_open_job_plays_through_after_job_is_dropped() {
    assert_played_through(&raw_ac3_fixture(), AfterHandoff::DropJob);
}

/// Поздние Stop и shutdown по уже отдавшему результат job-у не трогают играющий файл.
#[test]
fn late_stop_and_shutdown_of_finished_job_do_not_stop_handed_off_playback() {
    assert_played_through(
        &mpeg_ts_ac3_fixture(),
        AfterHandoff::StopThenShutdownThenDrop,
    );
}

/// Job, чей worker ждёт отмены источника так же, как блокирующее чтение demux-probe-а.
///
/// Возвращает job и приёмник, в который worker сообщает, что увидел отмену токена.
fn job_blocked_on_source_read() -> (LocalFileOpenJob, std::sync::mpsc::Receiver<()>) {
    let (token_sender, token_receiver) =
        std::sync::mpsc::channel::<source_core::CancellationToken>();
    let (observed_sender, observed_receiver) = std::sync::mpsc::channel::<()>();
    let worker = std::thread::spawn(move || {
        let Ok(source_token) = token_receiver.recv() else {
            return;
        };
        let deadline = Instant::now() + SCENARIO_DEADLINE;
        while !source_token.is_cancelled() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        if source_token.is_cancelled() {
            let _ = observed_sender.send(());
        }
    });
    let job = super::test_job(worker);
    token_sender
        .send(job.source_cancellation.clone())
        .expect("worker waits for its source token");
    (job, observed_receiver)
}

/// Stop во время подготовки по-прежнему прерывает чтение источника сразу.
#[test]
fn stop_during_in_flight_preparation_aborts_source_reads_promptly() {
    let (mut job, observed_cancellation) = job_blocked_on_source_read();

    assert_eq!(
        job.cancel_preparation(PreparationCancelCause::UserStop),
        PreparationCancelOutcome::Cancelled
    );

    observed_cancellation
        .recv_timeout(Duration::from_secs(1))
        .expect("in-flight source read must observe Stop promptly");
    // Worker не опубликовал итог, но отменённая владельцем подготовка всегда тихий `Cancelled`.
    assert!(matches!(
        super::drain_completion(&mut job),
        LocalFileOpenResult::Cancelled
    ));
}

/// Fail-safe Drop job-а с ещё идущей подготовкой тоже прерывает чтение источника.
#[test]
fn dropping_in_flight_preparation_aborts_source_reads() {
    let (job, observed_cancellation) = job_blocked_on_source_read();

    drop(job);

    observed_cancellation
        .recv_timeout(Duration::from_secs(1))
        .expect("Drop of an in-flight job must cancel its source reads");
}
