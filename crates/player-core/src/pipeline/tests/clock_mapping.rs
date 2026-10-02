//! Монотонный fallback без аудио и отображение аудио-часов при смене скорости.

use super::*;

#[test]
fn no_audio_monotonic_fallback_counts_position_from_anchor() {
    let mut pipeline = PlaybackPipeline::default();
    let anchored_at = Instant::now();
    let initial_position = Duration::from_millis(100);

    pipeline.start_monotonic_media_clock(initial_position, anchored_at, PlaybackRate::NORMAL);

    assert_eq!(
        pipeline.monotonic_media_position(anchored_at + Duration::from_millis(40)),
        Some(Duration::from_millis(140))
    );
}

#[test]
fn no_audio_monotonic_fallback_scales_position_by_playback_rate() {
    let anchored_at = Instant::now();
    let initial_position = Duration::from_millis(100);
    let two_x_rate = PlaybackRate::new(2.0).expect("2x playback rate must validate");
    let half_x_rate = PlaybackRate::new(0.5).expect("0.5x playback rate must validate");

    let mut fast_pipeline = PlaybackPipeline::default();
    fast_pipeline.start_monotonic_media_clock(initial_position, anchored_at, two_x_rate);

    assert_eq!(
        fast_pipeline.monotonic_media_position(anchored_at + Duration::from_millis(40)),
        Some(Duration::from_millis(180))
    );

    let mut slow_pipeline = PlaybackPipeline::default();
    slow_pipeline.start_monotonic_media_clock(initial_position, anchored_at, half_x_rate);

    assert_eq!(
        slow_pipeline.monotonic_media_position(anchored_at + Duration::from_millis(40)),
        Some(Duration::from_millis(120))
    );
}

#[test]
fn no_audio_monotonic_deadline_mapping_preserves_anchor_rounding_phase() {
    let empty_pipeline = PlaybackPipeline::default();
    let now = Instant::now();
    assert_eq!(
        empty_pipeline.monotonic_media_position_after_wall_delay(now, Duration::from_nanos(1)),
        None
    );
    assert_eq!(
        empty_pipeline.monotonic_wall_delay_until_media_deadline(now, Duration::from_nanos(1)),
        None
    );

    let one_and_half_rate = PlaybackRate::new(1.5).expect("1.5x playback rate must validate");
    let mut pipeline = PlaybackPipeline::default();
    pipeline.start_monotonic_media_clock(Duration::ZERO, now, one_and_half_rate);
    let current_time = now + Duration::from_nanos(1);

    assert_eq!(
        pipeline.monotonic_media_position(current_time),
        Some(Duration::from_nanos(1))
    );
    assert_eq!(
        pipeline.monotonic_media_position_after_wall_delay(current_time, Duration::from_nanos(1)),
        Some(Duration::from_nanos(3))
    );
    assert_eq!(
        pipeline.monotonic_wall_delay_until_media_deadline(current_time, Duration::from_nanos(3)),
        Some(Duration::from_nanos(1))
    );
}

#[test]
fn no_audio_monotonic_fallback_boundary_rates_saturate_without_wrapping() {
    let anchored_at = Instant::now();
    let near_max_position = Duration::MAX.saturating_sub(Duration::from_nanos(1));

    let mut max_rate_pipeline = PlaybackPipeline::default();
    max_rate_pipeline.start_monotonic_media_clock(
        near_max_position,
        anchored_at,
        PlaybackRate::MAX,
    );

    assert_eq!(
        max_rate_pipeline.monotonic_media_position(anchored_at + Duration::from_secs(1)),
        Some(Duration::MAX)
    );

    let mut min_rate_pipeline = PlaybackPipeline::default();
    min_rate_pipeline.start_monotonic_media_clock(Duration::ZERO, anchored_at, PlaybackRate::MIN);

    assert_eq!(
        min_rate_pipeline.monotonic_media_position(anchored_at + Duration::from_millis(4)),
        Some(Duration::from_millis(1))
    );
}

#[test]
fn installing_audio_clock_clears_monotonic_fallback_anchor() {
    let mut pipeline = PlaybackPipeline::default();
    let anchored_at = Instant::now();
    let clock = Arc::new(FixedAudioClock::new(Duration::from_millis(12), 3));

    pipeline.start_monotonic_media_clock(Duration::from_secs(3), anchored_at, PlaybackRate::NORMAL);
    assert!(pipeline.monotonic_media_position(anchored_at).is_some());

    pipeline.install_audio_clock(Arc::clone(&clock) as Arc<dyn PlayerAudioClock>);

    assert!(pipeline.has_audio_clock());
    assert_eq!(pipeline.audio_clock_now(), Duration::from_millis(12));
    assert_eq!(pipeline.audio_clock_underrun_callbacks(), 3);
    assert!(pipeline.monotonic_media_position(anchored_at).is_none());
    assert!(pipeline.reset_audio_clock());
    assert_eq!(clock.reset_count(), 1);
    assert_eq!(pipeline.audio_clock_now(), Duration::ZERO);
}

