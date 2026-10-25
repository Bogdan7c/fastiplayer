//! Функциональные тесты детектора отставания декодера на настоящем `PlayerSession`.
//!
//! Fake-декодер принимает packets, но кадры отдаёт только тогда, когда тест их кладёт.
//! Проверяем итог: событие `VideoDecoderFallingBehind` появляется в событиях плеера.

use super::*;
use crate::PlayerEvent;

/// Шаг tick-а (около 60 Гц).
pub(super) const TICK_STEP: Duration = Duration::from_millis(16);

/// Интервал кадров fake-видео (25 fps).
pub(super) const FRAME_STEP: Duration = Duration::from_millis(40);

/// Session в Playing с fake-декодером и очередью packets (у декодера всегда есть входная работа).
pub(super) fn playing_session_with_busy_decoder() -> (PlayerSession, RecordingVideoDecoderThread) {
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut session = PlayerSession::new();
    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::H264),
    );
    session.begin_playback_without_media_for_test().unwrap();
    for index in 0..8 {
        enqueue_selected_video_packet(
            &mut session,
            FRAME_STEP * index,
            Bytes::from_static(b"frame"),
            if index == 0 {
                PacketKeyframe::Keyframe
            } else {
                PacketKeyframe::NotKeyframe
            },
        );
    }
    (session, decoder_thread)
}

/// Сколько событий отставания накопилось у session.
fn falling_behind_events(session: &mut PlayerSession) -> usize {
    session
        .take_events()
        .iter()
        .filter(|event| matches!(event, PlayerEvent::VideoDecoderFallingBehind(_)))
        .count()
}

/// Тикает session `duration` фальшивого времени; перед каждым tick-ом `before_tick(индекс, session)`.
fn run_ticks(
    session: &mut PlayerSession,
    start: Instant,
    ticks: u32,
    mut before_tick: impl FnMut(u32, &mut PlayerSession),
) -> usize {
    let mut events = 0;
    for tick_index in 0..ticks {
        before_tick(tick_index, session);
        let now = start + TICK_STEP * tick_index;
        let _ = session.tick(PlayerTickContext::new(now));
        events += falling_behind_events(session);
    }
    events
}

#[test]
fn decoder_that_never_delivers_frames_raises_falling_behind_event() {
    let (mut session, _decoder_thread) = playing_session_with_busy_decoder();

    // 9 с при шаге 16 мс: хватает на 2 с тишины + 4 с окна.
    let events = run_ticks(&mut session, Instant::now(), 560, |_, _| {});

    assert_eq!(events, 1);
}

#[test]
fn decoder_keeping_up_does_not_raise_event() {
    let (mut session, decoder_thread) = playing_session_with_busy_decoder();
    let start = Instant::now();

    // Каждый tick кладём кадр, чей pts = текущее media-время: очередь показа не пустеет.
    let events = run_ticks(&mut session, start, 560, |tick_index, _| {
        let pts = TICK_STEP * tick_index;
        decoder_thread.push_decoded_frame(decoded_frame(pts, u64::from(tick_index) + 1));
    });

    assert_eq!(events, 0);
    // Кадры реально дошли до показа, а не просто не было тиков.
    assert!(session.pipeline.present_video_frame().is_some());
}

#[test]
fn decoder_delivering_frames_slower_than_fps_raises_event() {
    let (mut session, decoder_thread) = playing_session_with_busy_decoder();

    // Кадр только на каждом четвёртом tick-е: очередь показа пуста остальные три из четырёх.
    let events = run_ticks(&mut session, Instant::now(), 560, |tick_index, _| {
        if tick_index % 4 == 0 {
            let pts = TICK_STEP * tick_index;
            decoder_thread.push_decoded_frame(decoded_frame(pts, u64::from(tick_index) + 1));
        }
    });

    assert_eq!(events, 1);
}

#[test]
fn render_generation_change_in_the_middle_suppresses_the_event() {
    let (mut session, decoder_thread) = playing_session_with_busy_decoder();
    let start = Instant::now();

    // 5 с отставания (3 с зачётных из 4 с окна), затем seek-подобный сброс и нормальная игра.
    let before = run_ticks(&mut session, start, 310, |_, _| {});
    session.advance_render_generation();
    let after = run_ticks(
        &mut session,
        start + TICK_STEP * 310,
        400,
        |tick_index, _| {
            let pts = TICK_STEP * (310 + tick_index);
            decoder_thread.push_decoded_frame(decoded_frame(pts, 1_000 + u64::from(tick_index)));
        },
    );

    assert_eq!(before + after, 0);
}

/// Аудио-часы, замершие на заданной позиции (зависшее аудио-устройство).
struct FrozenAudioClock {
    position: Duration,
}

impl audio_core::PlayerAudioClock for FrozenAudioClock {
    fn now(&self) -> Duration {
        self.position
    }

    fn output_timing(&self) -> audio_core::AudioOutputClockTiming {
        audio_core::AudioOutputClockTiming::new(self.position, self.position)
    }

    fn reset(&self) {}

    fn underrun_callbacks(&self) -> u64 {
        0
    }
}

#[test]
fn stalled_audio_clock_with_busy_decoder_and_empty_queue_raises_no_event() {
    let (mut session, _decoder_thread) = playing_session_with_busy_decoder();
    session
        .pipeline
        .install_audio_clock(std::sync::Arc::new(FrozenAudioClock {
            position: Duration::from_secs(1),
        }));

    // 9 с: без исправления остановка звука записалась бы как голодание декодера.
    let events = run_ticks(&mut session, Instant::now(), 560, |_, _| {});

    assert_eq!(events, 0);
}
