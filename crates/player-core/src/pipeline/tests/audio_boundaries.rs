//! Аудио-декодер, выход, EOF drain и generation очистки буфера.

use super::*;

#[test]
fn audio_decoder_boundaries_preserve_absent_success_and_error_states() {
    let mut pipeline = PlaybackPipeline::default();
    let packet = audio_core::EncodedAudioPacket::without_timing(TrackId::new(2).get(), b"packet");
    let bad_packet =
        audio_core::EncodedAudioPacket::without_timing(TrackId::new(2).get(), b"bad packet");

    assert!(!pipeline.has_audio_decoder());
    assert!(pipeline.decode_audio_packet(&packet).is_none());
    assert!(pipeline.reset_audio_decoder().is_none());

    pipeline.install_audio_decoder(Box::new(FakeAudioDecoder::with_samples(
        vec![0.25, -0.25],
        44_100,
        2,
    )));

    let decoded_audio = pipeline
        .decode_audio_packet(&packet)
        .expect("installed decoder должен вернуть decode result")
        .expect("successful fake decoder не должен падать");
    let DecodedAudioPacket::Pcm {
        samples,
        output_spec,
    } = decoded_audio
    else {
        panic!("non-empty fake PCM must carry its atomic output spec");
    };
    assert_eq!(samples, vec![0.25, -0.25]);
    assert_eq!(output_spec.sample_rate, 44_100);
    assert_eq!(
        output_spec.channel_layout,
        audio_core::AudioChannelLayout::stereo()
    );

    pipeline.clear_audio_decoder();
    assert!(!pipeline.has_audio_decoder());

    pipeline.install_audio_decoder(Box::new(FakeAudioDecoder::with_decode_error(
        "decode failed",
    )));

    let decode_error = pipeline
        .decode_audio_packet(&bad_packet)
        .expect("installed decoder должен сохранить decode error")
        .expect_err("decode error должен дойти до session boundary");
    assert_eq!(decode_error.to_string(), "decode failed");

    pipeline.clear_audio_decoder();
    pipeline.install_audio_decoder(Box::new(FakeAudioDecoder::with_reset_error("reset failed")));

    let reset_error = pipeline
        .reset_audio_decoder()
        .expect("installed decoder должен вернуть reset result")
        .expect_err("reset error должен дойти до session boundary");
    assert_eq!(reset_error.to_string(), "reset failed");
}

#[test]
fn deferred_audio_decoder_config_boundary_preserves_absent_match_and_mismatch() {
    let mut pipeline = PlaybackPipeline::default();
    let config = audio_core::AudioDecoderConfig::from_track_metadata(
        TrackId::new(2).get(),
        "A_AAC",
        None,
        None,
    );

    assert!(!pipeline.has_deferred_audio_decoder_config());
    assert!(
        pipeline
            .take_deferred_audio_decoder_config(TrackId::new(2))
            .is_none()
    );

    pipeline.install_deferred_audio_decoder_config(config.clone());
    assert!(pipeline.has_deferred_audio_decoder_config());
    assert!(
        pipeline
            .take_deferred_audio_decoder_config(TrackId::new(3))
            .is_none()
    );
    assert!(pipeline.has_deferred_audio_decoder_config());

    let taken_config = pipeline
        .take_deferred_audio_decoder_config(TrackId::new(2))
        .expect("matching track should consume deferred decoder config");
    assert_eq!(taken_config, config);
    assert!(!pipeline.has_deferred_audio_decoder_config());
}

#[test]
fn audio_seek_runtime_state_classifies_slots_without_cpal_output() {
    assert_eq!(
        audio_seek_runtime_state_from_slots(false, false, false),
        AudioSeekRuntimeState::NoSelectedAudio
    );
    assert_eq!(
        audio_seek_runtime_state_from_slots(true, false, false),
        AudioSeekRuntimeState::WaitingForDecoder
    );
    assert_eq!(
        audio_seek_runtime_state_from_slots(true, true, false),
        AudioSeekRuntimeState::WaitingForOutput
    );
    assert_eq!(
        audio_seek_runtime_state_from_slots(true, true, true),
        AudioSeekRuntimeState::Ready
    );
}

#[test]
fn audio_seek_runtime_state_boundary_keeps_selection_ownership() {
    let mut pipeline = PlaybackPipeline::default();
    let track_id = TrackId::new(2);
    let decoder_config =
        audio_core::AudioDecoderConfig::from_track_metadata(track_id.get(), "A_OPUS", None, None);

    assert_eq!(
        pipeline.audio_seek_runtime_state(),
        AudioSeekRuntimeState::NoSelectedAudio
    );

    pipeline.select_audio_track(track_id);
    assert_eq!(
        pipeline.audio_seek_runtime_state(),
        AudioSeekRuntimeState::WaitingForDecoder
    );

    pipeline.install_deferred_audio_decoder_config(decoder_config);
    assert_eq!(
        pipeline.audio_seek_runtime_state(),
        AudioSeekRuntimeState::WaitingForDecoder
    );
    assert_eq!(pipeline.selected_audio_track_id(), Some(track_id));

    pipeline.install_audio_decoder(Box::new(FakeAudioDecoder::with_samples(
        vec![0.0, 0.0],
        48_000,
        2,
    )));
    assert_eq!(
        pipeline.audio_seek_runtime_state(),
        AudioSeekRuntimeState::WaitingForOutput
    );
    assert_eq!(pipeline.selected_audio_track_id(), Some(track_id));
}

