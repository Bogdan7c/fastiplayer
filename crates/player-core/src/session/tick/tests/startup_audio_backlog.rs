//! Запас отложенного сжатого звука, пока preroll ждёт первый кадр (сессия 03e моста).
//!
//! Проверяем решения, которые видят demux admission и tick: читает ли demux сверх
//! стартового потолка output-а, когда запас считается исчерпанным, и что защёлка
//! «звук первым» открывает только видео-ворота preroll-а и ничего больше.

use media_core::TrackId;

use super::super::audio_read_ahead::{StartupAudioBacklogRoom, startup_audio_backlog_room};
use super::*;
use crate::pipeline::PendingAudioBacklog;
use crate::session::video_preroll_wait::AudioFirstStartDecision;

/// Стартовый потолок output-а в тестах.
const STARTUP_CAP_MS: f64 = 1_500.0;
/// Уровень output-а выше потолка: декод звука стоит, сжатый звук копится в очереди.
const ABOVE_CAP_MS: f64 = STARTUP_CAP_MS + 10.0;
/// Предел запаса по media-времени в тестах.
const BACKLOG_SPAN_LIMIT: Duration = Duration::from_secs(1);
/// Предел запаса по байтам в тестах.
const BACKLOG_BYTES_LIMIT: usize = 1_000;
/// ID выбранной звуковой дорожки.
const AUDIO_TRACK: TrackId = TrackId::new(2);

/// Config с явными потолком и пределами запаса.
fn backlog_config() -> PlayerTickConfig {
    PlayerTickConfig {
        audio_buffer_high_water_mark_ms: 200.0,
        audio_startup_read_ahead_cap_ms: STARTUP_CAP_MS,
        startup_video_wait_max_audio_backlog: BACKLOG_SPAN_LIMIT,
        startup_video_wait_max_audio_backlog_bytes: BACKLOG_BYTES_LIMIT,
        ..PlayerTickConfig::default()
    }
}

/// A/V session без единого кадра с output-ом заданного уровня.
fn session_without_first_frame(audio_level_ms: f64) -> PlayerSession {
    let mut session = PlayerSession::new();
    install_empty_demuxer(&mut session);
    session.pipeline.select_audio_track(AUDIO_TRACK);
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    install_fixed_audio_output(&mut session, audio_level_ms);
    session
}

/// Та же session в стартовом preroll-е (`Buffering`, кадр ещё не пришёл).
fn starved_preroll_session(audio_level_ms: f64) -> PlayerSession {
    let mut session = session_without_first_frame(audio_level_ms);
    session
        .begin_autoplay_preroll()
        .expect("autoplay preroll starts");
    session
}

/// Кладёт в очередь сжатый audio packet с заданными PTS и размером.
fn enqueue_audio_at(session: &mut PlayerSession, pts: Duration, payload_bytes: usize) {
    let generation = session.pipeline.seek_generation();
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
            AUDIO_TRACK,
            pts,
            None,
            None,
            generation,
            Bytes::from(vec![0_u8; payload_bytes]),
        ));
}

/// Голодный preroll с output-ом выше потолка всё равно читает: запас ещё есть.
#[test]
fn starved_preroll_reads_past_startup_cap_while_backlog_has_room() {
    let mut session = starved_preroll_session(ABOVE_CAP_MS);
    enqueue_audio_at(&mut session, Duration::ZERO, 100);
    enqueue_audio_at(&mut session, Duration::from_millis(500), 100);
    let config = backlog_config();

    assert_eq!(
        startup_audio_backlog_room(&session, &config),
        StartupAudioBacklogRoom::Available
    );
    assert!(!audio_read_ahead_blocks_demux(&session, &config));
    assert!(can_read_next_demux_packet_with_audio_priority(
        &session, &config, false
    ));
    assert_eq!(demux_backpressure_reason(&session, &config, false), None);
}

