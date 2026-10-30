//! Звук начала отрезка, ждущий своего PTS, не останавливает чтение видео.
//!
//! Без output-а demux admission считает pending audio packet «нужным для создания
//! output-а» и не читает дальше, пока его не обработают. Удержанный packet обработают
//! только в его PTS, а до него идёт видео: блок по нему остановил бы всё чтение.

use bytes::Bytes;
use codec_core::{VideoCodec, VideoDecodeRequirement};
use media_core::TrackId;

use super::*;

/// ID выбранной звуковой дорожки.
const AUDIO_TRACK: TrackId = TrackId::new(2);

/// ID выбранной видеодорожки.
const VIDEO_TRACK: TrackId = TrackId::new(1);

/// A/V session без output-а звука, с одним pending audio packet-ом в `audio_start`.
fn av_session_with_pending_audio(audio_start: Duration) -> PlayerSession {
    let mut session = PlayerSession::new();
    session
        .pipeline
        .select_video_track(VIDEO_TRACK, VideoDecodeRequirement::new(VideoCodec::H264));
    session.pipeline.select_audio_track(AUDIO_TRACK);
    let generation = session.pipeline.seek_generation();
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
            AUDIO_TRACK,
            audio_start,
            None,
            None,
            generation,
            Bytes::from_static(b"audio"),
        ));
    session
}

#[test]
fn audio_held_until_its_pts_does_not_block_demux_without_output() {
    let session = av_session_with_pending_audio(Duration::from_secs(2));

    assert!(!audio_read_ahead_blocks_demux(
        &session,
        &PlayerTickConfig::default()
    ));
}

#[test]
fn audio_due_now_still_blocks_demux_until_it_creates_the_output() {
    let session = av_session_with_pending_audio(Duration::ZERO);

    assert!(audio_read_ahead_blocks_demux(
        &session,
        &PlayerTickConfig::default()
    ));
}

/// Пределы запаса для тестов: 1 с media или 1000 байт сжатого звука.
fn small_backlog_config() -> PlayerTickConfig {
    PlayerTickConfig {
        startup_video_wait_max_audio_backlog: Duration::from_secs(1),
        startup_video_wait_max_audio_backlog_bytes: 1_000,
        ..PlayerTickConfig::default()
    }
}

/// Докладывает удержанные audio packets по 0,25 с до `until` (сжатые, по 5 байт).
fn enqueue_more_held_audio(session: &mut PlayerSession, until: Duration) {
    let generation = session.pipeline.seek_generation();
    let mut packet_pts = Duration::from_millis(2_250);
    while packet_pts < until {
        session
            .pipeline
            .enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
                AUDIO_TRACK,
                packet_pts,
                None,
                None,
                generation,
                Bytes::from_static(b"audio"),
            ));
        packet_pts += Duration::from_millis(250);
    }
}

#[test]
fn held_audio_backlog_beyond_the_limit_stops_demux_until_the_audio_joins() {
    // Файл, где весь звук лежит раньше видео: удержанный звук не декодируется и output
    // его не ограничивает — предел ставит запас сжатого звука (как у голодного старта).
    let mut within_limit = av_session_with_pending_audio(Duration::from_secs(2));
    enqueue_more_held_audio(&mut within_limit, Duration::from_millis(2_600));
    let mut beyond_limit = av_session_with_pending_audio(Duration::from_secs(2));
    enqueue_more_held_audio(&mut beyond_limit, Duration::from_secs(4));

    assert!(!audio_read_ahead_blocks_demux(
        &within_limit,
        &small_backlog_config()
    ));
    assert!(
        audio_read_ahead_blocks_demux(&beyond_limit, &small_backlog_config()),
        "2 s of held audio exceed the 1 s backlog"
    );
}