#[test]
fn absent_audio_output_boundaries_are_noop_without_losing_absent_state() {
    let mut pipeline = PlaybackPipeline::default();

    assert!(!pipeline.has_audio_output());
    assert_eq!(
        pipeline.write_audio_output_samples(&[0.0, 0.1], AudioOutputWriteIntent::DirectDecodedPcm,),
        AudioOutputRoutingStatus::AudioOutputAbsent
    );
    assert!(pipeline.play_audio_output().is_none());
    assert!(pipeline.pause_audio_output_and_capture_clock().is_none());
    assert!(pipeline.clear_audio_output_for_seek(1).is_none());
    assert!(pipeline.audio_output_buffer_level_ms().is_none());
    assert!(pipeline.audio_output_clock().is_none());
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::NoSelectedAudio
    );

    pipeline.clear_audio_output();
    assert!(!pipeline.has_audio_output());
}

#[test]
fn installed_audio_output_boundary_forwards_calls_and_neutral_clock() {
    let mut pipeline = PlaybackPipeline::default();
    let output = FixedAudioOutput::new(42.0);
    let clock = output.clock_handle();
    let volume = output.volume_handle();

    pipeline.install_audio_output_for_tests(Box::new(output));

    assert!(pipeline.has_audio_output());
    assert!(pipeline.audio_output_clock().is_some());
    assert_eq!(
        pipeline
            .write_audio_output_samples(&[0.1, -0.1], AudioOutputWriteIntent::DirectDecodedPcm,),
        AudioOutputRoutingStatus::Written(AudioOutputWriteReport::complete(
            AudioOutputInputFrameCount::new(2),
            AudioOutputStreamFrameCount::new(2),
        ))
    );
    assert_eq!(pipeline.audio_output_buffer_level_ms(), Some(42.0));
    assert_eq!(
        pipeline
            .clear_audio_output_for_seek(7)
            .expect("installed output должен вернуть clear result")
            .expect("fake clear должен быть успешным"),
        7
    );
    assert!(pipeline.set_audio_output_volume(0.25));
    assert_eq!(
        *volume.lock().expect("fake volume mutex не должен ломаться"),
        Some(0.25)
    );

    pipeline.install_audio_clock(clock);
    assert!(pipeline.reset_audio_clock());
}

#[test]
fn audio_output_boundary_preserves_play_pause_errors() {
    let mut pipeline = PlaybackPipeline::default();

    pipeline.install_audio_output_for_tests(Box::new(FixedAudioOutput::with_errors(
        Some("play failed"),
        Some("pause failed"),
    )));

    let play_error = pipeline
        .play_audio_output()
        .expect("installed output должен вернуть play result")
        .expect_err("play error должен пройти через boundary");
    assert_eq!(play_error.to_string(), "play failed");

    let pause_error = pipeline
        .pause_audio_output_and_capture_clock()
        .expect("installed output должен вернуть pause result")
        .expect_err("pause error должен пройти через boundary");
    assert_eq!(pause_error.to_string(), "pause failed");
}

#[test]
fn audio_eof_drain_state_preserves_queue_output_and_playback_distinctions() {
    let mut pipeline = PlaybackPipeline::default();
    let audio_track_id = TrackId::new(2);

    pipeline.select_audio_track(audio_track_id);
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::NoOutput
    );

    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
        audio_track_id,
        Duration::ZERO,
        None,
        Some(Duration::from_millis(20)),
        pipeline.seek_generation(),
        Bytes::from_static(b"encoded-audio"),
    ));
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::PendingPackets { queued_packets: 1 }
    );

    let _pending_packet = pipeline.pop_pending_audio_packet_front();
    pipeline.install_audio_output_for_tests(Box::new(FixedAudioOutput::new(24.0)));
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::DrainingOutput {
            buffer_level_ms: 24.0,
            playback_requested: false,
        }
    );

    pipeline
        .play_audio_output()
        .expect("installed output должен вернуть play result")
        .expect("fake output play должен быть успешным");
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::DrainingOutput {
            buffer_level_ms: 24.0,
            playback_requested: true,
        }
    );

    pipeline.install_audio_output_for_tests(Box::new(FixedAudioOutput::new(0.0)));
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::DrainedOutput {
            playback_requested: false,
        }
    );
}

#[test]
fn audio_eof_drain_waits_for_pcm_already_submitted_to_dac() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.select_audio_track(TrackId::new(2));

    let output = FixedAudioOutput::new(0.0);
    let output_clock = output.clock_handle();
    output_clock.set_output_timing(Duration::ZERO, Duration::from_millis(100));
    pipeline.install_audio_output_for_tests(Box::new(output));

    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::DrainingOutput {
            buffer_level_ms: 100.0,
            playback_requested: false,
        }
    );

    output_clock.set_output_timing(Duration::from_millis(100), Duration::from_millis(100));
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::DrainedOutput {
            playback_requested: false,
        }
    );
}

#[test]
fn audio_buffer_clear_generation_boundary_records_ack_generation() {
    let mut pipeline = PlaybackPipeline::default();

    assert_eq!(pipeline.audio_buffer_clear_generation(), 0);

    pipeline.mark_audio_buffer_clear_ack(7);
    assert_eq!(pipeline.audio_buffer_clear_generation(), 7);

    // Второе подтверждение заменяет первое: хранится именно последнее поколение,
    // а не их сумма — иначе seek-gate сверял бы несуществующую generation.
    pipeline.mark_audio_buffer_clear_ack(9);
    assert_eq!(pipeline.audio_buffer_clear_generation(), 9);
}