/// Запас по media-времени исчерпан: demux снова ждёт, причина — `DemuxBackpressure`.
#[test]
fn starved_preroll_blocks_demux_when_backlog_span_is_exhausted() {
    let mut session = starved_preroll_session(ABOVE_CAP_MS);
    enqueue_audio_at(&mut session, Duration::from_secs(2), 100);
    enqueue_audio_at(&mut session, Duration::from_secs(3), 120);
    let config = backlog_config();

    assert_eq!(
        startup_audio_backlog_room(&session, &config),
        StartupAudioBacklogRoom::Exhausted(PendingAudioBacklog {
            packets: 2,
            encoded_bytes: 220,
            media_span: Duration::from_secs(1),
        })
    );
    assert!(audio_read_ahead_blocks_demux(&session, &config));
    assert!(!can_read_next_demux_packet_with_audio_priority(
        &session, &config, false
    ));
    assert_eq!(
        demux_backpressure_reason(&session, &config, false),
        Some(PipelinePauseReason::DemuxBackpressure)
    );
}

/// Битые PTS (все нули) не дают запасу расти бесконечно: держит предел по байтам.
#[test]
fn byte_limit_exhausts_backlog_even_with_broken_timestamps() {
    let mut session = starved_preroll_session(ABOVE_CAP_MS);
    for _ in 0..4 {
        enqueue_audio_at(&mut session, Duration::ZERO, BACKLOG_BYTES_LIMIT / 4);
    }
    let config = backlog_config();

    let StartupAudioBacklogRoom::Exhausted(audio_backlog) =
        startup_audio_backlog_room(&session, &config)
    else {
        panic!("byte limit must exhaust the backlog");
    };
    assert_eq!(audio_backlog.media_span, Duration::ZERO);
    assert_eq!(audio_backlog.encoded_bytes, BACKLOG_BYTES_LIMIT);
    assert!(audio_read_ahead_blocks_demux(&session, &config));
}

/// Пустая очередь запас не исчерпывает даже при нулевых пределах: сжатый звук копится
/// только когда output дошёл до потолка, а до этого ворота закрывать не за что.
#[test]
fn empty_backlog_never_counts_as_exhausted() {
    let session = starved_preroll_session(ABOVE_CAP_MS);
    let zero_limits = PlayerTickConfig {
        startup_video_wait_max_audio_backlog: Duration::ZERO,
        startup_video_wait_max_audio_backlog_bytes: 0,
        ..backlog_config()
    };

    assert_eq!(
        startup_audio_backlog_room(&session, &zero_limits),
        StartupAudioBacklogRoom::Available
    );
}

/// Вне голодного preroll-а запас не действует: обычный high-water блокирует чтение.
#[test]
fn backlog_room_does_not_apply_outside_starved_preroll() {
    let config = backlog_config();

    for state in [
        PlaybackState::Playing,
        PlaybackState::Paused,
        PlaybackState::Seeking,
    ] {
        let mut session = session_without_first_frame(ABOVE_CAP_MS);
        session.set_playback_state(state);
        assert_eq!(
            startup_audio_backlog_room(&session, &config),
            StartupAudioBacklogRoom::NotWaitingForVideo,
            "{state:?}"
        );
        assert!(
            audio_read_ahead_blocks_demux(&session, &config),
            "{state:?}"
        );
    }

    // Кадр уже есть: preroll не голоден, действует обычный high-water.
    let mut with_frame = starved_preroll_session(ABOVE_CAP_MS);
    with_frame
        .pipeline
        .set_present_video_frame(decoded_frame(Duration::ZERO, 1));
    assert_eq!(
        startup_audio_backlog_room(&with_frame, &config),
        StartupAudioBacklogRoom::NotWaitingForVideo
    );
    assert!(audio_read_ahead_blocks_demux(&with_frame, &config));
}

/// Защёлка открывает видео-ворота preroll-а и не трогает очереди, output и state.
#[test]
fn audio_first_latch_opens_only_the_video_gate() {
    let mut session = starved_preroll_session(ABOVE_CAP_MS);
    enqueue_audio_at(&mut session, Duration::ZERO, 100);
    let audio_backlog = session.pipeline.pending_audio_backlog();
    assert!(session.video_preroll_is_starved());
    assert!(!session.autoplay_video_preroll_ready());

    assert_eq!(
        session.start_audio_first_after_audio_backlog_exhausted(audio_backlog),
        AudioFirstStartDecision::Latched
    );

    assert!(session.autoplay_video_preroll_ready());
    assert!(!session.video_preroll_is_starved());
    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    assert_eq!(session.pipeline.pending_audio_backlog(), audio_backlog);
    assert_eq!(session.audio_buffer_level_ms(), Some(ABOVE_CAP_MS));
    assert_eq!(
        session.start_audio_first_after_audio_backlog_exhausted(audio_backlog),
        AudioFirstStartDecision::AlreadyLatched,
        "second request must not log or change anything"
    );
}

