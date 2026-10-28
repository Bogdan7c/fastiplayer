//! Boundary передачи presentation clock от доигранного звука monotonic-часам.

use super::*;

/// Позиция timeline, к которой привязан audio clock в тестах (конец звука 10 с).
const AUDIO_END_MEDIA_BASE: Duration = Duration::from_secs(10);

/// Pipeline с выбранным звуком и установленным audio clock на конце media base.
fn pipeline_with_audio_output(buffer_level_ms: f64) -> (PlaybackPipeline, Arc<FixedAudioClock>) {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.select_audio_track(TrackId::new(2));
    let output = FixedAudioOutput::new(buffer_level_ms);
    let output_clock = output.clock_handle();
    let player_clock: Arc<dyn PlayerAudioClock> = output_clock.clone();
    pipeline.install_audio_output_for_tests(Box::new(output));
    pipeline.install_audio_clock(player_clock);
    pipeline.set_media_clock_base(AUDIO_END_MEDIA_BASE);
    (pipeline, output_clock)
}

#[test]
fn handover_without_audio_clock_is_explicit_no_op() {
    let mut pipeline = PlaybackPipeline::default();
    let now = Instant::now();

    let outcome = pipeline.hand_over_presentation_clock_after_audio_end(now, PlaybackRate::NORMAL);

    assert_eq!(outcome, AudioEndClockHandoverOutcome::NoAudioClock);
    assert_eq!(
        pipeline.presentation_clock_source(),
        PresentationClockSource::MonotonicWithoutAudio
    );
    assert_eq!(
        pipeline.monotonic_media_position(now),
        None,
        "absent audio clock must not start a monotonic anchor behind the caller's back"
    );
}

#[test]
fn handover_is_rejected_while_audio_tail_still_plays() {
    let (mut pipeline, _clock) = pipeline_with_audio_output(25.0);
    let now = Instant::now();

    let outcome = pipeline.hand_over_presentation_clock_after_audio_end(now, PlaybackRate::NORMAL);

    assert_eq!(
        outcome,
        AudioEndClockHandoverOutcome::AudioTailNotDrained {
            audio_tail: AudioEofDrainState::DrainingOutput {
                buffer_level_ms: 25.0,
                playback_requested: false,
            },
        }
    );
    assert_eq!(
        pipeline.presentation_clock_source(),
        PresentationClockSource::AudioOutput
    );
    assert_eq!(pipeline.monotonic_media_position(now), None);
}

#[test]
fn drained_audio_hands_over_to_monotonic_clock_from_audio_end_at_playback_rate() {
    let (mut pipeline, _clock) = pipeline_with_audio_output(0.0);
    let two_x_rate = PlaybackRate::new(2.0).expect("2x playback rate must validate");
    let handed_over_at = Instant::now();
    let generation_before = pipeline.seek_generation();

    let outcome = pipeline.hand_over_presentation_clock_after_audio_end(handed_over_at, two_x_rate);

    assert_eq!(
        outcome,
        AudioEndClockHandoverOutcome::HandedOver {
            audio_end_position: AUDIO_END_MEDIA_BASE,
        }
    );
    assert_eq!(
        pipeline.presentation_clock_source(),
        PresentationClockSource::MonotonicAfterAudioEnd
    );
    // 2x: 100 мс wall-time = 200 мс media-time от конца звука.
    assert_eq!(
        pipeline.monotonic_media_position(handed_over_at + Duration::from_millis(100)),
        Some(AUDIO_END_MEDIA_BASE + Duration::from_millis(200))
    );
    // Boundary не трогает чужое состояние: audio tail, generation и media base прежние.
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::DrainedOutput {
            playback_requested: false,
        }
    );
    assert_eq!(pipeline.seek_generation(), generation_before);
    assert_eq!(pipeline.media_clock_base(), AUDIO_END_MEDIA_BASE);
    assert!(pipeline.has_audio_clock());
}

#[test]
fn repeated_handover_keeps_the_original_monotonic_anchor() {
    let (mut pipeline, _clock) = pipeline_with_audio_output(0.0);
    let handed_over_at = Instant::now();
    let _first =
        pipeline.hand_over_presentation_clock_after_audio_end(handed_over_at, PlaybackRate::NORMAL);

    let repeated = pipeline.hand_over_presentation_clock_after_audio_end(
        handed_over_at + Duration::from_millis(500),
        PlaybackRate::NORMAL,
    );

    assert_eq!(repeated, AudioEndClockHandoverOutcome::AlreadyHandedOver);
    assert_eq!(
        pipeline.monotonic_media_position(handed_over_at + Duration::from_millis(500)),
        Some(AUDIO_END_MEDIA_BASE + Duration::from_millis(500)),
        "a repeated EOF-drain tick must not re-anchor the running video tail"
    );
}

