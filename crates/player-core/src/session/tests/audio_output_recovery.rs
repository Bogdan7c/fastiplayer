//! Пропажа устройства вывода: переключение на default, сохранение позиции, защита от шторма.
//!
//! Сценарии из `user/ux-edge-cases/10-audio-device-loss.md`. Звук проверяется по факту:
//! decoded PCM должен реально дойти до нового fake output-а, а не только «вызвался create».

use audio_core::{AudioOutputDeviceRequest, AudioPacketTiming};
use bytes::Bytes;
use media_core::PacketPresentationWindow;

use super::audio_packet_window::RecordingPcmDecoder;
use super::test_support::{
    SCRIPTED_SELECTED_DEVICE_NAME, ScriptedAudioDevices, ScriptedAudioOutputFactory,
    ScriptedAudioOutputFactoryHandle, ScriptedAudioOutputHandle,
};
use super::*;
use crate::AudioOutputSwitchReason;
use crate::session::audio_output_recovery::AudioOutputRecoveryOutcome;

/// Audio track тестового media.
const AUDIO_TRACK: u32 = 2;

/// PCM, который fake decoder возвращает на каждый packet (stereo, 2 кадра).
const DECODED_PCM: [f32; 4] = [0.25, -0.5, 0.75, -1.0];

/// Позиция, на которой «выдёргивают» устройство.
const POSITION_AT_UNPLUG: Duration = Duration::from_secs(42);

/// Session с fake decoder-ом и fake устройствами; звук ещё не создан.
fn session_with_audio_track() -> (PlayerSession, ScriptedAudioOutputFactoryHandle) {
    let (factory, factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session = PlayerSession::with_audio_output_factory(factory);
    let track_id = TrackId::new(AUDIO_TRACK);
    session.pipeline.select_audio_track(track_id);
    session.snapshot.selected_tracks.audio_track = Some(track_id);
    session
        .pipeline
        .install_audio_decoder(Box::new(RecordingPcmDecoder::new(
            Arc::new(Mutex::new(Vec::new())),
            DECODED_PCM.to_vec(),
            48_000,
            2,
        )));
    (session, factory_handle)
}

/// Прогоняет один encoded packet через pending queue → decoder → output.
///
/// Packet всегда немного впереди позиции поломки: в реальном потоке после восстановления
/// decoder отдаёт будущие packet-ы, а packet до привязки clock-а session законно отбрасывает.
fn feed_one_audio_packet(session: &mut PlayerSession) {
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::with_timing(
            TrackId::new(AUDIO_TRACK),
            POSITION_AT_UNPLUG + Duration::from_millis(10),
            AudioPacketTiming::unknown(),
            PacketPresentationWindow::Unbounded,
            session.pipeline.seek_generation(),
            Bytes::from_static(b"encoded-audio"),
        ));
    session.process_pending_audio_packets_with_buffer_limit(200.0);
}

/// Играющая session, у которой звук уже идёт в выбранное устройство.
fn playing_session_with_active_output() -> (
    PlayerSession,
    ScriptedAudioOutputFactoryHandle,
    ScriptedAudioOutputHandle,
) {
    let (mut session, factory_handle) = session_with_audio_track();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    feed_one_audio_packet(&mut session);
    let first_output = factory_handle
        .last_output_handle()
        .expect("первый packet должен создать output на выбранном устройстве");
    session.current_source_position = POSITION_AT_UNPLUG;
    let _ = session.take_events();
    (session, factory_handle, first_output)
}

/// Ни одного подключённого устройства.
const NO_DEVICES: ScriptedAudioDevices = ScriptedAudioDevices {
    selected_connected: false,
    system_default_connected: false,
};

/// Выбранное устройство отключено, default работает.
const ONLY_SYSTEM_DEFAULT: ScriptedAudioDevices = ScriptedAudioDevices {
    selected_connected: false,
    system_default_connected: true,
};

