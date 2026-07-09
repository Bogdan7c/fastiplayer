//! Waveform-level regression tests ordered Signalsmith automation и EOF.

use signalsmith_stretch::Stretch;

use super::*;

/// Детерминированный multi-tone + impulses делает time mapping слышимым в тесте.
fn transition_waveform(start_frame: usize, frame_count: usize) -> Vec<f32> {
    (0..frame_count)
        .map(|offset| {
            let frame = start_frame + offset;
            let time = frame as f32 / 48_000.0;
            let base = 0.35 * (std::f32::consts::TAU * 437.0 * time).sin();
            let overtone = 0.2 * (std::f32::consts::TAU * 1_733.0 * time).sin();
            let impulse = if frame
                .checked_rem(997)
                .is_some_and(|remainder| remainder == 0)
            {
                0.4
            } else {
                0.0
            };
            base + overtone + impulse
        })
        .collect()
}

/// Выполняет raw backend call и возвращает его waveform.
fn raw_process(stretch: &mut Stretch, input: &[f32], output_frames: usize) -> Vec<f32> {
    let mut output = vec![0.0f32; output_frames];
    stretch.process(input, &mut output);
    output
}

/// Нормированная waveform error не зависит от общей громкости reference-а.
fn normalized_waveform_error(actual: &[f32], reference: &[f32]) -> f64 {
    assert_eq!(actual.len(), reference.len());
    let (difference_energy, reference_energy) = actual.iter().zip(reference).fold(
        (0.0f64, 0.0f64),
        |(difference_sum, reference_sum), (&actual_sample, &reference_sample)| {
            let difference = f64::from(actual_sample - reference_sample);
            (
                difference_sum + difference * difference,
                reference_sum + f64::from(reference_sample) * f64::from(reference_sample),
            )
        },
    );
    (difference_energy / reference_energy.max(f64::EPSILON)).sqrt()
}

/// Подготавливает raw reference теми же single-segment calls до mixed boundary.
fn prepare_rapid_transition_reference(stretch: &mut Stretch, input_latency: usize) -> usize {
    let quarter_latency = input_latency / 4;
    let initial = transition_waveform(0, input_latency);
    let first_fast_packet = transition_waveform(input_latency, quarter_latency);
    let slow_packet = transition_waveform(input_latency + quarter_latency, quarter_latency);
    let _ = raw_process(stretch, &initial, input_latency);
    let _ = raw_process(stretch, &first_fast_packet, quarter_latency);
    let _ = raw_process(stretch, &slow_packet, quarter_latency);
    input_latency + quarter_latency * 2
}

#[test]
fn eof_preserves_energy_from_final_impulse_and_ramp_samples() {
    for rate in [0.25, 0.5, 1.0, 2.0, 4.0] {
        let mut baseline_processor = processor_at_rate(rate);
        let mut final_samples_processor = processor_at_rate(rate);
        let baseline_input = vec![0.0f32; 24_000];
        let mut final_samples_input = baseline_input.clone();
        let protected_final_frames =
            (final_samples_processor.input_latency_frames.get() as usize).min(4_800);
        let final_region_start = final_samples_input.len() - protected_final_frames;
        for (index, sample) in final_samples_input[final_region_start..]
            .iter_mut()
            .enumerate()
        {
            *sample = 0.2 + 0.6 * index as f32 / protected_final_frames as f32;
        }
        // Отдельный последний frame доказывает, что boundary не обрезает самый конец.
        *final_samples_input
            .last_mut()
            .expect("test input is non-empty") += 1.0;

        let mut baseline_buffer = Vec::new();
        let mut final_samples_buffer = Vec::new();
        let (baseline_stream, _) = process_packet(
            &mut baseline_processor,
            &baseline_input,
            &mut baseline_buffer,
        );
        let (final_samples_stream, _) = process_packet(
            &mut final_samples_processor,
            &final_samples_input,
            &mut final_samples_buffer,
        );
        let baseline_tail = baseline_processor
            .finish_stream_into(&mut baseline_buffer)
            .expect("baseline finish should succeed")
            .interleaved_samples()
            .to_vec();
        let final_samples_tail = final_samples_processor
            .finish_stream_into(&mut final_samples_buffer)
            .expect("final-samples finish should succeed")
            .interleaved_samples()
            .to_vec();
        let difference_energy = baseline_tail
            .iter()
            .zip(&final_samples_tail)
            .map(|(&baseline, &with_final_samples)| {
                let difference = f64::from(with_final_samples - baseline);
                difference * difference
            })
            .sum::<f64>();
        let streaming_difference_energy = baseline_stream
            .iter()
            .zip(&final_samples_stream)
            .map(|(&baseline, &with_final_samples)| {
                let difference = f64::from(with_final_samples - baseline);
                difference * difference
            })
            .sum::<f64>();

        assert_eq!(baseline_tail.len(), final_samples_tail.len());
        assert!(
            difference_energy > 1.0e-4,
            "rate={rate}: EOF tail lost isolated final samples, tail_difference={difference_energy:e}, streaming_difference={streaming_difference_energy:e}"
        );
    }
}