/// Без ожидания кадра (нет видео, кадр уже есть, не `Buffering`) запрос — no-op.
#[test]
fn audio_first_latch_is_noop_when_preroll_does_not_wait_for_video() {
    let audio_backlog = PendingAudioBacklog::default();

    let mut audio_only = PlayerSession::new();
    install_empty_demuxer(&mut audio_only);
    audio_only.pipeline.select_audio_track(AUDIO_TRACK);
    audio_only
        .begin_autoplay_preroll()
        .expect("autoplay preroll starts");
    assert_eq!(
        audio_only.start_audio_first_after_audio_backlog_exhausted(audio_backlog),
        AudioFirstStartDecision::NotWaitingForVideo
    );

    let mut with_frame = starved_preroll_session(ABOVE_CAP_MS);
    with_frame
        .pipeline
        .set_present_video_frame(decoded_frame(Duration::ZERO, 1));
    assert_eq!(
        with_frame.start_audio_first_after_audio_backlog_exhausted(audio_backlog),
        AudioFirstStartDecision::NotWaitingForVideo
    );

    let mut paused = session_without_first_frame(ABOVE_CAP_MS);
    paused.set_playback_state(PlaybackState::Paused);
    assert_eq!(
        paused.start_audio_first_after_audio_backlog_exhausted(audio_backlog),
        AudioFirstStartDecision::NotWaitingForVideo
    );
    assert!(!paused.video_preroll_starts_audio_first());
}

/// Защёлка живёт только в текущем preroll-е: `Draining` её сохраняет, любой другой
/// переход и новый preroll возвращают обычное ожидание кадра.
#[test]
fn audio_first_latch_belongs_to_the_current_preroll_only() {
    let latched_session = || {
        let mut session = starved_preroll_session(ABOVE_CAP_MS);
        session.start_audio_first_after_audio_backlog_exhausted(PendingAudioBacklog::default());
        assert!(session.video_preroll_starts_audio_first());
        session
    };

    let mut draining = latched_session();
    draining.set_playback_state(PlaybackState::Draining);
    assert!(
        draining.video_preroll_starts_audio_first(),
        "EOF before the start keeps the decision for the EOF-drain start"
    );

    for state in [
        PlaybackState::Playing,
        PlaybackState::Paused,
        PlaybackState::Seeking,
        PlaybackState::Stopped,
    ] {
        let mut session = latched_session();
        session.set_playback_state(state);
        assert!(!session.video_preroll_starts_audio_first(), "{state:?}");
    }

    let mut restarted = latched_session();
    restarted
        .begin_autoplay_preroll()
        .expect("autoplay preroll restarts");
    assert!(!restarted.video_preroll_starts_audio_first());
    assert!(restarted.video_preroll_is_starved());
}

/// После защёлки, но пока звук ещё не набрал preroll target, чтение не блокируется:
/// уровень ниже обычного high-water, demux и декод добирают звук до старта (ревью 03e).
#[test]
fn audio_first_latch_with_unready_audio_keeps_reading() {
    let mut session = starved_preroll_session(20.0);
    enqueue_audio_at(&mut session, Duration::ZERO, 100);
    let audio_backlog = session.pipeline.pending_audio_backlog();
    assert_eq!(
        session.start_audio_first_after_audio_backlog_exhausted(audio_backlog),
        AudioFirstStartDecision::Latched
    );
    let config = backlog_config();

    assert_eq!(
        startup_audio_backlog_room(&session, &config),
        StartupAudioBacklogRoom::NotWaitingForVideo
    );
    assert!(!audio_read_ahead_blocks_demux(&session, &config));
    assert!(can_read_next_demux_packet_with_audio_priority(
        &session, &config, false
    ));
    assert!(
        !session
            .finish_autoplay_preroll_if_ready(50.0)
            .expect("preroll check"),
        "20 ms of audio is below the preroll target: the start waits for more audio"
    );
    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    assert!(session.video_preroll_starts_audio_first());
}
