//! Переключение аудиодорожки: настоящий MKV (`two-audio-tracks.mkv`: FLAC 440 Гц стерео
//! 22050 Гц и Opus 880 Гц моно 48000 Гц), настоящий демуксер и настоящие аудио-декодеры;
//! вместо звуковой карты — fake output, который запоминает записанные сэмплы.
//!
//! Проверяем наблюдаемое: какой сигнал дошёл до output-а, в каком формате пересоздан
//! output, что с позицией, состоянием и событиями.

pub(super) mod harness;

use std::time::Duration;

use crossbeam_channel::{TryRecvError, bounded};

use self::harness::*;
use super::*;
use crate::session::audio_track_switch::{AudioTrackSwitchOutcome, AudioTrackSwitchResume};
use crate::{
    InstalledMediaStateRestore, InstalledMediaStateRestoreOutcome, InstalledPositionRestore,
    InstalledSubtitleRestore, InstalledTrackRestore, PlaybackIntent, PlaybackState,
};

/// Доводит harness до Playing с живым первым output-ом (FLAC) и «проигранной» 1 секундой.
fn playing_first_track_at_one_second() -> SwitchHarness {
    let mut harness = SwitchHarness::audio_only(PlaybackIntent::StartPlaying);
    harness.tick_until("first output exists", |h| {
        h.output_factory.create_count() == 1
    });
    harness.play_out_latest(Duration::from_secs(1));
    harness.tick_until("playing", |h| {
        h.session.snapshot.playback_state == PlaybackState::Playing
            && h.position() >= Duration::from_millis(900)
    });
    harness
}

#[test]
fn switching_audio_track_plays_the_new_track_signal_from_the_same_position() {
    let mut harness = playing_first_track_at_one_second();
    let [first_track, second_track] = harness.audio_track_ids[..] else {
        panic!("fixture has two audio tracks");
    };
    assert_eq!(harness.selected_audio(), Some(first_track));
    let first_output = harness.latest_output();
    let first_samples = first_output.written_samples();
    assert!(
        (estimate_frequency_hz(&first_samples, FLAC_CHANNELS, FLAC_SAMPLE_RATE)
            - FLAC_FREQUENCY_HZ)
            .abs()
            < 15.0,
        "before the switch the output carries the 440 Hz FLAC track"
    );
    let position_before = harness.position();
    let _ = harness.session.take_events();

    let outcome = harness
        .session
        .switch_selected_audio_track(second_track)
        .expect("switch to a valid audio track");
    assert_eq!(
        outcome,
        AudioTrackSwitchOutcome::Switched(AudioTrackSwitchResume::SeekToCurrentPosition)
    );
    let switch_events = harness.session.take_events();
    assert_eq!(audio_selected_events(&switch_events), [second_track]);
    assert_eq!(seek_requested_count(&switch_events), 1);
    assert_eq!(harness.selected_audio(), Some(second_track));

    // Новый output пересоздан по spec ВТОРОЙ дорожки и получает её сигнал.
    harness.tick_until("second output created", |h| {
        h.output_factory.create_count() == 2
    });
    harness.tick_until("second output got audio", |h| {
        h.latest_output().written_samples().len() > OPUS_SAMPLE_RATE as usize / 4
    });
    let created_specs = harness.output_factory.created_specs();
    assert_eq!(
        (
            created_specs[1].sample_rate,
            created_specs[1].channels() as usize
        ),
        (OPUS_SAMPLE_RATE, OPUS_CHANNELS)
    );
    harness.play_out_latest(Duration::from_millis(200));
    harness.tick_until("playing again", |h| {
        h.session.snapshot.playback_state == PlaybackState::Playing
            && !h.session.snapshot.timeline.seeking
    });
    harness.ticks(60);

    let second_output = harness.latest_output();
    let second_samples = second_output.written_samples();
    let frequency = estimate_frequency_hz(&second_samples, OPUS_CHANNELS, OPUS_SAMPLE_RATE);
    assert!(
        (frequency - OPUS_FREQUENCY_HZ).abs() < 25.0,
        "after the switch the output carries the 880 Hz Opus track, got {frequency} Hz"
    );
    // Продолжение с той же точки, а не с начала файла: звука осталось ~(3 с - позиция).
    let remaining_seconds = seconds_of(&second_samples, OPUS_CHANNELS, OPUS_SAMPLE_RATE);
    let expected_remaining = FIXTURE_SECONDS - position_before.as_secs_f64();
    assert!(
        (remaining_seconds - expected_remaining).abs() < 0.4,
        "new track continues from ~{position_before:?}: wrote {remaining_seconds:.2}s, expected ~{expected_remaining:.2}s"
    );
    assert!(
        harness.position() >= position_before.saturating_sub(Duration::from_millis(300)),
        "position must not jump back to the start: {:?} vs {position_before:?}",
        harness.position()
    );
    // Старый output не получал звук второй дорожки (его буфер остался как был).
    assert_eq!(first_output.written_samples(), first_samples);
    assert_eq!(
        harness.decoder_factory.requested_decoder_count(),
        2,
        "one decoder per track"
    );
}

