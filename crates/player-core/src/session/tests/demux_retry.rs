use super::test_support::*;
use super::*;
use crate::MediaInstanceId;
use crate::session::demux_retry::DemuxAudioStarvationMargin;

/// Минимальный retry hint сохраняет production anti-busy-spin contract.
fn retry_hint() -> media_core::DemuxRetryHint {
    media_core::DemuxRetryHint::new(media_core::DemuxRetryHint::MIN_RETRY_AFTER)
        .expect("минимальный public retry hint обязан быть допустим")
}

/// Создаёт валидный hint для точных runway/deadline сценариев.
fn retry_hint_for(retry_after: Duration) -> media_core::DemuxRetryHint {
    media_core::DemuxRetryHint::new(retry_after)
        .expect("focused retry duration должна укладываться в public bounds")
}

/// Использует существующий low-water config как именованный scheduler/callback запас.
fn starvation_margin(milliseconds: f64) -> DemuxAudioStarvationMargin {
    DemuxAudioStarvationMargin::from_low_water_mark_ms(milliseconds)
}

/// Ждёт только нормативный minimum, чтобы следующий tick мог читать source снова.
fn wait_for_retry_deadline() {
    std::thread::sleep(media_core::DemuxRetryHint::MIN_RETRY_AFTER + Duration::from_millis(1));
}

/// Строит production-like tick config с одним наблюдаемым demux event за tick.
fn retry_tick_config() -> PlayerTickConfig {
    PlayerTickConfig {
        max_demux_packets_per_tick: 1,
        adaptive_catch_up_time_budget: Duration::ZERO,
        audio_preroll_target_ms: 50.0,
        ..PlayerTickConfig::default()
    }
}

/// Выполняет production tick и явно подтверждает, что telemetry этого теста не нужна.
fn tick_session(session: &mut PlayerSession, tick_config: PlayerTickConfig) {
    let _tick_result = session.tick(PlayerTickContext::with_config(Instant::now(), tick_config));
}

/// Выполняет production tick с явно заданным caller timestamp для clock regression tests.
fn tick_session_at(
    session: &mut PlayerSession,
    tick_config: PlayerTickConfig,
    tick_started_at: Instant,
) {
    let _tick_result = session.tick(PlayerTickContext::with_config(tick_started_at, tick_config));
}

/// Устанавливает scripted source и выбирает tracks через обычные session boundaries.
fn install_scripted_retry_media(
    session: &mut PlayerSession,
    tracks: Vec<TrackInfo>,
    events: impl IntoIterator<Item = DemuxReadEvent>,
) {
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let mut demuxer = FakeDemuxer::new(tracks.clone(), Some(Duration::from_secs(30)), seek_log);
    for event in events {
        demuxer.push_event(event);
    }

    session
        .pipeline
        .install_opened_media(Box::new(demuxer), None, None, tracks.clone());
    if tracks.iter().any(|track| track.kind == TrackKind::Video) {
        session
            .select_default_video_track(&tracks, "retry fixture должна содержать video")
            .expect("VP9 retry fixture должна пройти production selection");
    }
    if let Some(audio_track_id) = tracks
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .map(|track| track.id)
    {
        session.pipeline.select_audio_track(audio_track_id);
    }
    session.snapshot.media_instance_id = Some(MediaInstanceId::new_unique());
    session.set_snapshot_duration(Some(Duration::from_secs(30)));
}

