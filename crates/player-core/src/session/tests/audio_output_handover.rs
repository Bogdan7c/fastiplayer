//! Пересоздание audio output-а не должно сдвигать звук относительно видео (C3).
//!
//! Проверяется наблюдаемый A/V синхрон: media-время, которое presentation clock показывает
//! в момент, когда новый output начинает играть первый настоящий PCM, должно совпадать с
//! PTS этого PCM (позиция слышимого звука + хвост старого output-а).

use audio_core::{
    AudioChannelLayout, AudioOutputDeviceRequest, AudioOutputInputFrameCount, AudioOutputSpec,
    AudioOutputStreamFrameCount, AudioOutputWriteError, AudioOutputWriteReport,
};

use super::audio_output_recovery::{
    ONLY_SYSTEM_DEFAULT, POSITION_AT_UNPLUG, feed_one_audio_packet,
    playing_session_with_active_output, session_with_audio_track,
};
use super::test_support::ScriptedAudioOutput;
use super::*;
use crate::session::audio_output_handover::prefill_replacement_output_with_silence;

/// PCM, который старый output принял, но ещё не успел проиграть.
const UNPLAYED_TAIL: Duration = Duration::from_millis(200);

/// Sample rate fake decoder-а в тестах session.
const SAMPLE_RATE: u32 = 48_000;

/// Допуск A/V расхождения: один кадр на 48 кГц округляется до ~21 мкс, берём 1 мс с запасом.
const SYNC_TOLERANCE: Duration = Duration::from_millis(1);

fn stereo_spec() -> AudioOutputSpec {
    AudioOutputSpec::new(SAMPLE_RATE, AudioChannelLayout::stereo())
}

/// Расхождение двух media-позиций по модулю.
fn drift(left: Duration, right: Duration) -> Duration {
    left.abs_diff(right)
}

/// Длительность тишины, записанной в output, по числу interleaved stereo samples.
fn written_duration(output: &ScriptedAudioOutputHandle) -> Duration {
    let frames = output.written_samples().len() / 2;
    Duration::from_secs_f64(frames as f64 / f64::from(SAMPLE_RATE))
}

/// Media-время presentation clock-а, когда новый output дошёл до clock-позиции `played`.
fn media_time_when_new_output_reaches(
    session: &PlayerSession,
    new_output: &ScriptedAudioOutputHandle,
    played: Duration,
) -> Duration {
    new_output.set_underlying_output_timing(played, played);
    session.presentation_clock_position_at(Instant::now())
}

use super::test_support::ScriptedAudioOutputHandle;

#[test]
fn recreated_output_keeps_audio_in_sync_with_video_despite_unplayed_tail() {
    let (mut session, factory_handle, first_output) = playing_session_with_active_output();
    factory_handle.set_devices(ONLY_SYSTEM_DEFAULT);
    // Старый output проиграл 1 с, а ещё 200 мс PCM лежат в его очереди и пропадут.
    first_output.set_underlying_output_timing(
        Duration::from_secs(1),
        Duration::from_secs(1) + UNPLAYED_TAIL,
    );

    first_output.fail_stream();
    session.recover_failed_audio_output_if_needed(Instant::now());

    let new_output = factory_handle.last_output_handle().expect("новый output");
    // Decoder ушёл дальше хвоста: первый настоящий PCM — это PTS `T + хвост`.
    let first_real_pcm_pts = POSITION_AT_UNPLUG + UNPLAYED_TAIL;
    // Настоящий PCM встанет в очередь нового output-а сразу после записанной тишины.
    let first_real_pcm_output_position = written_duration(&new_output);
    let clock_media_time =
        media_time_when_new_output_reaches(&session, &new_output, first_real_pcm_output_position);

    assert!(
        drift(clock_media_time, first_real_pcm_pts) <= SYNC_TOLERANCE,
        "звук расходится с видео: clock {clock_media_time:?}, PTS звука {first_real_pcm_pts:?}"
    );
}

#[test]
fn replacement_output_before_any_playback_does_not_gain_silence() {
    let (mut session, factory_handle, first_output) = playing_session_with_active_output();
    factory_handle.set_devices(ONLY_SYSTEM_DEFAULT);
    // Хвоста нет (например, buffer только что очищен перемоткой): тишина не нужна.
    first_output.set_underlying_output_timing(Duration::from_secs(1), Duration::from_secs(1));

    first_output.fail_stream();
    session.recover_failed_audio_output_if_needed(Instant::now());

    let new_output = factory_handle.last_output_handle().expect("новый output");
    assert!(new_output.written_samples().is_empty());
    assert_eq!(session.current_source_position, POSITION_AT_UNPLUG);
}

