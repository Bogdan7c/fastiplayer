//! Регрессия «вечный Buffering из-за decode-ahead окна»: пока audio clock не запущен,
//! окно опережения (500 мс) не должно держать video packets, если decoder ещё не выдал
//! первый кадр.
//!
//! Сценарий — настоящий MKV (`low-fps-vp8-opus.mkv`: VP8 10 fps + Opus, 8 с — обычный,
//! не короткий старт), настоящий демуксер и аудио-декодер, дефолтный tick config. Fake
//! video decoder моделирует frame threading FFmpeg: первый кадр только после
//! `FIRST_FRAME_AFTER_PACKETS` принятых packets. При 10 fps в окно 500 мс помещается
//! всего ~6 packets, поэтому без исправления кадр не появляется никогда.

use super::startup_read_ahead::StartupHarness;
use super::*;

/// Глубина frame threading (packets до первого кадра): заведомо больше окна 500 мс при 10 fps.
const FIRST_FRAME_AFTER_PACKETS: usize = 14;

/// Бюджет tick-ов сценария: конечный, чтобы зависание проваливало тест, а не вешало его.
const TICK_BUDGET: usize = 600;

/// Низкий fps + глубокий frame threading: старт доходит до Playing, звук идёт, кадр показан.
#[test]
fn low_fps_startup_with_deep_decoder_pipeline_reaches_playing_with_audio_and_video() {
    let mut harness =
        StartupHarness::with_fixture("low-fps-vp8-opus.mkv", FIRST_FRAME_AFTER_PACKETS);
    assert_eq!(harness.session.playback_state(), PlaybackState::Buffering);

    for _ in 0..TICK_BUDGET {
        harness.tick();
        if harness.session.playback_state() == PlaybackState::Playing
            && harness.frames_presented > 0
        {
            break;
        }
    }

    assert_eq!(
        harness.session.playback_state(),
        PlaybackState::Playing,
        "sent video packets: {}",
        harness.video_decoder.sent_packets().len()
    );
    assert!(
        harness.video_decoder.sent_packets().len() >= FIRST_FRAME_AFTER_PACKETS,
        "decoder must be fed past the decode-ahead window to produce the first frame"
    );
    assert!(
        harness.frames_presented > 0,
        "first video frame must be presented"
    );
    let output = harness.output().expect("audio output created");
    assert!(output.play_count() > 0, "audio stream must be started");
    assert!(
        !output.written_samples().is_empty(),
        "decoded audio must reach the output"
    );
}
