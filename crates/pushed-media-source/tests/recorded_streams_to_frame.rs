//! Записанные потоки протокола моста доходят до показа кадра и до звука.
//!
//! Потоки записаны в фикстурах протокола (сессия 01) ровно так, как их шлёт
//! экстеншен: сообщения MSE и двоичные рамки, нарезка посреди боксов. Тест
//! проводит их через того же сторожа поколений, что и сервер 02, в источник, а
//! источник — в настоящую сессию плеера с настоящими декодерами.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::time::Duration;

use player_core::PlaybackState;
use support::playback::{PlaybackReport, open, play};
use support::recorded::replay;

/// Длительность кадра фикстур (25 к/с).
const FRAME: Duration = Duration::from_millis(40);

/// Времена кадров `0, 40, 80 … ` мс.
fn frame_times(count: u32) -> Vec<Duration> {
    (0..count).map(|index| FRAME * index).collect()
}

fn presented_times(report: &PlaybackReport) -> Vec<Duration> {
    report.frames.iter().map(|frame| frame.pts).collect()
}

/// Общие проверки «зритель всё увидел и услышал».
fn assert_played_to_the_end(report: &PlaybackReport, frames: u32, audio_at_least: Duration) {
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    assert_eq!(presented_times(report), frame_times(frames));
    assert!(report.presented_count >= u64::from(frames));
    assert!(
        report.audio_duration() >= audio_at_least,
        "audio played {:?}",
        report.audio_duration()
    );
    // Синус фикстуры (амплитуда ffmpeg по умолчанию 1/8, RMS ≈ 0,088): настоящий
    // звук, а не тишина-заглушка (RMS 0).
    assert!(
        report.audio_rms() > 0.05,
        "audio RMS {}",
        report.audio_rms()
    );
    // Кадр показан не дальше одного кадра от часов звука.
    assert!(
        report.max_av_offset() <= FRAME,
        "A/V offset {:?}",
        report.max_av_offset()
    );
}

#[test]
fn two_source_buffers_with_mid_box_cuts_play_every_frame_with_sound_in_sync() {
    let mut stream = replay("two-source-buffers-mid-box-cuts");
    let report = play(open(&mut stream.opener), |_| {});
    assert_played_to_the_end(&report, 10, Duration::from_millis(400));
    assert!(
        report
            .frames
            .iter()
            .all(|frame| (frame.width, frame.height) == (64, 64))
    );
}

/// Записанный поток не вызывает `endOfStream`: страница «потом» заканчивает поток.
///
/// Без конца потока и без новых байтов многопоточный декодер держит последние
/// кадры в своём конвейере, а плеер честно ждёт продолжения (Buffering) — это
/// поведение любого потока при остановке подачи, а не источника.
fn end_recorded_stream_later(stream: &mut support::recorded::ReplayedStream) {
    stream.writer.end_of_stream().expect("end of stream");
}

#[test]
fn muxed_webm_vp9_opus_with_abort_and_resend_plays_every_frame_once() {
    // Страница прислала начало, перемоталась на 0 (`abort` посреди кластера) и
    // прислала файл заново целиком: повторный такой же init не пересоздаёт
    // эпизод, недособранный кластер выброшен — каждый кадр показан ровно раз.
    let mut stream = replay("muxed-av-single-source-buffer");
    end_recorded_stream_later(&mut stream);
    let report = play(open(&mut stream.opener), |_| {});
    assert_played_to_the_end(&report, 10, Duration::from_millis(380));
}

#[test]
fn stale_generation_tail_never_reaches_the_new_generation() {
    // Поколение 1 успело прислать 3000 байтов видео и потом «хвост» после старта
    // поколения 2. Сторож выбрасывает хвост; играет только поколение 2 — целый
    // ролик без мусора в начале или середине.
    let mut stream = replay("generation-switch-drops-stale");
    end_recorded_stream_later(&mut stream);
    let report = play(open(&mut stream.opener), |_| {});
    assert_played_to_the_end(&report, 10, Duration::from_millis(400));
}