#[test]
fn settings_device_change_uses_same_handover_path() {
    let (mut session, factory_handle, first_output) = playing_session_with_active_output();
    first_output.set_underlying_output_timing(
        Duration::from_secs(3),
        Duration::from_secs(3) + UNPLAYED_TAIL,
    );

    session
        .recreate_active_audio_output(AudioOutputDeviceRequest::SelectedOnly)
        .expect("выбранное устройство подключено");

    let new_output = factory_handle.last_output_handle().expect("новый output");
    assert_eq!(written_duration(&new_output), UNPLAYED_TAIL);
    let clock_media_time = media_time_when_new_output_reaches(&session, &new_output, UNPLAYED_TAIL);
    assert!(
        drift(clock_media_time, POSITION_AT_UNPLUG + UNPLAYED_TAIL) <= SYNC_TOLERANCE,
        "смена устройства в настройках сдвинула звук: {clock_media_time:?}"
    );
}

#[test]
fn recreation_while_paused_prefills_silence_without_starting_output() {
    let (mut session, factory_handle) = session_with_audio_track();
    feed_one_audio_packet(&mut session);
    let first_output = factory_handle.last_output_handle().expect("output создан");
    factory_handle.set_devices(ONLY_SYSTEM_DEFAULT);
    first_output.set_underlying_output_timing(
        Duration::from_secs(1),
        Duration::from_secs(1) + UNPLAYED_TAIL,
    );
    assert_ne!(session.playback_state(), PlaybackState::Playing);

    first_output.fail_stream();
    session.recover_failed_audio_output_if_needed(Instant::now());

    let new_output = factory_handle.last_output_handle().expect("новый output");
    assert_eq!(written_duration(&new_output), UNPLAYED_TAIL);
    assert_eq!(new_output.play_count(), 0);
}

#[test]
fn recreation_during_eof_drain_keeps_tail_length_and_restarts_output() {
    let (mut session, factory_handle, first_output) = playing_session_with_active_output();
    factory_handle.set_devices(ONLY_SYSTEM_DEFAULT);
    session.snapshot.playback_state = PlaybackState::Draining;
    first_output.set_underlying_output_timing(
        Duration::from_secs(9),
        Duration::from_secs(9) + UNPLAYED_TAIL,
    );

    first_output.fail_stream();
    session.recover_failed_audio_output_if_needed(Instant::now());

    let new_output = factory_handle.last_output_handle().expect("новый output");
    // Хвост EOF-drain остаётся той же длины: новый output «доигрывает» его тишиной.
    assert_eq!(written_duration(&new_output), UNPLAYED_TAIL);
    assert_eq!(new_output.play_count(), 1);
    assert_eq!(
        new_output.clock().output_timing().audible_output_position(),
        Duration::ZERO
    );
}

#[test]
fn silence_length_follows_tail_sample_rate_and_channels() {
    let mut output = ScriptedAudioOutput::new(0.0, None);

    let frames = prefill_replacement_output_with_silence(&mut output, stereo_spec(), UNPLAYED_TAIL)
        .expect("fake принимает всё");

    assert_eq!(frames, 9_600);
    let written = output.handle.written_samples();
    assert_eq!(written.len(), 9_600 * 2);
    assert!(written.iter().all(|sample| *sample == 0.0));
}

#[test]
fn zero_tail_writes_nothing_to_replacement_output() {
    let mut output = ScriptedAudioOutput::new(0.0, None);

    let frames =
        prefill_replacement_output_with_silence(&mut output, stereo_spec(), Duration::ZERO)
            .expect("пустой хвост — не ошибка");

    assert_eq!(frames, 0);
    assert!(output.handle.written_samples().is_empty());
}

#[test]
fn rejected_silence_write_is_reported_as_audio_device_error() {
    let mut output = ScriptedAudioOutput::new(0.0, None);
    output
        .handle
        .set_write_result_override(Err(AudioOutputWriteError::InvalidChannelCount {
            boundary: "output",
        }));

    let error = prefill_replacement_output_with_silence(&mut output, stereo_spec(), UNPLAYED_TAIL)
        .expect_err("ошибка записи не должна глотаться");

    assert_eq!(error.kind, PlayerErrorKind::AudioDeviceUnavailable);
}

#[test]
fn partially_accepted_silence_is_reported_as_audio_device_error() {
    let mut output = ScriptedAudioOutput::new(0.0, None);
    let partial = AudioOutputWriteReport::try_new(
        AudioOutputInputFrameCount::new(9_600),
        AudioOutputStreamFrameCount::new(9_600),
        AudioOutputStreamFrameCount::new(100),
    )
    .expect("queued не больше prepared");
    output.handle.set_write_result_override(Ok(partial));

    let error = prefill_replacement_output_with_silence(&mut output, stereo_spec(), UNPLAYED_TAIL)
        .expect_err("неполная запись тишины делает output непригодным");

    assert_eq!(error.kind, PlayerErrorKind::AudioDeviceUnavailable);
}