#[test]
fn prime_then_finish_resets_lifecycle_and_reuse_matches_fresh_processor() {
    let mut reused_processor = processor_at_rate(1.0);
    let history_samples = transition_waveform(0, 12_000);
    let history =
        AudioTempoDecodedMedia::from_interleaved_samples(&history_samples, mono_48k_format())
            .expect("valid history");
    reused_processor
        .prime_decoded_history(history)
        .expect("history prime should succeed");
    let mut reused_buffer = Vec::new();

    let terminal_finish = reused_processor
        .finish_stream_into(&mut reused_buffer)
        .expect("prime-only stream should finish");

    assert!(terminal_finish.interleaved_samples().is_empty());
    assert!(!reused_processor.history_primed);
    assert!(!reused_processor.timeline_state.has_decoded_media);

    let packet = transition_waveform(12_000, 9_600);
    let (reused_stream_output, reused_report) =
        process_packet(&mut reused_processor, &packet, &mut reused_buffer);
    let reused_tail = reused_processor
        .finish_stream_into(&mut reused_buffer)
        .expect("reused processor should finish")
        .interleaved_samples()
        .to_vec();

    let mut fresh_processor = processor_at_rate(1.0);
    let mut fresh_buffer = Vec::new();
    let (fresh_stream_output, fresh_report) =
        process_packet(&mut fresh_processor, &packet, &mut fresh_buffer);
    let fresh_tail = fresh_processor
        .finish_stream_into(&mut fresh_buffer)
        .expect("fresh processor should finish")
        .interleaved_samples()
        .to_vec();

    assert_eq!(
        reused_report.produced_stretched_output(),
        fresh_report.produced_stretched_output()
    );
    assert_eq!(
        reused_report.pending_processor_output(),
        fresh_report.pending_processor_output()
    );
    assert!(normalized_waveform_error(&reused_stream_output, &fresh_stream_output) < 1.0e-6);
    assert!(normalized_waveform_error(&reused_tail, &fresh_tail) < 1.0e-6);
}

#[test]
fn rapid_rate_change_waveform_matches_ordered_calls_not_weighted_average() {
    let mut processor = processor_at_rate(1.0);
    let input_latency = processor.input_latency_frames.get() as usize;
    assert_eq!(
        input_latency % 4,
        0,
        "test geometry needs quarter latency chunks"
    );
    let quarter_latency = input_latency / 4;
    let initial = transition_waveform(0, input_latency);
    let first_fast_packet = transition_waveform(input_latency, quarter_latency);
    let slow_packet = transition_waveform(input_latency + quarter_latency, quarter_latency);
    let final_start = input_latency + quarter_latency * 2;
    let mixed_packet = transition_waveform(final_start, input_latency * 2);
    let mut output_buffer = Vec::new();

    process_packet(&mut processor, &initial, &mut output_buffer);
    processor
        .set_segment(AudioTempoSegment::new(
            AudioTempoSegmentId::new(2),
            AudioTempoRatio::new(2.0).expect("valid ratio"),
        ))
        .expect("first rate change");
    process_packet(&mut processor, &first_fast_packet, &mut output_buffer);
    processor
        .set_segment(AudioTempoSegment::new(
            AudioTempoSegmentId::new(3),
            AudioTempoRatio::new(0.5).expect("valid ratio"),
        ))
        .expect("second rate change");
    process_packet(&mut processor, &slow_packet, &mut output_buffer);
    processor
        .set_segment(AudioTempoSegment::new(
            AudioTempoSegmentId::new(4),
            AudioTempoRatio::new(2.0).expect("valid ratio"),
        ))
        .expect("third rate change");
    let (actual, report) = process_packet(&mut processor, &mixed_packet, &mut output_buffer);

    // Processing queue: old 1x half, 2x quarter, 0.5x quarter, current 2x full latency.
    let input_chunks = [
        input_latency / 2,
        quarter_latency,
        quarter_latency,
        input_latency,
    ];
    let output_chunks = [
        input_latency / 2,
        quarter_latency / 2,
        quarter_latency * 2,
        input_latency / 2,
    ];
    let mut ordered_reference_backend = Stretch::preset_default(1, 48_000);
    prepare_rapid_transition_reference(&mut ordered_reference_backend, input_latency);
    let mut ordered_reference = Vec::new();
    let mut input_offset = 0usize;
    for (&input_frames, &output_frames) in input_chunks.iter().zip(&output_chunks) {
        let next_input_offset = input_offset + input_frames;
        ordered_reference.extend(raw_process(
            &mut ordered_reference_backend,
            &mixed_packet[input_offset..next_input_offset],
            output_frames,
        ));
        input_offset = next_input_offset;
    }

    let mut weighted_backend = Stretch::preset_default(1, 48_000);
    prepare_rapid_transition_reference(&mut weighted_backend, input_latency);
    let weighted_reference = raw_process(&mut weighted_backend, &mixed_packet, actual.len());
    let ordered_error = normalized_waveform_error(&actual, &ordered_reference);
    let weighted_error = normalized_waveform_error(&actual, &weighted_reference);

    assert_eq!(
        report.produced_stretched_output().frame_count().get() as usize,
        output_chunks.iter().sum::<usize>()
    );
    assert!(
        ordered_error < weighted_error * 0.65,
        "actual waveform must follow ordered automation: ordered_error={ordered_error:.4}, weighted_error={weighted_error:.4}"
    );
}