/// Разрыв времени, который тест применяет к pipeline после передачи clock-а.
type TimeDiscontinuity = fn(&mut PlaybackPipeline, Arc<FixedAudioClock>);

#[test]
fn every_time_discontinuity_returns_presentation_to_audio_clock() {
    let now = Instant::now();
    let discontinuities: [(&str, TimeDiscontinuity); 4] = [
        ("seek generation", |pipeline, _clock| {
            let _generation = pipeline.begin_seek_generation();
        }),
        ("media clock rebase", |pipeline, _clock| {
            pipeline.set_media_clock_base(Duration::from_secs(3));
        }),
        ("audio clock reinstall", |pipeline, clock| {
            let player_clock: Arc<dyn PlayerAudioClock> = clock;
            pipeline.install_audio_clock(player_clock);
        }),
        ("audio clock reset", |pipeline, _clock| {
            assert!(pipeline.reset_audio_clock());
        }),
    ];

    for (name, discontinuity) in discontinuities {
        let (mut pipeline, clock) = pipeline_with_audio_output(0.0);
        let _handed_over =
            pipeline.hand_over_presentation_clock_after_audio_end(now, PlaybackRate::NORMAL);
        assert_eq!(
            pipeline.presentation_clock_source(),
            PresentationClockSource::MonotonicAfterAudioEnd
        );

        discontinuity(&mut pipeline, clock);

        assert_eq!(
            pipeline.presentation_clock_source(),
            PresentationClockSource::AudioOutput,
            "{name} must cancel the audio-end handover"
        );
    }
}

#[test]
fn media_reset_and_clock_removal_drop_the_handover() {
    let now = Instant::now();
    let (mut removed_clock_pipeline, _clock) = pipeline_with_audio_output(0.0);
    let _handed_over = removed_clock_pipeline
        .hand_over_presentation_clock_after_audio_end(now, PlaybackRate::NORMAL);
    removed_clock_pipeline.clear_audio_clock();
    assert_eq!(
        removed_clock_pipeline.presentation_clock_source(),
        PresentationClockSource::MonotonicWithoutAudio
    );
    let player_clock: Arc<dyn PlayerAudioClock> = Arc::new(FixedAudioClock::new(Duration::ZERO, 0));
    removed_clock_pipeline.install_audio_clock(player_clock);
    assert_eq!(
        removed_clock_pipeline.presentation_clock_source(),
        PresentationClockSource::AudioOutput,
        "a new audio clock must not inherit the old handover"
    );

    let (mut reset_pipeline, clock) = pipeline_with_audio_output(0.0);
    let _handed_over =
        reset_pipeline.hand_over_presentation_clock_after_audio_end(now, PlaybackRate::NORMAL);
    reset_pipeline.reset_media_slots();
    let player_clock: Arc<dyn PlayerAudioClock> = clock;
    reset_pipeline.audio_clock = Some(player_clock);
    assert_eq!(
        reset_pipeline.presentation_clock_source(),
        PresentationClockSource::AudioOutput,
        "media reset must forget the handover even if a clock slot is refilled directly"
    );
}

/// Кладёт в очередь audio packet выбранного трека текущего поколения (звук после разрыва).
fn enqueue_audio_after_gap(pipeline: &mut PlaybackPipeline, pts: Duration) {
    let generation = pipeline.seek_generation();
    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
        TrackId::new(2),
        pts,
        None,
        Some(Duration::from_millis(100)),
        generation,
        Bytes::from_static(b"audio-after-gap"),
    ));
}

