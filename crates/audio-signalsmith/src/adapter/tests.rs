use audio_core::{
    AudioTempoChannelCount, AudioTempoRatio, AudioTempoSampleRateHz, AudioTempoSegmentId,
};

use super::*;

#[path = "tests/content.rs"]
mod content;

/// Общий mono format делает анализ EOF samples однозначным.
fn mono_48k_format() -> AudioTempoPcmFormat {
    AudioTempoPcmFormat::new(
        AudioTempoSampleRateHz::new(48_000).expect("valid rate"),
        AudioTempoChannelCount::new(1).expect("valid channels"),
    )
}

/// Создаёт processor с уникальным initial segment id.
fn processor_at_rate(rate: f64) -> SignalsmithTempoProcessor {
    let segment = AudioTempoSegment::new(
        AudioTempoSegmentId::new(1),
        AudioTempoRatio::new(rate).expect("valid ratio"),
    );
    SignalsmithTempoProcessor::new(AudioTempoProcessorConfig::new(mono_48k_format(), segment))
}

/// Обрабатывает packet и копирует output до следующего mutable borrow processor-а.
fn process_packet(
    processor: &mut SignalsmithTempoProcessor,
    samples: &[f32],
    output_buffer: &mut Vec<f32>,
) -> (Vec<f32>, AudioTempoProcessReport) {
    let decoded = AudioTempoDecodedMedia::from_interleaved_samples(samples, mono_48k_format())
        .expect("frame-aligned packet");
    let output = processor
        .process_decoded_media_into(decoded, output_buffer)
        .expect("process should succeed");
    (
        output.interleaved_samples().to_vec(),
        output.report().clone(),
    )
}

#[test]
fn boundary_rates_keep_fractional_output_accounting_bounded() {
    for rate in [0.25, 0.5, 1.0, 2.0, 4.0] {
        let mut processor = processor_at_rate(rate);
        let packet = vec![0.25f32; 997];
        let mut output_buffer = Vec::new();
        let mut produced_frames = 0u64;
        for _ in 0..113 {
            let (_, report) = process_packet(&mut processor, &packet, &mut output_buffer);
            produced_frames += report.produced_stretched_output().frame_count().get();
        }

        let consumed_frames = 997u64 * 113;
        let expected_frames = (consumed_frames as f64 / rate).floor() as u64;
        assert_eq!(produced_frames, expected_frames, "rate={rate}");
    }
}

#[test]
fn fractional_output_carry_survives_segment_transition() {
    let pcm_format = mono_48k_format();
    let first_segment = AudioTempoSegment::new(
        AudioTempoSegmentId::new(41),
        AudioTempoRatio::new(2.0).expect("valid ratio"),
    );
    let second_segment = AudioTempoSegment::new(
        AudioTempoSegmentId::new(42),
        AudioTempoRatio::new(2.0).expect("valid ratio"),
    );
    let mut timeline = SignalsmithTimelineState::default();
    let mut first_input_span = AudioTempoOutputSegmentSpans::default();
    first_input_span.push(audio_core::AudioTempoOutputSegmentSpan::new(
        pcm_format,
        first_segment,
        AudioTempoFrameCount::new(1),
    ));
    let mut second_input_span = AudioTempoOutputSegmentSpans::default();
    second_input_span.push(audio_core::AudioTempoOutputSegmentSpan::new(
        pcm_format,
        second_segment,
        AudioTempoFrameCount::new(1),
    ));

    let (_, first_output) = timeline
        .schedule_processing_spans(&first_input_span, pcm_format)
        .expect("first span should schedule");
    let (_, second_output) = timeline
        .schedule_processing_spans(&second_input_span, pcm_format)
        .expect("second span should schedule");

    assert_eq!(sum_public_span_frames(&first_output).unwrap(), 0);
    assert_eq!(
        sum_public_span_frames(&second_output).unwrap(),
        1,
        "two half-frames across a segment boundary must become one output frame"
    );
}