fn switch_events(events: &[PlayerEvent]) -> Vec<AudioOutputSwitchReason> {
    events
        .iter()
        .filter_map(|event| match event {
            PlayerEvent::AudioOutputSwitchedToSystemDefault(reason) => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

fn audio_device_errors(events: &[PlayerEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                PlayerEvent::RecoverableError(error)
                    if error.kind == PlayerErrorKind::AudioDeviceUnavailable
            )
        })
        .count()
}

#[test]
fn unplugged_device_switches_sound_to_default_and_new_output_receives_pcm() {
    let (mut session, factory_handle, first_output) = playing_session_with_active_output();
    factory_handle.set_devices(ONLY_SYSTEM_DEFAULT);

    first_output.fail_stream();
    let outcome = session.recover_failed_audio_output_if_needed(Instant::now());
    feed_one_audio_packet(&mut session);

    assert_eq!(outcome, AudioOutputRecoveryOutcome::SwitchedToSystemDefault);
    assert_eq!(
        factory_handle.device_requests(),
        vec![
            AudioOutputDeviceRequest::SelectedOrSystemDefault,
            AudioOutputDeviceRequest::SystemDefault,
        ]
    );
    let replacement_output = factory_handle
        .last_output_handle()
        .expect("восстановление должно создать новый output");
    // Звук снова идёт: PCM после переключения дошёл до нового output-а, а не до сломанного.
    assert_eq!(replacement_output.written_samples(), DECODED_PCM.to_vec());
    assert_eq!(first_output.written_samples(), DECODED_PCM.to_vec());
    assert_eq!(replacement_output.play_count.load(Ordering::Relaxed), 1);
    assert_eq!(
        switch_events(&session.take_events()),
        vec![AudioOutputSwitchReason::ActiveDeviceStopped]
    );
}

#[test]
fn switch_to_default_keeps_playback_position() {
    let (mut session, factory_handle, first_output) = playing_session_with_active_output();
    factory_handle.set_devices(ONLY_SYSTEM_DEFAULT);
    let timeline_position_before = session.snapshot.timeline.current_position;

    first_output.fail_stream();
    session.recover_failed_audio_output_if_needed(Instant::now());

    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(session.current_source_position, POSITION_AT_UNPLUG);
    assert_eq!(
        session.snapshot.timeline.current_position,
        timeline_position_before
    );
    // Новый clock привязан к той же позиции: видео не прыгает назад/вперёд.
    assert_eq!(
        session.presentation_clock_position_at(Instant::now()),
        POSITION_AT_UNPLUG
    );
}

#[test]
fn stream_failure_is_detected_by_regular_tick() {
    let (mut session, factory_handle, first_output) = playing_session_with_active_output();
    factory_handle.set_devices(ONLY_SYSTEM_DEFAULT);

    first_output.fail_stream();
    let _ = session.tick(PlayerTickContext::new(Instant::now()));

    assert_eq!(
        factory_handle.device_requests().last(),
        Some(&AudioOutputDeviceRequest::SystemDefault)
    );
    assert_eq!(
        switch_events(&session.take_events()),
        vec![AudioOutputSwitchReason::ActiveDeviceStopped]
    );
}

#[test]
fn healthy_stream_is_never_recreated() {
    let (mut session, factory_handle, _first_output) = playing_session_with_active_output();

    for _ in 0..10 {
        let outcome = session.recover_failed_audio_output_if_needed(Instant::now());
        assert_eq!(outcome, AudioOutputRecoveryOutcome::StreamRunning);
    }

    assert_eq!(factory_handle.create_count(), 1);
    assert!(switch_events(&session.take_events()).is_empty());
}

#[test]
fn no_output_is_reported_as_absent_not_as_failure() {
    let (mut session, factory_handle) = session_with_audio_track();

    let outcome = session.recover_failed_audio_output_if_needed(Instant::now());

    assert_eq!(outcome, AudioOutputRecoveryOutcome::NoActiveOutput);
    assert_eq!(factory_handle.create_count(), 0);
    assert!(session.snapshot.last_error.is_none());
}

#[test]
fn paused_session_recovers_output_without_starting_it() {
    let (mut session, factory_handle) = session_with_audio_track();
    feed_one_audio_packet(&mut session);
    let first_output = factory_handle.last_output_handle().expect("output создан");
    factory_handle.set_devices(ONLY_SYSTEM_DEFAULT);
    let state_before = session.playback_state();

    first_output.fail_stream();
    let outcome = session.recover_failed_audio_output_if_needed(Instant::now());

    let replacement_output = factory_handle.last_output_handle().expect("новый output");
    assert_eq!(outcome, AudioOutputRecoveryOutcome::SwitchedToSystemDefault);
    assert_ne!(state_before, PlaybackState::Playing);
    assert_eq!(session.playback_state(), state_before);
    assert_eq!(replacement_output.play_count.load(Ordering::Relaxed), 0);
}

#[test]
fn missing_saved_device_at_open_plays_on_default_and_keeps_audio_track() {
    let (mut session, factory_handle) = session_with_audio_track();
    factory_handle.set_devices(ONLY_SYSTEM_DEFAULT);
    session.dispatch_command(PlayerCommand::Play).unwrap();

    feed_one_audio_packet(&mut session);

    let output = factory_handle
        .last_output_handle()
        .expect("звук должен пойти в default");
    assert_eq!(output.written_samples(), DECODED_PCM.to_vec());
    assert_eq!(
        session.snapshot.selected_tracks.audio_track,
        Some(TrackId::new(AUDIO_TRACK))
    );
    assert!(session.snapshot.last_error.is_none());
    assert_eq!(
        switch_events(&session.take_events()),
        vec![AudioOutputSwitchReason::SelectedDeviceUnavailable {
            device_name: SCRIPTED_SELECTED_DEVICE_NAME.to_string(),
        }]
    );
}

#[test]
fn no_device_at_all_at_open_keeps_video_playing_with_audio_error() {
    let (mut session, factory_handle) = session_with_audio_track();
    factory_handle.set_devices(NO_DEVICES);
    session.dispatch_command(PlayerCommand::Play).unwrap();

    feed_one_audio_packet(&mut session);

    let events = session.take_events();
    assert!(!session.pipeline.has_audio_output());
    assert_eq!(session.snapshot.selected_tracks.audio_track, None);
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(audio_device_errors(&events), 1);
    assert!(switch_events(&events).is_empty());
}

#[test]
fn failing_default_is_retried_at_most_three_times_then_video_continues_without_audio() {
    let (mut session, factory_handle, first_output) = playing_session_with_active_output();
    factory_handle.set_devices(NO_DEVICES);
    let unplugged_at = Instant::now();

    first_output.fail_stream();
    // Первая попытка сразу, дальше — частые tick-и внутри интервала: новых попыток быть не должно.
    let first = session.recover_failed_audio_output_if_needed(unplugged_at);
    for tick_offset_ms in (50..1_000).step_by(50) {
        let outcome = session.recover_failed_audio_output_if_needed(
            unplugged_at + Duration::from_millis(tick_offset_ms),
        );
        assert_eq!(outcome, AudioOutputRecoveryOutcome::RetryPending);
    }
    let second =
        session.recover_failed_audio_output_if_needed(unplugged_at + Duration::from_secs(1));
    let third =
        session.recover_failed_audio_output_if_needed(unplugged_at + Duration::from_secs(2));
    let after_give_up =
        session.recover_failed_audio_output_if_needed(unplugged_at + Duration::from_secs(60));

    assert_eq!(first, AudioOutputRecoveryOutcome::RetryPending);
    assert_eq!(second, AudioOutputRecoveryOutcome::RetryPending);
    assert_eq!(third, AudioOutputRecoveryOutcome::AudioDisabled);
    assert_eq!(after_give_up, AudioOutputRecoveryOutcome::NoActiveOutput);
    // 1 исходный output + ровно 3 попытки восстановления.
    assert_eq!(factory_handle.create_count(), 4);
    let events = session.take_events();
    assert_eq!(audio_device_errors(&events), 1);
    assert!(switch_events(&events).is_empty());
    assert_eq!(session.snapshot.selected_tracks.audio_track, None);
    assert_eq!(session.playback_state(), PlaybackState::Playing);
}

#[test]
fn video_clock_keeps_running_after_audio_is_disabled() {
    let (mut session, factory_handle, first_output) = playing_session_with_active_output();
    factory_handle.set_devices(NO_DEVICES);
    let unplugged_at = Instant::now();
    first_output.fail_stream();
    for attempt in 0..3 {
        session.recover_failed_audio_output_if_needed(unplugged_at + Duration::from_secs(attempt));
    }
    let disabled_at = unplugged_at + Duration::from_secs(2);

    let later_position =
        session.presentation_clock_position_at(disabled_at + Duration::from_millis(500));

    // Без audio clock видео ведёт monotonic clock от позиции поломки — кадры продолжают идти.
    assert!(!session.pipeline.has_audio_clock());
    assert_eq!(
        later_position,
        POSITION_AT_UNPLUG + Duration::from_millis(500)
    );
}

#[test]
fn default_that_breaks_again_and_again_does_not_cause_recreation_storm() {
    let (mut session, factory_handle, first_output) = playing_session_with_active_output();
    factory_handle.set_devices(ONLY_SYSTEM_DEFAULT);
    let unplugged_at = Instant::now();
    first_output.fail_stream();

    // Каждый новый output на default тут же снова ломается (например, звуковой сервер падает).
    let mut outcomes = Vec::new();
    for second in 0..10 {
        outcomes.push(
            session
                .recover_failed_audio_output_if_needed(unplugged_at + Duration::from_secs(second)),
        );
        if let Some(current_output) = factory_handle.last_output_handle() {
            current_output.fail_stream();
        }
    }

    assert_eq!(factory_handle.create_count(), 4);
    assert_eq!(
        &outcomes[..4],
        &[
            AudioOutputRecoveryOutcome::SwitchedToSystemDefault,
            AudioOutputRecoveryOutcome::SwitchedToSystemDefault,
            AudioOutputRecoveryOutcome::SwitchedToSystemDefault,
            AudioOutputRecoveryOutcome::AudioDisabled,
        ]
    );
    assert!(!session.pipeline.has_audio_output());
}

#[test]
fn new_media_output_gets_fresh_recovery_budget() {
    let (mut session, factory_handle, first_output) = playing_session_with_active_output();
    factory_handle.set_devices(NO_DEVICES);
    let unplugged_at = Instant::now();
    first_output.fail_stream();
    for attempt in 0..3 {
        session.recover_failed_audio_output_if_needed(unplugged_at + Duration::from_secs(attempt));
    }
    assert!(!session.pipeline.has_audio_output());

    // Пользователь вернул устройство и открыл следующий файл: звук создаётся заново.
    factory_handle.set_devices(ONLY_SYSTEM_DEFAULT);
    let track_id = TrackId::new(AUDIO_TRACK);
    session.pipeline.select_audio_track(track_id);
    session
        .pipeline
        .install_audio_decoder(Box::new(RecordingPcmDecoder::new(
            Arc::new(Mutex::new(Vec::new())),
            DECODED_PCM.to_vec(),
            48_000,
            2,
        )));
    feed_one_audio_packet(&mut session);
    let reopened_output = factory_handle.last_output_handle().expect("output");
    reopened_output.fail_stream();
    let outcome =
        session.recover_failed_audio_output_if_needed(unplugged_at + Duration::from_secs(10));

    assert_eq!(outcome, AudioOutputRecoveryOutcome::SwitchedToSystemDefault);
}

#[test]
fn settings_device_change_never_substitutes_default() {
    let (mut session, factory_handle, first_output) = playing_session_with_active_output();
    factory_handle.set_devices(ONLY_SYSTEM_DEFAULT);

    let error = session
        .recreate_active_audio_output(AudioOutputDeviceRequest::SelectedOnly)
        .expect_err("явно выбранное, но отключённое устройство — это ошибка для настроек");

    assert_eq!(error.kind, PlayerErrorKind::AudioDeviceUnavailable);
    assert_eq!(
        factory_handle.device_requests().last(),
        Some(&AudioOutputDeviceRequest::SelectedOnly)
    );
    // Старый output остаётся рабочим и продолжает получать звук.
    feed_one_audio_packet(&mut session);
    assert_eq!(first_output.written_samples().len(), DECODED_PCM.len() * 2);
    assert!(switch_events(&session.take_events()).is_empty());
}