#[test]
fn gap_handover_rejects_missing_clock_missing_output_and_playing_tail() {
    let now = Instant::now();
    let mut without_clock = PlaybackPipeline::default();
    assert_eq!(
        without_clock.hand_over_presentation_clock_for_audio_gap(now, PlaybackRate::NORMAL),
        AudioEndClockHandoverOutcome::NoAudioClock
    );

    let mut clock_without_output = PlaybackPipeline::default();
    let player_clock: Arc<dyn PlayerAudioClock> = Arc::new(FixedAudioClock::new(Duration::ZERO, 0));
    clock_without_output.install_audio_clock(player_clock);
    assert_eq!(
        clock_without_output.hand_over_presentation_clock_for_audio_gap(now, PlaybackRate::NORMAL),
        AudioEndClockHandoverOutcome::NoAudioOutput
    );

    let (mut still_playing, _clock) = pipeline_with_audio_output(30.0);
    assert_eq!(
        still_playing.hand_over_presentation_clock_for_audio_gap(now, PlaybackRate::NORMAL),
        AudioEndClockHandoverOutcome::AudioOutputStillPlaying {
            unplayed_tail_ms: 30.0,
        }
    );
    assert_eq!(
        still_playing.presentation_clock_source(),
        PresentationClockSource::AudioOutput,
        "audio before the gap keeps the clock until it is fully heard"
    );
}

#[test]
fn gap_handover_accepts_held_audio_after_the_gap_unlike_eof_handover() {
    let (mut pipeline, _clock) = pipeline_with_audio_output(0.0);
    enqueue_audio_after_gap(&mut pipeline, Duration::from_secs(12));
    let now = Instant::now();

    assert_eq!(
        pipeline.hand_over_presentation_clock_after_audio_end(now, PlaybackRate::NORMAL),
        AudioEndClockHandoverOutcome::AudioTailNotDrained {
            audio_tail: AudioEofDrainState::PendingPackets { queued_packets: 1 },
        },
        "EOF handover must not ignore audio that is still to come"
    );
    assert_eq!(
        pipeline.hand_over_presentation_clock_for_audio_gap(now, PlaybackRate::NORMAL),
        AudioEndClockHandoverOutcome::HandedOver {
            audio_end_position: AUDIO_END_MEDIA_BASE,
        }
    );
    assert_eq!(
        pipeline.front_pending_audio_packet_start(),
        Some(Duration::from_secs(12)),
        "the handover must keep the held audio packet in the queue"
    );
    assert_eq!(pipeline.pending_audio_packet_len(), 1);
}

#[test]
fn audio_resume_after_gap_reanchors_clock_at_presentation_position() {
    let (mut pipeline, _clock) = pipeline_with_audio_output(0.0);
    let now = Instant::now();
    assert_eq!(
        pipeline.resume_audio_clock_after_gap(Duration::from_secs(12)),
        AudioGapResumeOutcome::NotHandedOver
    );
    assert_eq!(
        pipeline.media_clock_base(),
        AUDIO_END_MEDIA_BASE,
        "resume without a handover must not touch the audio clock mapping"
    );

    let _handed_over =
        pipeline.hand_over_presentation_clock_for_audio_gap(now, PlaybackRate::NORMAL);
    pipeline.note_audio_packet_written(
        Duration::from_millis(4_900),
        9_600,
        audio_output_spec_for_tests(48_000, 2),
    );
    let resumed_at = Duration::from_millis(12_030);
    let outcome = pipeline.resume_audio_clock_after_gap(resumed_at);

    assert_eq!(outcome, AudioGapResumeOutcome::Resumed { resumed_at });
    assert_eq!(
        pipeline.presentation_clock_source(),
        PresentationClockSource::AudioOutput
    );
    assert_eq!(pipeline.media_clock_base(), resumed_at);
    assert_eq!(
        pipeline.media_position_from_audio_clock(),
        resumed_at,
        "the audio clock continues exactly from the monotonic position (no jump)"
    );
    assert_eq!(pipeline.monotonic_media_position(now), None);
    assert_eq!(
        pipeline.written_audio_end(),
        None,
        "the audio after the gap starts a new continuous segment"
    );
}

#[test]
fn written_audio_end_tracks_the_furthest_pcm_and_resets_on_seek() {
    let (mut pipeline, _clock) = pipeline_with_audio_output(0.0);
    let stereo_48k = audio_output_spec_for_tests(48_000, 2);
    assert_eq!(pipeline.written_audio_end(), None);

    // 4800 stereo frames = 100 мс PCM.
    pipeline.note_audio_packet_written(Duration::from_secs(5), 9_600, stereo_48k);
    assert_eq!(
        pipeline.written_audio_end(),
        Some(Duration::from_millis(5_100))
    );
    // Перекрывающийся более ранний packet не сдвигает конец назад.
    pipeline.note_audio_packet_written(Duration::from_millis(4_950), 4_800, stereo_48k);
    assert_eq!(
        pipeline.written_audio_end(),
        Some(Duration::from_millis(5_100))
    );

    let _generation = pipeline.begin_seek_generation();
    assert_eq!(pipeline.written_audio_end(), None);
}