/// Создаёт A/V session с выбранным audio, активным output и будущим queued video frame.
fn playing_av_retry_session(
    events: impl IntoIterator<Item = DemuxReadEvent>,
    play_error: Option<&'static str>,
    pause_error: Option<&'static str>,
) -> (PlayerSession, ScriptedAudioOutputHandle) {
    let mut session = PlayerSession::new();
    let audio_track = fake_track(1, TrackKind::Audio);
    let video_track = fake_track(2, TrackKind::Video);
    install_scripted_retry_media(&mut session, vec![audio_track, video_track], events);

    let reset_count = Arc::new(AtomicUsize::new(0));
    session
        .pipeline
        .install_audio_decoder(counting_audio_decoder_handle(reset_count));
    let scripted_output = match pause_error {
        Some(error) => ScriptedAudioOutput::with_pause_error(0.0, error),
        None => ScriptedAudioOutput::new(0.0, play_error),
    };
    let (audio_output, output_handle) = scripted_output.into_parts();
    session
        .pipeline
        .install_audio_output_for_tests(audio_output);
    session.pipeline.install_audio_clock(output_handle.clock());
    output_handle.set_underlying_output_timing(Duration::from_secs(2), Duration::from_secs(2));

    let queued_frame =
        decoded_frame_for_current_seek_generation(&session, Duration::from_secs(10), 91);
    session.pipeline.enqueue_queued_video_frame(queued_frame);
    session
        .dispatch_command(PlayerCommand::Play)
        .expect("fixture должна войти в Playing через production command");

    (session, output_handle)
}

/// A/V starvation замораживает audio master независимо от готового video runway,
/// а source recovery проходит общий 50 ms A/V preroll gate до единственного resume.
#[test]
fn audio_starvation_buffers_and_resumes_only_after_full_av_preroll() {
    let audio_track_id = TrackId::new(1);
    let video_track_id = TrackId::new(2);
    let recovered_video = fake_video_packet(video_track_id, Duration::from_secs(3));
    let (mut session, output_handle) = playing_av_retry_session(
        [
            DemuxReadEvent::TemporarilyUnavailable(retry_hint()),
            DemuxReadEvent::Packet(recovered_video),
        ],
        None,
        None,
    );
    let tick_config = retry_tick_config();
    let initial_play_count = output_handle.play_count();

    tick_session(&mut session, tick_config);

    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    assert_eq!(output_handle.pause_count(), 1);
    assert_eq!(output_handle.play_count(), initial_play_count);
    assert_eq!(session.pipeline.video_present_queue_len(), 1);
    assert_eq!(
        output_handle.audible_output_position(),
        Duration::from_secs(2)
    );

    output_handle.set_underlying_output_timing(Duration::from_secs(7), Duration::from_secs(7));
    assert_eq!(
        output_handle.audible_output_position(),
        Duration::from_secs(2),
        "pause/freeze boundary не должен пропускать wall-time jump"
    );

    wait_for_retry_deadline();
    tick_session(&mut session, tick_config);
    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    assert_eq!(output_handle.pause_count(), 1);
    assert_eq!(output_handle.play_count(), initial_play_count);
    assert_eq!(session.pipeline.video_present_queue_len(), 1);

    output_handle.set_buffer_level_ms(49.0);
    assert!(
        !session
            .finish_autoplay_preroll_if_ready(50.0)
            .expect("49 ms audio не должны пройти preroll")
    );
    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    assert_eq!(output_handle.play_count(), initial_play_count);

    output_handle.set_buffer_level_ms(50.0);
    assert!(
        session
            .finish_autoplay_preroll_if_ready(50.0)
            .expect("recovered source и полный A/V preroll должны resume")
    );
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(output_handle.pause_count(), 1);
    assert_eq!(output_handle.play_count(), initial_play_count + 1);

    let current_generation = session.pipeline.seek_generation();
    session.process_audio_packet(
        audio_track_id,
        Duration::from_millis(3_020),
        None,
        Some(Duration::from_millis(20)),
        current_generation,
        b"follow-up encoded audio",
    );
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(output_handle.pause_count(), 1);
    assert_eq!(
        output_handle.play_count(),
        initial_play_count + 1,
        "follow-up packets не должны повторять resume"
    );
}