#[test]
fn audio_clock_mapping_scales_output_progress_by_playback_rate() {
    let mut pipeline = PlaybackPipeline::default();
    let clock = Arc::new(FixedAudioClock::new(Duration::ZERO, 0));
    pipeline.install_audio_clock(Arc::clone(&clock) as Arc<dyn PlayerAudioClock>);
    pipeline.reanchor_audio_clock_media_mapping(
        Duration::from_secs(10),
        PlaybackRate::new(2.0).expect("2x playback rate should be valid"),
    );

    clock.set_now(Duration::from_secs(1));

    assert_eq!(
        pipeline.media_position_from_audio_clock(),
        Duration::from_secs(12)
    );
}

/// Помогает сравнивать mapping позиции с допуском на f64 интерполяцию хвоста.
fn assert_media_position_close(actual: Duration, expected: Duration) {
    let delta = actual.abs_diff(expected);
    assert!(
        delta <= Duration::from_millis(1),
        "media position {actual:?} должна быть близка к {expected:?}"
    );
}

#[test]
fn rate_change_reanchor_accounts_written_output_tail_at_old_rate() {
    let mut pipeline = PlaybackPipeline::default();
    // Ring уже пуст, но 200 ms submitted PCM находятся между callback и DAC.
    let output = FixedAudioOutput::new(0.0);
    let clock = output.clock_handle();
    pipeline.install_audio_output_for_tests(Box::new(output));
    pipeline.install_audio_clock(clock.clone() as Arc<dyn PlayerAudioClock>);

    // Играем на 1.0x: anchor media 10s на output 60s.
    clock.set_output_timing(Duration::from_secs(60), Duration::from_millis(60_200));
    pipeline.reanchor_audio_clock_media_mapping(Duration::from_secs(10), PlaybackRate::NORMAL);

    // Смена на 2.0x при 200 ms записанного, но не проигранного output-а.
    pipeline.reanchor_audio_clock_media_mapping_for_rate_change(
        Duration::from_secs(10),
        PlaybackRate::new(2.0).expect("2x playback rate should be valid"),
    );

    // В момент смены позиция не прыгает.
    assert_media_position_close(
        pipeline.media_position_from_audio_clock(),
        Duration::from_secs(10),
    );

    // Внутри хвоста media идёт со СТАРЫМ темпом: +100 ms output = +100 ms media.
    clock.set_now(Duration::from_millis(60_100));
    assert_media_position_close(
        pipeline.media_position_from_audio_clock(),
        Duration::from_millis(10_100),
    );

    // Конец хвоста точен: 200 ms output старого rate = +200 ms media.
    clock.set_now(Duration::from_millis(60_200));
    assert_media_position_close(
        pipeline.media_position_from_audio_clock(),
        Duration::from_millis(10_200),
    );

    // После хвоста работает новый rate: ещё +1s output = +2s media,
    // а не замороженная ошибка `tail × (new − old)`.
    clock.set_now(Duration::from_millis(61_200));
    assert_media_position_close(
        pipeline.media_position_from_audio_clock(),
        Duration::from_millis(12_200),
    );
}

#[test]
fn rate_change_reanchor_without_buffered_output_matches_plain_reanchor() {
    let mut pipeline = PlaybackPipeline::default();
    let output = FixedAudioOutput::new(0.0);
    let clock = output.clock_handle();
    pipeline.install_audio_output_for_tests(Box::new(output));
    pipeline.install_audio_clock(clock.clone() as Arc<dyn PlayerAudioClock>);

    clock.set_now(Duration::from_secs(5));
    pipeline.reanchor_audio_clock_media_mapping_for_rate_change(
        Duration::from_secs(10),
        PlaybackRate::new(2.0).expect("2x playback rate should be valid"),
    );

    clock.set_now(Duration::from_secs(6));
    assert_eq!(
        pipeline.media_position_from_audio_clock(),
        Duration::from_secs(12)
    );
}

