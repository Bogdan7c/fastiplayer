//! Мусор вместо контейнера: честный отказ поколения, а не вечное ожидание (09c, инвариант 1).
//!
//! Живая находка стенда (`attack-garbage`): настоящий поток играет, страница кладёт в
//! `SourceBuffer` случайные байты, и плеер молчит, пока вкладка не закроется. Что обязано
//! случиться теперь:
//! - странице ровно один `stream_rejected{unparsable_media}`, экстеншен по нему заканчивает
//!   передачу и возвращает видео странице;
//! - в окне — сообщение об отказе; безоконному показывать некому;
//! - отказ — свойство поколения: сеанс жив, следующее поколение играет.
//!
//! Оконный вариант идёт под частным `kwin_wayland --virtual` и печатает SKIPPED без него.

use super::session_ends::{send_all, six_seconds_downloaded, wait_until_playing_past};
use super::stream_error::{data_frame, lane_frames};
use super::viewer_notices::{FIXTURE_CLIP_US, REJECTION_NOTICE_LINE, fixture, generation_messages};
use super::*;

/// Столько мусора кладёт страница стенда за один раз.
const GARBAGE_BYTES: usize = 64 * 1024;
/// Полоса видео в сообщениях `six_seconds_downloaded`.
const VIDEO_SOURCE_BUFFER: u16 = 1;
/// Полоса звука там же.
const AUDIO_SOURCE_BUFFER: u16 = 0;

/// «Случайные» байты: повторяемо, без настоящего типа бокса в начале.
fn garbage(seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2_654_435_761) | 1;
    (0..GARBAGE_BYTES)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn rejections(messages: &[PlayerSessionMessage]) -> Vec<&PlayerSessionMessage> {
    messages
        .iter()
        .filter(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. }))
        .collect()
}

#[test]
fn garbage_after_a_playing_stream_is_rejected_to_the_page_without_a_window() {
    garbage_after_a_playing_stream_is_rejected_to_the_page(VideoMode::Headless);
}

#[test]
fn garbage_after_a_playing_stream_is_rejected_to_the_page_in_a_window() {
    garbage_after_a_playing_stream_is_rejected_to_the_page(VideoMode::Window);
}

fn garbage_after_a_playing_stream_is_rejected_to_the_page(mode: VideoMode) {
    let Some(process) = SessionProcessUnderTest::start("garbage-after-stream", mode) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();

    // Настоящий поток: кадры доходят до показа, часы идут.
    let mut announcements = six_seconds_downloaded();
    announcements.truncate(4);
    send_all(&mut socket, announcements);
    let video = lane_frames(VIDEO_SOURCE_BUFFER, "video-64-6s.mp4");
    let audio = lane_frames(AUDIO_SOURCE_BUFFER, "audio-6s.mp4");
    let next_video_sequence = video.len() as u64;
    send_all(&mut socket, video);
    send_all(&mut socket, audio);
    wait_until_playing_past(&mut socket, 500_000);

    // Страница кладёт мусор дважды, как `attack-garbage`.
    send_all(
        &mut socket,
        vec![
            data_frame(VIDEO_SOURCE_BUFFER, next_video_sequence, &garbage(1)),
            data_frame(VIDEO_SOURCE_BUFFER, next_video_sequence + 1, &garbage(2)),
        ],
    );

    let messages = read_player_messages_until(&mut socket, "stream_rejected", |message| {
        matches!(message, PlayerSessionMessage::StreamRejected { .. })
    });
    assert_eq!(
        rejections(&messages),
        [&PlayerSessionMessage::StreamRejected {
            generation: StreamGeneration::FIRST,
            reason: StreamRejection::UnparsableMedia,
        }]
    );
    if mode == VideoMode::Window {
        let notice = bus.wait_for_log_line(REJECTION_NOTICE_LINE);
        assert!(notice.contains("reason=unparsable_media"), "{notice}");
    }

    // Отказ — не обрыв: сеанс закроет экстеншен, закончив передачу. Вкладка уходит — 4002.
    send_tab_gone_and_expect_close(&mut socket);
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    assert!(
        log_number(&summary, "presented_frames") > 0,
        "valid segments were shown before the garbage: {summary}"
    );
    bus.wait_for_log_line("Browser session process finished");
}

/// Мусор на самых первых байтах: поток не откроется никогда, и страница узнаёт это сразу;
/// следующее поколение того же сеанса играет до конца.
#[test]
fn garbage_on_the_first_bytes_is_rejected_and_the_next_generation_plays() {
    let Some(process) = SessionProcessUnderTest::start("garbage-first-bytes", VideoMode::Headless)
    else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();

    send_all(
        &mut socket,
        generation_messages(1, FIXTURE_CLIP_US, garbage(3), fixture("audio.mp4"), false),
    );
    let before_rejection = read_player_messages_until(&mut socket, "stream_rejected", |message| {
        matches!(message, PlayerSessionMessage::StreamRejected { .. })
    });
    assert_eq!(
        rejections(&before_rejection),
        [&PlayerSessionMessage::StreamRejected {
            generation: StreamGeneration::FIRST,
            reason: StreamRejection::UnparsableMedia,
        }]
    );

    send_all(
        &mut socket,
        generation_messages(
            2,
            FIXTURE_CLIP_US,
            fixture("video.mp4"),
            fixture("audio.mp4"),
            true,
        ),
    );
    let after_rejection =
        read_player_messages_until(&mut socket, "the clock state ended", |message| {
            matches!(
                message,
                PlayerSessionMessage::Clock {
                    state: ClockState::Ended,
                    ..
                }
            )
        });
    assert!(
        rejections(&after_rejection).is_empty(),
        "the playable generation is not rejected: {after_rejection:?}"
    );
    send_tab_gone_and_expect_close(&mut socket);
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    assert!(log_number(&summary, "presented_frames") > 0, "{summary}");
    bus.wait_for_log_line("Browser session process finished");
}