/// Video-only playback сохраняет прежний full-drain gate и не замораживается при queued frame.
#[test]
fn video_only_retry_keeps_playing_while_present_queue_has_runway() {
    let mut session = PlayerSession::new();
    let video_track = fake_track(2, TrackKind::Video);
    install_scripted_retry_media(
        &mut session,
        vec![video_track],
        [DemuxReadEvent::TemporarilyUnavailable(retry_hint())],
    );
    let queued_frame =
        decoded_frame_for_current_seek_generation(&session, Duration::from_secs(10), 92);
    session.pipeline.enqueue_queued_video_frame(queued_frame);
    session
        .dispatch_command(PlayerCommand::Play)
        .expect("video-only fixture должна войти в Playing");

    tick_session(&mut session, retry_tick_config());

    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(session.pipeline.video_present_queue_len(), 1);
}

/// Малый ненулевой runway сравнивается с remaining retry wait до фактической тишины.
#[test]
fn selected_audio_runway_freezes_before_retry_deadline_can_exhaust_it() {
    let (mut session, output_handle) = playing_av_retry_session([], None, None);
    output_handle.set_buffer_level_ms(5.0);
    let observed_at = Instant::now();
    session.schedule_installed_demux_retry(observed_at, retry_hint_for(Duration::from_millis(10)));

    assert!(
        session
            .enter_buffering_for_demux_underrun_if_needed(observed_at, starvation_margin(4.0),)
            .expect("5 ms runway должен заморозиться до 10 ms retry")
    );

    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    assert_eq!(output_handle.pause_count(), 1);
    assert_eq!(session.pipeline.video_present_queue_len(), 1);
}

/// Runway, покрывающий retry wait и margin, не вызывает преждевременную паузу.
#[test]
fn sufficient_selected_audio_runway_keeps_playing_until_retry() {
    let (mut session, output_handle) = playing_av_retry_session([], None, None);
    output_handle.set_buffer_level_ms(15.0);
    let observed_at = Instant::now();
    session.schedule_installed_demux_retry(observed_at, retry_hint_for(Duration::from_millis(10)));

    assert!(
        !session
            .enter_buffering_for_demux_underrun_if_needed(observed_at, starvation_margin(4.0),)
            .expect("15 ms runway должен покрыть 10 ms retry и 4 ms margin")
    );

    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(output_handle.pause_count(), 0);
    assert_eq!(session.pipeline.video_present_queue_len(), 1);
}

/// Public крайние retry hints участвуют в том же saturating duration calculation.
#[test]
fn selected_audio_runway_budget_handles_public_retry_hint_bounds() {
    for retry_after in [
        media_core::DemuxRetryHint::MIN_RETRY_AFTER,
        media_core::DemuxRetryHint::MAX_RETRY_AFTER,
    ] {
        let (mut session, output_handle) = playing_av_retry_session([], None, None);
        let exact_budget_ms = retry_after.as_secs_f64() * 1_000.0 + 4.0;
        output_handle.set_buffer_level_ms(exact_budget_ms);
        let observed_at = Instant::now();
        session.schedule_installed_demux_retry(observed_at, retry_hint_for(retry_after));

        assert!(
            session
                .enter_buffering_for_demux_underrun_if_needed(observed_at, starvation_margin(4.0),)
                .expect("exact runway budget должен безопасно войти в Buffering"),
            "retry hint bound {retry_after:?} должен участвовать без truncation"
        );
        assert_eq!(output_handle.pause_count(), 1);
    }
}

/// Runway decision не использует устаревший caller timestamp до созданного deadline-а.
#[test]
fn fresh_runway_decision_time_does_not_overestimate_remaining_retry_wait() {
    let retry_after = Duration::from_millis(10);
    let (mut session, output_handle) = playing_av_retry_session(
        [DemuxReadEvent::TemporarilyUnavailable(retry_hint_for(
            retry_after,
        ))],
        None,
        None,
    );
    output_handle.set_buffer_level_ms(50.0);
    let tick_config = PlayerTickConfig {
        max_demux_packets_per_tick: 1,
        audio_demux_low_water_mark_ms: 1.0,
        ..retry_tick_config()
    };
    let stale_tick_started_at = Instant::now()
        .checked_sub(Duration::from_millis(100))
        .expect("focused old tick timestamp должен помещаться в Instant");

    tick_session_at(&mut session, tick_config, stale_tick_started_at);

    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(output_handle.pause_count(), 0);
    assert!(
        session
            .installed_demux_retry_delay(Instant::now())
            .is_some()
    );
}