/// Состояние, которое отказ/no-op не имеют права менять.
#[derive(Debug, PartialEq)]
struct AudioPathObservation {
    selected_track: Option<TrackId>,
    has_decoder: bool,
    has_output: bool,
    decoders_requested: usize,
    outputs_created: usize,
    written_sample_count: usize,
}

fn observe(harness: &SwitchHarness) -> AudioPathObservation {
    AudioPathObservation {
        selected_track: harness.selected_audio(),
        has_decoder: harness.session.pipeline.has_audio_decoder(),
        has_output: harness.session.pipeline.has_audio_output(),
        decoders_requested: harness.decoder_factory.requested_decoder_count(),
        outputs_created: harness.output_factory.create_count(),
        written_sample_count: harness
            .output_factory
            .last_output_handle()
            .map_or(0, |output| output.written_samples().len()),
    }
}

#[test]
fn selecting_the_already_selected_track_is_a_noop_without_reset_seek_or_event() {
    let mut harness = playing_first_track_at_one_second();
    let first_track = harness.audio_track_ids[0];
    let _ = harness.session.take_events();
    let before = observe(&harness);
    assert!(
        before.has_decoder && before.has_output,
        "audio path is live"
    );

    let outcome = harness.session.switch_selected_audio_track(first_track);

    assert_eq!(outcome, Ok(AudioTrackSwitchOutcome::AlreadySelected));
    assert_eq!(observe(&harness), before, "decoder/output are untouched");
    let events = harness.session.take_events();
    assert!(
        audio_selected_events(&events).is_empty() && seek_requested_count(&events) == 0,
        "no event and no seek, got {events:?}"
    );
    assert!(!harness.session.snapshot.timeline.seeking);
}

#[test]
fn unknown_audio_track_id_is_rejected_and_changes_nothing() {
    let mut harness = playing_first_track_at_one_second();
    let _ = harness.session.take_events();
    let before = observe(&harness);
    let position_before = harness.position();

    let error = harness
        .session
        .switch_selected_audio_track(TrackId::new(9_999))
        .expect_err("a track that is not in the media must be rejected");

    assert_eq!(error.kind, PlayerErrorKind::InvalidCommand);
    assert_eq!(observe(&harness), before);
    assert_eq!(harness.position(), position_before);
    let events = harness.session.take_events();
    assert!(audio_selected_events(&events).is_empty());
    assert_eq!(seek_requested_count(&events), 0);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, PlayerEvent::RecoverableError(_))),
        "the rejection is reported as a recoverable error, got {events:?}"
    );
}

#[test]
fn video_track_id_is_rejected_as_an_audio_selection() {
    let (mut harness, _decoder) = SwitchHarness::with_video("two-audio-tracks.mkv");
    let video_track = harness
        .session
        .pipeline
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .expect("fixture has a video track")
        .id;
    let before = observe(&harness);
    let _ = harness.session.take_events();

    let error = harness
        .session
        .dispatch_command(PlayerCommand::SelectAudioTrack(video_track))
        .expect_err("video id is not an audio track");

    assert_eq!(error.kind, PlayerErrorKind::InvalidCommand);
    assert_eq!(observe(&harness), before);
    assert!(audio_selected_events(&harness.session.take_events()).is_empty());
}

#[test]
fn subtitle_track_id_is_rejected_as_an_audio_selection() {
    let (mut harness, _decoder) = SwitchHarness::with_video("two-srt-tracks.mkv");
    let subtitle_track = harness
        .session
        .pipeline
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Subtitle)
        .expect("fixture has subtitle tracks")
        .id;
    let before = observe(&harness);

    let error = harness
        .session
        .switch_selected_audio_track(subtitle_track)
        .expect_err("subtitle id is not an audio track");

    assert_eq!(error.kind, PlayerErrorKind::InvalidCommand);
    assert_eq!(observe(&harness), before);
}