#[test]
fn eof_mixed_tail_waveform_uses_ordered_silence_calls_before_flush() {
    let mut processor = processor_at_rate(1.0);
    let input_latency = processor.input_latency_frames.get() as usize;
    let output_latency = processor.output_latency_frames.get() as usize;
    assert_eq!(
        input_latency % 4,
        0,
        "test geometry needs quarter latency chunks"
    );
    let quarter_latency = input_latency / 4;
    let initial = transition_waveform(0, input_latency);
    let first_fast_packet = transition_waveform(input_latency, quarter_latency);
    let slow_packet = transition_waveform(input_latency + quarter_latency, quarter_latency);
    let mut output_buffer = Vec::new();

    process_packet(&mut processor, &initial, &mut output_buffer);
    processor
        .set_segment(AudioTempoSegment::new(
            AudioTempoSegmentId::new(2),
            AudioTempoRatio::new(2.0).expect("valid ratio"),
        ))
        .expect("first rate change");
    process_packet(&mut processor, &first_fast_packet, &mut output_buffer);
    processor
        .set_segment(AudioTempoSegment::new(
            AudioTempoSegmentId::new(3),
            AudioTempoRatio::new(0.5).expect("valid ratio"),
        ))
        .expect("second rate change");
    process_packet(&mut processor, &slow_packet, &mut output_buffer);
    let actual = processor
        .finish_stream_into(&mut output_buffer)
        .expect("mixed EOF should finish")
        .interleaved_samples()
        .to_vec();

    // Pending input queue at EOF: old 1x half, 2x quarter, 0.5x quarter.
    let input_chunks = [input_latency / 2, quarter_latency, quarter_latency];
    let output_chunks = [input_latency / 2, quarter_latency / 2, quarter_latency * 2];
    let silence = vec![0.0f32; input_latency];
    let mut ordered_reference_backend = Stretch::preset_default(1, 48_000);
    prepare_rapid_transition_reference(&mut ordered_reference_backend, input_latency);
    let mut ordered_reference = Vec::new();
    let mut silence_offset = 0usize;
    for (&input_frames, &output_frames) in input_chunks.iter().zip(&output_chunks) {
        let next_silence_offset = silence_offset + input_frames;
        ordered_reference.extend(raw_process(
            &mut ordered_reference_backend,
            &silence[silence_offset..next_silence_offset],
            output_frames,
        ));
        silence_offset = next_silence_offset;
    }
    let mut ordered_flush = vec![0.0f32; output_latency];
    ordered_reference_backend.flush(&mut ordered_flush);
    ordered_reference.extend(ordered_flush);

    let mut weighted_backend = Stretch::preset_default(1, 48_000);
    prepare_rapid_transition_reference(&mut weighted_backend, input_latency);
    let processing_output_frames = output_chunks.iter().sum::<usize>();
    let mut weighted_reference =
        raw_process(&mut weighted_backend, &silence, processing_output_frames);
    let mut weighted_flush = vec![0.0f32; output_latency];
    weighted_backend.flush(&mut weighted_flush);
    weighted_reference.extend(weighted_flush);
    let ordered_error = normalized_waveform_error(&actual, &ordered_reference);
    let weighted_error = normalized_waveform_error(&actual, &weighted_reference);

    assert!(
        ordered_error < weighted_error * 0.65,
        "EOF waveform must preserve ordered old segments: ordered_error={ordered_error:.4}, weighted_error={weighted_error:.4}"
    );
}