/// Starvation margin использует тот же invalid-config fallback, что demux admission.
#[test]
fn invalid_audio_low_water_marks_use_canonical_default_margin() {
    for invalid_low_water_mark_ms in [-1.0, f64::NAN, f64::INFINITY] {
        let (mut session, output_handle) = playing_av_retry_session([], None, None);
        output_handle.set_buffer_level_ms(50.0);
        let observed_at = Instant::now();
        session.schedule_installed_demux_retry(observed_at, retry_hint());

        assert!(
            session
                .enter_buffering_for_demux_underrun_if_needed(
                    observed_at,
                    starvation_margin(invalid_low_water_mark_ms),
                )
                .expect("canonical 100 ms fallback должен заморозить 50 ms runway"),
            "invalid low-water mark {invalid_low_water_mark_ms:?} обязан использовать default"
        );
        assert_eq!(output_handle.pause_count(), 1);
    }
}

/// Chained temporary readiness сохраняет demux-owned Buffering до accepted packet-а.
#[test]
fn chained_demux_retry_blocks_ready_preroll_until_next_accepted_packet() {
    let audio_track_id = TrackId::new(1);
    let video_track_id = TrackId::new(2);
    let chained_retry_after = Duration::from_secs(1);
    let (mut session, output_handle) = playing_av_retry_session(
        [
            DemuxReadEvent::TemporarilyUnavailable(retry_hint()),
            DemuxReadEvent::Packet(fake_video_packet(video_track_id, Duration::from_secs(3))),
            DemuxReadEvent::TemporarilyUnavailable(retry_hint_for(chained_retry_after)),
            DemuxReadEvent::Packet(fake_audio_packet(
                audio_track_id,
                Duration::from_secs(3),
                Duration::from_millis(20),
            )),
        ],
        None,
        None,
    );
    let tick_config = PlayerTickConfig {
        max_demux_packets_per_tick: 2,
        ..retry_tick_config()
    };
    let initial_play_count = output_handle.play_count();

    tick_session(&mut session, tick_config);
    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    output_handle.set_buffer_level_ms(50.0);
    wait_for_retry_deadline();
    let _initial_events = session.take_events();

    tick_session(&mut session, tick_config);

    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    assert_eq!(output_handle.pause_count(), 1);
    assert_eq!(output_handle.play_count(), initial_play_count);
    assert_eq!(session.pipeline.video_present_queue_len(), 1);
    assert!(session.installed_demux_read_is_blocked(Instant::now()));
    assert!(session.take_events().iter().all(|event| {
        !matches!(
            event,
            PlayerEvent::AudioPlaybackResumed
                | PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)
        )
    }));

    assert!(
        !session
            .finish_autoplay_preroll_if_ready(50.0)
            .expect("pending chained retry должен блокировать ready preroll")
    );
    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    assert_eq!(output_handle.play_count(), initial_play_count);
    assert!(session.take_events().is_empty());

    std::thread::sleep(chained_retry_after + Duration::from_millis(1));
    let resume_tick_config = PlayerTickConfig {
        max_demux_packets_per_tick: 1,
        ..tick_config
    };
    tick_session(&mut session, resume_tick_config);

    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(output_handle.pause_count(), 1);
    assert_eq!(output_handle.play_count(), initial_play_count + 1);
    let resume_events = session.take_events();
    assert_eq!(
        resume_events
            .iter()
            .filter(|event| matches!(event, PlayerEvent::AudioPlaybackResumed))
            .count(),
        1
    );
    assert_eq!(
        resume_events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)
                )
            })
            .count(),
        1
    );
}

