//! Политика audio read-ahead: временный стартовый потолок, пока видео ждёт первый кадр.
//!
//! Проверяем решения, которые наблюдает demux admission: блокирует ли уровень audio buffer-а
//! чтение, какой предел действует и какая причина backpressure сообщается.

use media_core::TrackId;

use super::super::audio_read_ahead::{
    AudioReadAheadAllowance, audio_read_ahead_allowance, sanitize_startup_read_ahead_cap_ms,
};
use super::*;

/// Высота стартового потолка в тестах: явная, чтобы не зависеть от дефолтов config-а.
const STARTUP_CAP_MS: f64 = 1_500.0;

/// Config с известными high-water (200 мс) и стартовым потолком.
fn read_ahead_config() -> PlayerTickConfig {
    PlayerTickConfig {
        audio_buffer_high_water_mark_ms: 200.0,
        audio_startup_read_ahead_cap_ms: STARTUP_CAP_MS,
        ..PlayerTickConfig::default()
    }
}

/// Session с аудио и видео треками, fake output-ом заданного уровня и без единого кадра.
fn session_without_first_frame(audio_level_ms: f64) -> PlayerSession {
    let mut session = PlayerSession::new();
    install_empty_demuxer(&mut session);
    session.pipeline.select_audio_track(TrackId::new(2));
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    install_fixed_audio_output(&mut session, audio_level_ms);
    session
}

/// Тот же session, но в autoplay preroll-е (`Buffering`).
fn buffering_session_without_first_frame(audio_level_ms: f64) -> PlayerSession {
    let mut session = session_without_first_frame(audio_level_ms);
    session
        .begin_autoplay_preroll()
        .expect("autoplay preroll starts");
    session
}

/// До первого кадра чтение идёт дальше обычных 200 мс и demux не блокируется.
#[test]
fn starved_preroll_relaxes_audio_gate_beyond_normal_high_water() {
    let session = buffering_session_without_first_frame(600.0);
    let config = read_ahead_config();

    assert_eq!(
        audio_read_ahead_allowance(&session, &config),
        AudioReadAheadAllowance::VideoPrerollStarved {
            cap_ms: STARTUP_CAP_MS
        }
    );
    assert!(!audio_read_ahead_blocks_demux(&session, &config));
    assert_eq!(demux_backpressure_reason(&session, &config, false), None);
}

/// На потолке demux снова блокируется с причиной `DemuxBackpressure`.
#[test]
fn starved_preroll_blocks_demux_again_above_startup_cap() {
    let config = read_ahead_config();

    let at_cap = buffering_session_without_first_frame(STARTUP_CAP_MS);
    assert!(!audio_read_ahead_blocks_demux(&at_cap, &config));

    let above_cap = buffering_session_without_first_frame(STARTUP_CAP_MS + 1.0);
    assert!(audio_read_ahead_blocks_demux(&above_cap, &config));
    assert!(!can_read_next_demux_packet_with_audio_priority(
        &above_cap, &config, false
    ));
    assert_eq!(
        demux_backpressure_reason(&above_cap, &config, false),
        Some(PipelinePauseReason::DemuxBackpressure)
    );
}

/// Когда первый кадр уже есть, снова действует обычный high-water.
#[test]
fn first_video_frame_restores_normal_high_water() {
    let mut session = buffering_session_without_first_frame(600.0);
    let config = read_ahead_config();
    assert!(!audio_read_ahead_blocks_demux(&session, &config));

    session
        .pipeline
        .set_present_video_frame(decoded_frame(Duration::ZERO, 1));

    assert_eq!(
        audio_read_ahead_allowance(&session, &config),
        AudioReadAheadAllowance::Normal {
            high_water_mark_ms: 200.0
        }
    );
    assert!(audio_read_ahead_blocks_demux(&session, &config));
}

/// Buffering-resume (не стартовый autoplay) без кадров тоже считается голодным preroll-ом.
#[test]
fn buffering_resume_without_frames_is_also_relaxed() {
    let mut session = session_without_first_frame(600.0);
    session.set_playback_state(PlaybackState::Buffering);
    let config = read_ahead_config();

    assert!(session.video_preroll_is_starved());
    assert!(!audio_read_ahead_blocks_demux(&session, &config));
}

/// Вне `Buffering` (Paused/Playing/Seeking) предел обычный, даже если кадров нет.
#[test]
fn outside_buffering_the_normal_high_water_applies() {
    let config = read_ahead_config();

    for state in [
        PlaybackState::Paused,
        PlaybackState::Playing,
        PlaybackState::Seeking,
    ] {
        let mut session = session_without_first_frame(600.0);
        session.set_playback_state(state);

        assert!(!session.video_preroll_is_starved(), "{state:?}");
        assert!(
            audio_read_ahead_blocks_demux(&session, &config),
            "600 ms must still block in {state:?}"
        );
    }
}

/// Без видеотрека (audio-only) поведение не меняется: потолок не включается.
#[test]
fn audio_only_media_keeps_normal_high_water_while_buffering() {
    let mut session = PlayerSession::new();
    install_empty_demuxer(&mut session);
    session.pipeline.select_audio_track(TrackId::new(2));
    install_fixed_audio_output(&mut session, 600.0);
    session
        .begin_autoplay_preroll()
        .expect("autoplay preroll starts");
    let config = read_ahead_config();

    assert!(!session.video_preroll_is_starved());
    assert!(audio_read_ahead_blocks_demux(&session, &config));
}

/// Без аудиотрека гейт вообще не применяется (как и раньше), потолок ничего не меняет.
#[test]
fn media_without_audio_track_is_never_blocked_by_audio_gate() {
    let mut session = PlayerSession::new();
    install_empty_demuxer(&mut session);
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    install_fixed_audio_output(&mut session, 5_000.0);
    session
        .begin_autoplay_preroll()
        .expect("autoplay preroll starts");

    assert!(!audio_read_ahead_blocks_demux(
        &session,
        &read_ahead_config()
    ));
}

/// Потолок всегда в безопасных рамках: не ниже high-water и не выше ёмкости output-а.
#[test]
fn startup_cap_is_clamped_to_safe_bounds() {
    let max_ms = fastiplayer_config::MAX_AUDIO_STARTUP_READ_AHEAD_CAP_MS as f64;

    assert_eq!(sanitize_startup_read_ahead_cap_ms(1_000.0, 200.0), 1_000.0);
    assert_eq!(
        sanitize_startup_read_ahead_cap_ms(1.0e9, 200.0),
        max_ms,
        "oversized cap must be limited by the audio output capacity"
    );
    assert_eq!(
        sanitize_startup_read_ahead_cap_ms(50.0, 200.0),
        200.0,
        "cap below high-water must not narrow the normal gate"
    );
    for invalid in [f64::NAN, f64::INFINITY, 0.0, -5.0] {
        assert_eq!(
            sanitize_startup_read_ahead_cap_ms(invalid, 200.0),
            200.0,
            "invalid cap {invalid} falls back to the normal high-water"
        );
    }

    // Тот же клэмп виден через публичное решение policy.
    let session = buffering_session_without_first_frame(0.0);
    let oversized = PlayerTickConfig {
        audio_startup_read_ahead_cap_ms: 1.0e9,
        ..read_ahead_config()
    };
    assert_eq!(
        audio_read_ahead_allowance(&session, &oversized),
        AudioReadAheadAllowance::VideoPrerollStarved { cap_ms: max_ms }
    );
}