#[test]
fn history_seek_primes_dsp_without_output_or_pending_accounting() {
    let mut processor = processor_at_rate(2.0);
    let history_samples = vec![0.25f32; 12_000];
    let history =
        AudioTempoDecodedMedia::from_interleaved_samples(&history_samples, mono_48k_format())
            .expect("valid history");

    let prime_report = processor
        .prime_decoded_history(history)
        .expect("fresh processor should accept history");

    assert!(processor.history_primed);
    assert_eq!(
        prime_report.consumed_decoded_media().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert_eq!(
        prime_report.produced_stretched_output().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert_eq!(
        prime_report.pending_processor_output().frame_count(),
        AudioTempoFrameCount::ZERO
    );

    let real_packet = vec![0.5f32; 960];
    let mut output_buffer = Vec::new();
    let (_, first_report) = process_packet(&mut processor, &real_packet, &mut output_buffer);
    assert_eq!(
        first_report.consumed_decoded_media().frame_count().get(),
        960
    );
    assert_eq!(
        first_report.produced_stretched_output().frame_count().get(),
        480
    );
    assert_eq!(
        first_report
            .output_progress_mapping()
            .produced_output_segments()
            .iter()
            .map(|span| span.stretched_output().frame_count().get())
            .sum::<u64>(),
        480,
        "warmup history must not be counted as first real output"
    );
}

#[test]
fn history_prime_after_stream_start_is_typed_and_non_mutating() {
    let mut processor = processor_at_rate(1.0);
    let mut output_buffer = Vec::new();
    let (_, first_report) = process_packet(&mut processor, &vec![0.25; 960], &mut output_buffer);
    let pending_before = first_report.pending_processor_output().frame_count();
    let history_samples = vec![0.5f32; 960];
    let history =
        AudioTempoDecodedMedia::from_interleaved_samples(&history_samples, mono_48k_format())
            .expect("valid history");

    let error = processor
        .prime_decoded_history(history)
        .expect_err("mid-stream history prime must fail");

    assert_eq!(
        error.downcast_ref::<AudioTempoProcessorError>(),
        Some(&AudioTempoProcessorError::HistoryPrimeAfterStreamStart)
    );
    let no_op_report = processor
        .set_segment(processor.active_segment)
        .expect("state remains usable");
    assert_eq!(
        no_op_report.pending_processor_output().frame_count(),
        pending_before
    );
}

#[test]
fn eof_returns_input_latency_process_and_output_latency_flush_as_one_result() {
    let mut processor = processor_at_rate(2.0);
    let input = vec![0.25f32; 12_000];
    let mut output_buffer = Vec::new();
    let (_, process_report) = process_packet(&mut processor, &input, &mut output_buffer);
    let pending_before_finish = process_report
        .pending_processor_output()
        .frame_count()
        .get();

    let output = processor
        .finish_stream_into(&mut output_buffer)
        .expect("finish should succeed");

    assert_eq!(
        output
            .report()
            .produced_stretched_output()
            .frame_count()
            .get(),
        pending_before_finish
    );
    assert_eq!(
        output.report().pending_processor_output().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert!(
        output.report().input_latency().frame_count().get() > 0,
        "static input latency remains observable after finish"
    );
    assert!(
        output.report().output_latency().frame_count().get() > 0,
        "static output latency remains observable after finish"
    );
}

#[test]
fn reset_and_finished_stream_report_zero_actual_pending_without_hiding_latencies() {
    let mut processor = processor_at_rate(1.0);
    let mut output_buffer = Vec::new();
    process_packet(&mut processor, &vec![0.5; 4_800], &mut output_buffer);

    let reset_report = processor.reset().expect("reset should succeed");
    assert_eq!(
        reset_report.pending_processor_output().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert!(reset_report.input_latency().frame_count().get() > 0);
    assert!(reset_report.output_latency().frame_count().get() > 0);

    let repeated_finish = processor
        .finish_stream_into(&mut output_buffer)
        .expect("empty finish should succeed");
    assert!(repeated_finish.interleaved_samples().is_empty());
    assert_eq!(
        repeated_finish
            .report()
            .pending_processor_output()
            .frame_count(),
        AudioTempoFrameCount::ZERO
    );
}

#[test]
fn reset_drops_previous_dsp_samples_without_leaking_into_new_stream() {
    let mut processor = processor_at_rate(0.5);
    let mut output_buffer = Vec::new();
    process_packet(&mut processor, &vec![0.9; 8_000], &mut output_buffer);
    processor.reset().expect("reset should succeed");

    let zeros = vec![0.0f32; 8_000];
    let (mut new_stream_output, _) = process_packet(&mut processor, &zeros, &mut output_buffer);
    let tail = processor
        .finish_stream_into(&mut output_buffer)
        .expect("new zero stream should finish");
    new_stream_output.extend_from_slice(tail.interleaved_samples());
    let leaked_peak = new_stream_output
        .iter()
        .copied()
        .map(f32::abs)
        .fold(0.0f32, f32::max);

    assert!(
        leaked_peak <= f32::EPSILON,
        "reset leaked old DSP samples into new stream: peak={leaked_peak}"
    );
}

#[test]
fn empty_packet_preserves_existing_actual_pending_tail() {
    let mut processor = processor_at_rate(2.0);
    let mut output_buffer = Vec::new();
    let (_, process_report) =
        process_packet(&mut processor, &vec![0.25; 4_800], &mut output_buffer);
    let pending_before = process_report.pending_processor_output().frame_count();

    let empty = AudioTempoDecodedMedia::from_interleaved_samples(&[], mono_48k_format())
        .expect("empty packet has a valid format");
    let output = processor
        .process_decoded_media_into(empty, &mut output_buffer)
        .expect("empty packet should be a no-op");

    assert!(output.interleaved_samples().is_empty());
    assert_eq!(
        output.report().pending_processor_output().frame_count(),
        pending_before
    );
    assert!(
        !output
            .report()
            .output_progress_mapping()
            .pending_output_segments()
            .is_empty()
    );
}

#[test]
fn segment_change_keeps_old_dsp_tail_and_orders_transition_output() {
    let mut processor = processor_at_rate(1.0);
    let packet = vec![0.25f32; 960];
    let mut output_buffer = Vec::new();
    process_packet(&mut processor, &packet, &mut output_buffer);
    let old_segment = processor.active_segment;
    let new_segment = AudioTempoSegment::new(
        AudioTempoSegmentId::new(2),
        AudioTempoRatio::new(4.0).expect("valid ratio"),
    );

    let change_report = processor
        .set_segment(new_segment)
        .expect("segment change should succeed");
    let pending_at_change = change_report
        .output_progress_mapping()
        .pending_output_segments();
    assert!(!pending_at_change.is_empty());
    assert_eq!(pending_at_change[0].segment(), old_segment);

    let mut saw_new_segment = false;
    for _ in 0..16 {
        let (_, report) = process_packet(&mut processor, &packet, &mut output_buffer);
        let produced = report.output_progress_mapping().produced_output_segments();
        let old_index = produced
            .iter()
            .position(|span| span.segment() == old_segment);
        let new_index = produced
            .iter()
            .position(|span| span.segment() == new_segment);
        if let Some(new_index) = new_index {
            saw_new_segment = true;
            if let Some(old_index) = old_index {
                assert!(old_index < new_index, "old tail must precede new segment");
            }
        }
    }
    assert!(
        saw_new_segment,
        "new segment must emerge after latency tail"
    );
}

#[test]
fn multiple_fast_rate_changes_preserve_pending_segment_order() {
    let mut processor = processor_at_rate(1.0);
    let mut output_buffer = Vec::new();
    let packet = vec![0.25f32; 240];
    process_packet(&mut processor, &packet, &mut output_buffer);

    for (id, rate) in [(2, 2.0), (3, 0.5), (4, 4.0)] {
        let segment = AudioTempoSegment::new(
            AudioTempoSegmentId::new(id),
            AudioTempoRatio::new(rate).expect("valid ratio"),
        );
        processor
            .set_segment(segment)
            .expect("segment change should succeed");
        process_packet(&mut processor, &packet, &mut output_buffer);
    }

    let report = processor
        .set_segment(AudioTempoSegment::new(
            AudioTempoSegmentId::new(5),
            AudioTempoRatio::NORMAL,
        ))
        .expect("final segment change should succeed");
    let ids: Vec<u64> = report
        .output_progress_mapping()
        .pending_output_segments()
        .iter()
        .map(|span| span.segment().segment_id().get())
        .collect();
    assert!(ids.windows(2).all(|pair| pair[0] <= pair[1]), "ids={ids:?}");
    assert!(ids.contains(&1));
    assert!(ids.contains(&2));
    assert!(ids.contains(&3));
    assert!(ids.contains(&4));
}

#[test]
fn format_mismatch_preserves_processor_state_and_caller_buffer() {
    let mut processor = processor_at_rate(1.0);
    let stereo_format = AudioTempoPcmFormat::new(
        AudioTempoSampleRateHz::new(48_000).expect("valid rate"),
        AudioTempoChannelCount::new(2).expect("valid channels"),
    );
    let decoded = AudioTempoDecodedMedia::from_interleaved_samples(&[0.0, 0.0], stereo_format)
        .expect("valid stereo packet");
    let mut output_buffer = vec![7.0, 8.0];

    let error = processor
        .process_decoded_media_into(decoded, &mut output_buffer)
        .expect_err("format mismatch must fail");

    assert!(matches!(
        error.downcast_ref::<AudioTempoProcessorError>(),
        Some(AudioTempoProcessorError::PcmFormatMismatch { .. })
    ));
    assert_eq!(output_buffer, [7.0, 8.0]);
    assert!(!processor.timeline_state.has_decoded_media);
}

#[test]
fn caller_output_allocation_is_reused_between_packets() {
    let mut processor = processor_at_rate(0.25);
    let packet = vec![0.25f32; 960];
    let mut output_buffer = Vec::with_capacity(8_000);
    let allocation_address = output_buffer.as_ptr();

    process_packet(&mut processor, &packet, &mut output_buffer);
    assert_eq!(output_buffer.as_ptr(), allocation_address);
    process_packet(&mut processor, &packet, &mut output_buffer);
    assert_eq!(output_buffer.as_ptr(), allocation_address);
}