/// Paused user intent не превращается в autoplay при recovery packet-е.
#[test]
fn paused_intent_is_not_overridden_by_demux_recovery() {
    let (mut session, output_handle) = playing_av_retry_session([], None, None);
    session
        .dispatch_command(PlayerCommand::Pause)
        .expect("fixture pause должна завершиться успешно");
    let pause_count = output_handle.pause_count();
    session.schedule_installed_demux_retry(Instant::now(), retry_hint());

    assert!(
        !session
            .enter_buffering_for_demux_underrun_if_needed(Instant::now(), starvation_margin(4.0),)
            .expect("paused intent не должен вызывать backend error")
    );
    session.complete_installed_demux_retry_after_event();

    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert_eq!(output_handle.pause_count(), pause_count);
    assert_eq!(output_handle.play_count(), 1);
}

/// Selected audio без lazy-created output входит в Buffering как typed absent-output no-op.
#[test]
fn selected_audio_without_output_enters_buffering_without_false_device_error() {
    let mut session = PlayerSession::new();
    let audio_track = fake_track(1, TrackKind::Audio);
    let video_track = fake_track(2, TrackKind::Video);
    install_scripted_retry_media(
        &mut session,
        vec![audio_track, video_track],
        [DemuxReadEvent::TemporarilyUnavailable(retry_hint())],
    );
    let queued_frame =
        decoded_frame_for_current_seek_generation(&session, Duration::from_secs(10), 93);
    session.pipeline.enqueue_queued_video_frame(queued_frame);
    session
        .dispatch_command(PlayerCommand::Play)
        .expect("absent-output fixture должна войти в Playing");

    tick_session(&mut session, retry_tick_config());

    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    assert!(session.snapshot().last_error.is_none());
    assert_eq!(session.pipeline.video_present_queue_len(), 1);
}

/// Pause error не публикует ложный Buffering и доходит до typed fatal runtime state.
#[test]
fn demux_buffering_pause_error_is_typed_and_has_no_partial_transition() {
    let (mut session, output_handle) = playing_av_retry_session(
        [DemuxReadEvent::TemporarilyUnavailable(retry_hint())],
        None,
        Some("scripted demux buffering pause failed"),
    );

    tick_session(&mut session, retry_tick_config());

    assert_eq!(session.playback_state(), PlaybackState::Failed);
    assert_eq!(output_handle.pause_count(), 1);
    let error = session
        .snapshot()
        .last_error
        .as_ref()
        .expect("pause failure должен быть опубликован");
    assert_eq!(error.kind, PlayerErrorKind::RuntimeError);
    assert!(
        error
            .message
            .contains("scripted demux buffering pause failed")
    );
}

/// Resume play error оставляет atomic boundary в Buffering и допускает следующий retry.
#[test]
fn demux_buffering_resume_play_error_remains_typed() {
    let (mut session, output_handle) = playing_av_retry_session(
        [DemuxReadEvent::TemporarilyUnavailable(retry_hint())],
        None,
        None,
    );
    tick_session(&mut session, retry_tick_config());
    output_handle.set_buffer_level_ms(50.0);
    session.complete_installed_demux_retry_after_event();
    output_handle.set_play_error(Some("scripted demux buffering resume failed"));
    session.snapshot.last_error = None;
    let _pre_resume_events = session.take_events();

    assert!(
        !session
            .finish_autoplay_preroll_if_ready(50.0)
            .expect("recoverable play error не должен стать fatal")
    );

    let error = session
        .snapshot()
        .last_error
        .as_ref()
        .expect("play failure должен быть опубликован");
    assert_eq!(error.kind, PlayerErrorKind::RuntimeError);
    assert!(
        error
            .message
            .contains("scripted demux buffering resume failed")
    );
    assert_eq!(output_handle.play_count(), 2);
    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    output_handle.set_underlying_output_timing(Duration::from_secs(9), Duration::from_secs(9));
    assert_eq!(
        output_handle.audible_output_position(),
        Duration::from_secs(2),
        "failed resume не должен размораживать clock"
    );
    let failed_events = session.take_events();
    assert!(failed_events.iter().all(|event| {
        !matches!(
            event,
            PlayerEvent::AudioPlaybackResumed
                | PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)
        )
    }));

    assert!(
        !session
            .finish_autoplay_preroll_if_ready(50.0)
            .expect("следующий обычный preroll attempt остаётся recoverable")
    );
    assert!(
        session.take_events().is_empty(),
        "идентичная persistent ошибка не должна спамить RecoverableError"
    );

    output_handle.set_play_error(None);
    assert!(
        session
            .finish_autoplay_preroll_if_ready(50.0)
            .expect("следующий readiness attempt должен повторить play")
    );
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(output_handle.play_count(), 4);
    let resumed_events = session.take_events();
    assert_eq!(
        resumed_events
            .iter()
            .filter(|event| matches!(event, PlayerEvent::AudioPlaybackResumed))
            .count(),
        1
    );
    assert_eq!(
        resumed_events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)
                )
            })
            .count(),
        1
    );
}