#[test]
fn repeated_rate_change_inside_tail_keeps_mapping_monotonic_and_bounded() {
    let mut pipeline = PlaybackPipeline::default();
    let output = FixedAudioOutput::new(0.0);
    let clock = output.clock_handle();
    pipeline.install_audio_output_for_tests(Box::new(output));
    pipeline.install_audio_clock(clock.clone() as Arc<dyn PlayerAudioClock>);

    clock.set_output_timing(Duration::from_secs(60), Duration::from_millis(60_200));
    pipeline.reanchor_audio_clock_media_mapping(Duration::from_secs(10), PlaybackRate::NORMAL);
    pipeline.reanchor_audio_clock_media_mapping_for_rate_change(
        Duration::from_secs(10),
        PlaybackRate::new(2.0).expect("2x playback rate should be valid"),
    );

    // Вторая смена в середине ещё не проигранного хвоста.
    clock.set_output_timing(Duration::from_millis(60_100), Duration::from_millis(60_300));
    let mid_tail_position = pipeline.media_position_from_audio_clock();
    pipeline.reanchor_audio_clock_media_mapping_for_rate_change(
        mid_tail_position,
        PlaybackRate::new(4.0).expect("4x playback rate should be valid"),
    );

    // Позиция в момент смены сохраняется.
    assert_media_position_close(
        pipeline.media_position_from_audio_clock(),
        mid_tail_position,
    );

    // Хвост (ещё 200 ms output) заканчивается на значении старого mapping,
    // а не прыгает на новый rate сразу.
    clock.set_now(Duration::from_millis(60_300));
    let tail_end_position = pipeline.media_position_from_audio_clock();
    assert!(tail_end_position >= mid_tail_position);
    assert!(
        tail_end_position <= Duration::from_millis(10_500),
        "конец хвоста {tail_end_position:?} не должен применять 4x к старому output-у"
    );

    // После хвоста media идёт с новым 4x темпом.
    clock.set_now(Duration::from_millis(61_300));
    assert_media_position_close(
        pipeline.media_position_from_audio_clock(),
        tail_end_position + Duration::from_secs(4),
    );
}

#[test]
fn passthrough_audio_history_is_bounded_and_keeps_latest_samples() {
    let mut pipeline = PlaybackPipeline::default();
    // 100 Hz stereo: бюджет 600 ms = 60 frames = 120 samples.
    let sample_rate = 100;
    let channels = 2;
    let output_spec = audio_output_spec_for_tests(sample_rate, channels);

    let old_chunk: Vec<f32> = (0..100).map(|i| i as f32).collect();
    let new_chunk: Vec<f32> = (100..160).map(|i| i as f32).collect();
    pipeline.record_passthrough_audio_history(&old_chunk, output_spec);
    pipeline.record_passthrough_audio_history(&new_chunk, output_spec);

    let history = pipeline.take_passthrough_audio_history_for_priming(output_spec);
    assert_eq!(
        history.len(),
        120,
        "история должна быть обрезана до бюджета"
    );
    assert_eq!(
        history.last().copied(),
        Some(159.0),
        "история должна хранить последние samples"
    );
    assert_eq!(
        history.len() % channels as usize,
        0,
        "история frame-aligned"
    );

    // Повторный take пуст: история одноразовая для одного прайминга.
    assert!(
        pipeline
            .take_passthrough_audio_history_for_priming(output_spec)
            .is_empty()
    );
}

#[test]
fn passthrough_audio_history_with_mismatched_spec_is_not_used_for_priming() {
    let mut pipeline = PlaybackPipeline::default();
    let stereo_spec = audio_output_spec_for_tests(48_000, 2);
    let same_count_unknown_layout = audio_core::AudioOutputSpec::new(
        48_000,
        audio_core::AudioChannelLayout::discrete(2).unwrap(),
    );
    pipeline.record_passthrough_audio_history(&[1.0, 2.0, 3.0, 4.0], stereo_spec);

    assert!(
        pipeline
            .take_passthrough_audio_history_for_priming(same_count_unknown_layout)
            .is_empty(),
        "история чужого PCM format не должна праймить processor"
    );
}

#[test]
fn seek_clock_reset_restores_base_and_clears_fallback_sample_window() {
    let mut pipeline = PlaybackPipeline::default();
    let anchored_at = Instant::now();
    let target_position = Duration::from_secs(9);

    pipeline.set_media_clock_base(Duration::from_secs(2));
    pipeline.start_monotonic_media_clock(Duration::from_secs(4), anchored_at, PlaybackRate::NORMAL);
    pipeline.reset_audio_clock_sample(Duration::from_secs(3), anchored_at);

    pipeline.reset_clocks_for_seek(target_position);

    assert_eq!(pipeline.media_clock_base(), target_position);
    assert!(pipeline.monotonic_media_position(anchored_at).is_none());
    assert!(pipeline.audio_clock_stalled_for(Instant::now()) < Duration::from_secs(1));
}

#[test]
fn stalled_audio_duration_is_measured_from_last_changed_sample() {
    let mut pipeline = PlaybackPipeline::default();
    let first_observed_at = Instant::now();
    let unchanged_observed_at = first_observed_at + Duration::from_millis(20);
    let changed_observed_at = first_observed_at + Duration::from_millis(40);

    pipeline.reset_audio_clock_sample(Duration::ZERO, first_observed_at);
    pipeline.note_audio_clock_sample(Duration::ZERO, unchanged_observed_at);

    assert_eq!(
        pipeline.audio_clock_stalled_for(first_observed_at + Duration::from_millis(30)),
        Duration::from_millis(30)
    );

    pipeline.note_audio_clock_sample(Duration::from_millis(5), changed_observed_at);

    assert_eq!(
        pipeline.audio_clock_stalled_for(changed_observed_at + Duration::from_millis(15)),
        Duration::from_millis(15)
    );
}