#[test]
fn selecting_without_installed_media_or_after_shutdown_is_invalid_command() {
    let mut empty_session = PlayerSession::default();
    let no_media = empty_session
        .dispatch_command(PlayerCommand::SelectAudioTrack(TrackId::new(1)))
        .expect_err("no media installed");
    assert_eq!(no_media.kind, PlayerErrorKind::InvalidCommand);
    assert_eq!(empty_session.snapshot.selected_tracks.audio_track, None);

    let mut harness = playing_first_track_at_one_second();
    let second_track = harness.audio_track_ids[1];
    harness
        .session
        .dispatch_command(PlayerCommand::Shutdown)
        .expect("shutdown is accepted");
    let after_shutdown = harness
        .session
        .dispatch_command(PlayerCommand::SelectAudioTrack(second_track))
        .expect_err("commands after shutdown are refused");
    assert_eq!(after_shutdown.kind, PlayerErrorKind::InvalidCommand);
    assert_eq!(harness.selected_audio(), Some(harness.audio_track_ids[0]));
}

#[test]
fn switch_while_paused_keeps_paused_and_does_not_start_the_new_output() {
    let mut harness = playing_first_track_at_one_second();
    harness
        .session
        .dispatch_command(PlayerCommand::Pause)
        .expect("pause");
    assert_eq!(
        harness.session.snapshot.playback_state,
        PlaybackState::Paused
    );
    let second_track = harness.audio_track_ids[1];
    let _ = harness.session.take_events();

    let outcome = harness.session.switch_selected_audio_track(second_track);

    assert_eq!(
        outcome,
        Ok(AudioTrackSwitchOutcome::Switched(
            AudioTrackSwitchResume::SeekToCurrentPosition
        ))
    );
    for _ in 0..80 {
        harness.tick();
        assert_ne!(
            harness.session.snapshot.playback_state,
            PlaybackState::Playing,
            "the silent seek must not resume a paused player"
        );
    }
    assert_eq!(
        harness.session.snapshot.playback_state,
        PlaybackState::Paused
    );
    assert!(
        !harness.session.snapshot.timeline.seeking,
        "seek has settled"
    );
    assert_eq!(harness.selected_audio(), Some(second_track));
    assert!(
        harness.position() >= Duration::from_millis(700),
        "position stays near the pause point, got {:?}",
        harness.position()
    );
    if let Some(output) = harness.output_factory.last_output_handle() {
        assert_eq!(
            output.play_count(),
            0,
            "a paused player never starts the recreated output"
        );
    }
}

#[test]
fn switch_on_non_seekable_source_resets_audio_and_continues_without_seek() {
    let mut harness = SwitchHarness::audio_only_not_seekable(PlaybackIntent::StartPlaying);
    harness.tick_until("first output exists", |h| {
        h.output_factory.create_count() == 1
    });
    harness.tick_until("playing", |h| {
        h.session.snapshot.playback_state == PlaybackState::Playing
    });
    let second_track = harness.audio_track_ids[1];
    let _ = harness.session.take_events();

    let outcome = harness
        .session
        .switch_selected_audio_track(second_track)
        .expect("switch on a live-like source must not fail");

    assert_eq!(
        outcome,
        AudioTrackSwitchOutcome::Switched(AudioTrackSwitchResume::ContinueForwardWithoutSeek)
    );
    let events = harness.session.take_events();
    assert_eq!(
        seek_requested_count(&events),
        0,
        "no seek on a non-seekable source"
    );
    assert_eq!(audio_selected_events(&events), [second_track]);
    assert!(
        !harness.session.pipeline.has_audio_output(),
        "old output is gone"
    );

    // Воспроизведение не падает и не зависает: новая дорожка начинает играть со следующих пакетов.
    harness.tick_until("second output created", |h| {
        h.output_factory.create_count() == 2
    });
    harness.ticks(40);
    let created_specs = harness.output_factory.created_specs();
    assert_eq!(created_specs[1].sample_rate, OPUS_SAMPLE_RATE);
    let samples = harness.latest_output().written_samples();
    assert!(!samples.is_empty(), "the new track reaches the output");
    assert!(
        (estimate_frequency_hz(&samples, OPUS_CHANNELS, OPUS_SAMPLE_RATE) - OPUS_FREQUENCY_HZ)
            .abs()
            < 25.0
    );
    assert!(harness.session.snapshot.last_error.is_none(), "no error");
}