/// Stale seek generation не имеет права заморозить новый pipeline.
#[test]
fn stale_retry_generation_cannot_enter_buffering() {
    let (mut session, output_handle) = playing_av_retry_session([], None, None);
    session.schedule_installed_demux_retry(Instant::now(), retry_hint());
    session.pipeline.begin_seek_generation();

    assert!(
        !session
            .enter_buffering_for_demux_underrun_if_needed(Instant::now(), starvation_margin(4.0),)
            .expect("stale fence должен быть side-effect-free")
    );
    session.complete_installed_demux_retry_after_event();

    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(output_handle.pause_count(), 0);
}

/// EOF после temporary readiness остаётся terminal drain и не считается recovery resume.
#[test]
fn eof_after_retry_does_not_resume_buffered_audio() {
    let (mut session, output_handle) = playing_av_retry_session(
        [
            DemuxReadEvent::TemporarilyUnavailable(retry_hint()),
            DemuxReadEvent::EndOfStream,
        ],
        None,
        None,
    );
    let tick_config = retry_tick_config();
    tick_session(&mut session, tick_config);
    wait_for_retry_deadline();

    tick_session(&mut session, tick_config);

    assert!(session.is_eof_draining());
    assert_ne!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(output_handle.pause_count(), 1);
    assert_eq!(output_handle.play_count(), 1);
}

/// Playback-window rejected packet не доказывает source recovery и не снимает retry fence.
#[test]
fn playback_window_rejected_packet_does_not_recover_buffering() {
    let rejected_audio = fake_audio_packet(
        TrackId::new(1),
        Duration::from_secs(1),
        Duration::from_millis(20),
    );
    let (mut session, output_handle) = playing_av_retry_session(
        [
            DemuxReadEvent::TemporarilyUnavailable(retry_hint()),
            DemuxReadEvent::Packet(rejected_audio),
            DemuxReadEvent::TemporarilyUnavailable(retry_hint()),
        ],
        None,
        None,
    );
    session.playback_window = Some(
        MediaPlaybackWindow::new(MediaTime::from_secs(5), Some(MediaTime::from_secs(20)))
            .expect("focused playback window должна быть валидна"),
    );
    let tick_config = retry_tick_config();
    tick_session(&mut session, tick_config);
    wait_for_retry_deadline();

    tick_session(&mut session, tick_config);

    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    assert_eq!(output_handle.play_count(), 1);
    assert!(
        session
            .installed_demux_retry_delay(Instant::now())
            .is_some()
    );
}

/// Logical pause не должна сама увеличивать callback-silence counter.
#[test]
fn buffering_pause_keeps_underrun_counter_stable() {
    let (mut session, output_handle) = playing_av_retry_session(
        [DemuxReadEvent::TemporarilyUnavailable(retry_hint())],
        None,
        None,
    );
    output_handle.set_underrun_callbacks(4);

    tick_session(&mut session, retry_tick_config());
    session.diagnose_audio_output_starvation(Instant::now());

    assert_eq!(session.playback_state(), PlaybackState::Buffering);
    assert_eq!(output_handle.underrun_callbacks(), 4);
    assert_eq!(output_handle.pause_count(), 1);
}
