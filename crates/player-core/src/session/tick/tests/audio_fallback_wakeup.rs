//! Потолок резервного coarse wakeup-а при играющем звуке (продолжение бэклога AC-3 #19).
//!
//! Planner попадает в `CoarseProgress`, когда не нашёл ни работы, ни точного deadline-а.
//! Здесь это воспроизводится нефинитным уровнем буфера: ни «работа сейчас», ни refill
//! deadline, ни demux его не принимают — остаётся только fallback.

use super::super::audio_fallback_wakeup::coarse_progress_interval_for_audio_runway;
use super::*;

/// Production-интервал резервного wakeup-а worker-а.
const CONFIGURED_COARSE_INTERVAL: Duration = Duration::from_millis(250);

/// Poll interval decoder readiness (без видео не участвует).
const DECODER_READINESS_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Уровень, при котором planner не находит ни работы, ни deadline-а.
const LEVEL_WITHOUT_DEADLINE_MS: f64 = f64::NAN;

/// Config с заданным целевым audio buffer-ом (high-water).
fn config_with_buffer_target(buffer_target_ms: f64) -> PlayerTickConfig {
    PlayerTickConfig {
        audio_buffer_high_water_mark_ms: buffer_target_ms,
        ..PlayerTickConfig::default()
    }
}

/// Строит plan с production-like coarse interval-ом.
fn plan_wakeup(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    coarse_interval: Duration,
) -> PlayerWorkerWakeupPlan {
    session.worker_wakeup_plan(
        Instant::now(),
        tick_config,
        DECODER_READINESS_POLL_INTERVAL,
        coarse_interval,
    )
}

/// Audio-only session с выбранной дорожкой и output-ом заданного уровня.
fn audio_session_with_output(buffer_level_ms: f64) -> PlayerSession {
    let mut session = PlayerSession::new();
    install_empty_demuxer(&mut session);
    session.pipeline.select_audio_track(TrackId::new(2));
    install_fixed_audio_output(&mut session, buffer_level_ms);
    session
}

/// Playing audio-only session, где planner вынужден взять fallback.
fn playing_session_without_deadline() -> PlayerSession {
    let mut session = audio_session_with_output(LEVEL_WITHOUT_DEADLINE_MS);
    session
        .dispatch_command(PlayerCommand::Play)
        .expect("Play is accepted for installed media");
    session
}

/// Играющий звук: fallback = половина настроенного буфера (200 мс → 100 мс), не 250 мс.
#[test]
fn playing_audio_caps_coarse_fallback_to_half_of_buffer_target() {
    let session = playing_session_without_deadline();

    let plan = plan_wakeup(
        &session,
        &config_with_buffer_target(200.0),
        CONFIGURED_COARSE_INTERVAL,
    );

    assert_eq!(plan.reason, WorkerWakeupReason::CoarseProgress);
    assert_eq!(plan.delay, Some(Duration::from_millis(100)));
}

/// Потолок выводится из config-а, а не захардкожен: буфер 120 мс → 60 мс.
#[test]
fn coarse_fallback_cap_follows_configured_buffer_target() {
    let session = playing_session_without_deadline();

    let plan = plan_wakeup(
        &session,
        &config_with_buffer_target(120.0),
        CONFIGURED_COARSE_INTERVAL,
    );

    assert_eq!(plan.reason, WorkerWakeupReason::CoarseProgress);
    assert_eq!(plan.delay, Some(Duration::from_millis(60)));
}

/// Настроенный interval короче потолка остаётся как есть: cap только укорачивает.
#[test]
fn shorter_configured_coarse_interval_is_not_extended() {
    let session = playing_session_without_deadline();
    let short_interval = Duration::from_millis(40);

    let plan = plan_wakeup(&session, &config_with_buffer_target(200.0), short_interval);

    assert_eq!(plan.delay, Some(short_interval));
}

/// Buffering: output заморожен, буфер не тратится — прежние 250 мс.
#[test]
fn buffering_with_frozen_output_keeps_configured_coarse_interval() {
    let mut session = audio_session_with_output(LEVEL_WITHOUT_DEADLINE_MS);
    session
        .begin_autoplay_preroll()
        .expect("autoplay preroll starts");

    let plan = plan_wakeup(
        &session,
        &config_with_buffer_target(200.0),
        CONFIGURED_COARSE_INTERVAL,
    );

    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    assert_eq!(plan.reason, WorkerWakeupReason::CoarseProgress);
    assert_eq!(plan.delay, Some(CONFIGURED_COARSE_INTERVAL));
}

/// Пауза: planner по-прежнему ждёт только событий, cap не создаёт wakeup-ов.
#[test]
fn paused_audio_stays_idle() {
    let mut session = playing_session_without_deadline();
    session
        .dispatch_command(PlayerCommand::Pause)
        .expect("Pause is accepted while playing");

    let plan = plan_wakeup(
        &session,
        &config_with_buffer_target(200.0),
        CONFIGURED_COARSE_INTERVAL,
    );

    assert_eq!(plan.reason, WorkerWakeupReason::Idle);
    assert_eq!(plan.delay, None);
}

/// Нет output-а или нет выбранного звука: интервал не меняется.
#[test]
fn absent_audio_output_or_track_keeps_configured_interval() {
    let tick_config = config_with_buffer_target(200.0);

    let mut selected_without_output = PlayerSession::new();
    install_empty_demuxer(&mut selected_without_output);
    selected_without_output
        .dispatch_command(PlayerCommand::Play)
        .expect("Play is accepted for installed media");
    selected_without_output
        .pipeline
        .select_audio_track(TrackId::new(2));
    assert_eq!(
        coarse_progress_interval_for_audio_runway(
            &selected_without_output,
            &tick_config,
            CONFIGURED_COARSE_INTERVAL,
        ),
        CONFIGURED_COARSE_INTERVAL
    );

    let mut video_only = PlayerSession::new();
    install_empty_demuxer(&mut video_only);
    video_only
        .dispatch_command(PlayerCommand::Play)
        .expect("Play is accepted for installed media");
    assert_eq!(
        coarse_progress_interval_for_audio_runway(
            &video_only,
            &tick_config,
            CONFIGURED_COARSE_INTERVAL,
        ),
        CONFIGURED_COARSE_INTERVAL
    );
}

/// Другие виды plan-ов не трогаются: refill deadline 250 мс остаётся 250 мс.
#[test]
fn audio_refill_deadline_is_not_capped() {
    let mut session = audio_session_with_output(450.0);
    session
        .dispatch_command(PlayerCommand::Play)
        .expect("Play is accepted for installed media");

    let plan = plan_wakeup(
        &session,
        &config_with_buffer_target(200.0),
        CONFIGURED_COARSE_INTERVAL,
    );

    assert_eq!(plan.reason, WorkerWakeupReason::PipelineWorkReady);
    assert_eq!(plan.delay, Some(Duration::from_millis(250)));
}