/// Запускает restore состояния сразу после установки и возвращает приёмник результата.
fn begin_restore(
    harness: &mut SwitchHarness,
    audio_track: InstalledTrackRestore,
    position: Duration,
) -> crossbeam_channel::Receiver<InstalledMediaStateRestoreOutcome> {
    let (outcome_tx, outcome_rx) = bounded(1);
    harness.session.begin_installed_media_state_restore(
        InstalledMediaStateRestore {
            request_id: harness.request_id,
            media_instance_id: harness.media_instance_id,
            video_track: InstalledTrackRestore::KeepDefault,
            audio_track,
            subtitle_track: InstalledSubtitleRestore::KeepDefault,
            volume: crate::InstalledVolumeRestore::KeepCurrent,
            position: InstalledPositionRestore::SeekTo(position),
        },
        outcome_tx,
    );
    outcome_rx
}

fn tick_until_restore_applied(
    harness: &mut SwitchHarness,
    outcome_rx: &crossbeam_channel::Receiver<InstalledMediaStateRestoreOutcome>,
) -> InstalledMediaStateRestoreOutcome {
    for _ in 0..400 {
        match outcome_rx.try_recv() {
            Ok(outcome) => return outcome,
            Err(TryRecvError::Empty) => harness.tick(),
            Err(TryRecvError::Disconnected) => panic!("restore outcome sender dropped"),
        }
    }
    panic!("restore outcome never arrived");
}

#[test]
fn restore_at_install_selects_second_track_and_position_restore_still_lands() {
    let mut harness = SwitchHarness::audio_only(PlaybackIntent::StartPaused);
    let second_track = harness.audio_track_ids[1];
    let restore_position = Duration::from_millis(1_200);

    let outcome_rx = begin_restore(
        &mut harness,
        InstalledTrackRestore::Select(second_track),
        restore_position,
    );
    let outcome = tick_until_restore_applied(&mut harness, &outcome_rx);

    assert_eq!(
        outcome,
        InstalledMediaStateRestoreOutcome::Applied {
            media_instance_id: harness.media_instance_id
        }
    );
    assert_eq!(harness.selected_audio(), Some(second_track));
    assert_eq!(
        harness.session.snapshot.playback_state,
        PlaybackState::Paused
    );
    let position = harness.position();
    assert!(
        position.abs_diff(restore_position) < Duration::from_millis(300),
        "position restore applies after the track switch: {position:?}"
    );
    // Звук после восстановления — вторая дорожка в её формате.
    harness.tick_until("output of the restored track", |h| {
        h.output_factory.create_count() >= 1
    });
    let created_specs = harness.output_factory.created_specs();
    assert!(
        created_specs
            .iter()
            .all(|spec| spec.sample_rate == OPUS_SAMPLE_RATE),
        "no output was ever built for the first track: {created_specs:?}"
    );
    assert_eq!(
        harness.decoder_factory.requested_decoder_count(),
        1,
        "only the restored track gets a decoder"
    );
}

#[test]
fn restore_selecting_the_default_track_does_not_reset_audio_or_add_a_seek() {
    let mut harness = SwitchHarness::audio_only(PlaybackIntent::StartPaused);
    let first_track = harness.audio_track_ids[0];
    let _ = harness.session.take_events();

    let outcome_rx = begin_restore(
        &mut harness,
        InstalledTrackRestore::Select(first_track),
        Duration::from_millis(1_200),
    );
    let outcome = tick_until_restore_applied(&mut harness, &outcome_rx);

    assert!(matches!(
        outcome,
        InstalledMediaStateRestoreOutcome::Applied { .. }
    ));
    assert_eq!(harness.selected_audio(), Some(first_track));
    assert_eq!(
        harness.session.snapshot.playback_state,
        PlaybackState::Paused
    );
    let events = harness.session.take_events();
    assert!(
        audio_selected_events(&events).is_empty(),
        "selecting the default track at restore is a no-op"
    );
    assert_eq!(
        seek_requested_count(&events),
        1,
        "only the position restore seeks, got {events:?}"
    );
}
