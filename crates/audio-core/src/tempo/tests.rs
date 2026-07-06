use super::{
    AudioTempoChannelCount, AudioTempoDecodedMedia, AudioTempoFrameCount,
    AudioTempoOutputProgressMapping, AudioTempoPcmFormat, AudioTempoProcessReport,
    AudioTempoProcessor, AudioTempoProcessorConfig, AudioTempoProcessorError,
    AudioTempoProcessorFactory, AudioTempoProcessorHandle, AudioTempoRatio,
    AudioTempoReportFrameCounts, AudioTempoSampleRateHz, AudioTempoSegment, AudioTempoSegmentId,
    AudioTempoStretchedOutput,
};
use anyhow::Result;
use std::time::Duration;

#[derive(Debug)]
struct FakeTempoProcessor {
    pcm_format: AudioTempoPcmFormat,
    segment: AudioTempoSegment,
    pending_processor_output: AudioTempoFrameCount,
    processor_latency: AudioTempoFrameCount,
    process_error: Option<AudioTempoProcessorError>,
}

impl FakeTempoProcessor {
    fn new(config: AudioTempoProcessorConfig) -> Self {
        Self {
            pcm_format: config.pcm_format(),
            segment: config.initial_segment(),
            pending_processor_output: AudioTempoFrameCount::ZERO,
            processor_latency: AudioTempoFrameCount::ZERO,
            process_error: None,
        }
    }

    fn with_internal_state(
        mut self,
        pending_processor_output: AudioTempoFrameCount,
        processor_latency: AudioTempoFrameCount,
    ) -> Self {
        self.pending_processor_output = pending_processor_output;
        self.processor_latency = processor_latency;
        self
    }

    fn with_process_error(mut self, process_error: AudioTempoProcessorError) -> Self {
        self.process_error = Some(process_error);
        self
    }

    fn report_for_operation(
        &self,
        consumed_decoded_media: AudioTempoFrameCount,
        produced_stretched_output: AudioTempoFrameCount,
    ) -> AudioTempoProcessReport {
        AudioTempoProcessReport::from_frame_counts(
            self.pcm_format,
            self.segment,
            AudioTempoReportFrameCounts {
                consumed_decoded_media,
                produced_stretched_output,
                pending_processor_output: self.pending_processor_output,
                processor_latency: self.processor_latency,
            },
        )
    }

    fn produced_frames_for(
        &self,
        consumed_decoded_media: AudioTempoFrameCount,
    ) -> AudioTempoFrameCount {
        if self.segment.ratio().is_normal() {
            return consumed_decoded_media;
        }

        AudioTempoFrameCount::new(
            (consumed_decoded_media.get() as f64 / self.segment.ratio().as_f64()).round() as u64,
        )
    }

    fn output_samples_for(
        &self,
        decoded_media: AudioTempoDecodedMedia<'_>,
        produced_stretched_output: AudioTempoFrameCount,
    ) -> Vec<f32> {
        if self.segment.ratio().is_normal() {
            return decoded_media.interleaved_samples().to_vec();
        }

        let output_sample_count = produced_stretched_output
            .interleaved_sample_len(self.pcm_format.channel_count())
            .expect("fake output frame count should fit usize");
        vec![0.0; output_sample_count]
    }
}

impl AudioTempoProcessor for FakeTempoProcessor {
    fn set_segment(&mut self, segment: AudioTempoSegment) -> Result<AudioTempoProcessReport> {
        self.segment = segment;
        Ok(self.report_for_operation(AudioTempoFrameCount::ZERO, AudioTempoFrameCount::ZERO))
    }

    fn process_decoded_media(
        &mut self,
        decoded_media: AudioTempoDecodedMedia<'_>,
    ) -> Result<AudioTempoStretchedOutput> {
        if let Some(process_error) = self.process_error.take() {
            return Err(process_error.into());
        }

        let consumed_decoded_media = decoded_media.frame_count();
        let produced_stretched_output = self.produced_frames_for(consumed_decoded_media);
        let report = self.report_for_operation(consumed_decoded_media, produced_stretched_output);
        let interleaved_samples = self.output_samples_for(decoded_media, produced_stretched_output);

        AudioTempoStretchedOutput::new(interleaved_samples, report, self.pcm_format)
    }

    fn flush(&mut self) -> Result<AudioTempoStretchedOutput> {
        let produced_stretched_output = self.pending_processor_output;
        self.pending_processor_output = AudioTempoFrameCount::ZERO;

        let report =
            self.report_for_operation(AudioTempoFrameCount::ZERO, produced_stretched_output);
        let output_sample_count = produced_stretched_output
            .interleaved_sample_len(self.pcm_format.channel_count())
            .expect("fake output frame count should fit usize");

        AudioTempoStretchedOutput::new(vec![0.0; output_sample_count], report, self.pcm_format)
    }

    fn reset(&mut self) -> Result<AudioTempoProcessReport> {
        self.pending_processor_output = AudioTempoFrameCount::ZERO;
        self.processor_latency = AudioTempoFrameCount::ZERO;

        Ok(self.report_for_operation(AudioTempoFrameCount::ZERO, AudioTempoFrameCount::ZERO))
    }
}

struct FailingTempoProcessorFactory {
    error: AudioTempoProcessorError,
}

impl AudioTempoProcessorFactory for FailingTempoProcessorFactory {
    fn create_processor(
        &self,
        _config: AudioTempoProcessorConfig,
    ) -> Result<AudioTempoProcessorHandle> {
        Err(self.error.clone().into())
    }
}

fn stereo_48k_format() -> AudioTempoPcmFormat {
    AudioTempoPcmFormat::new(
        AudioTempoSampleRateHz::new(48_000).expect("sample rate should be valid"),
        AudioTempoChannelCount::new(2).expect("channel count should be valid"),
    )
}

fn config_for_ratio(ratio: AudioTempoRatio) -> AudioTempoProcessorConfig {
    AudioTempoProcessorConfig::new(
        stereo_48k_format(),
        AudioTempoSegment::new(AudioTempoSegmentId::new(7), ratio),
    )
}

fn decoded_media_for_frames(interleaved_samples: &[f32]) -> AudioTempoDecodedMedia<'_> {
    AudioTempoDecodedMedia::from_interleaved_samples(interleaved_samples, stereo_48k_format())
        .expect("fake samples should describe whole PCM frames")
}

#[test]
fn passthrough_ratio_preserves_samples_and_equal_media_output_durations() {
    let mut processor = FakeTempoProcessor::new(config_for_ratio(AudioTempoRatio::NORMAL));
    let samples = vec![0.25; 960];

    let output = processor
        .process_decoded_media(decoded_media_for_frames(&samples))
        .expect("fake 1x process should succeed");

    assert_eq!(output.interleaved_samples(), samples.as_slice());
    assert_eq!(
        output.report().consumed_decoded_media().frame_count(),
        AudioTempoFrameCount::new(480)
    );
    assert_eq!(
        output.report().produced_stretched_output().frame_count(),
        AudioTempoFrameCount::new(480)
    );
    assert_eq!(
        output.report().consumed_decoded_media().duration(),
        Duration::from_millis(10)
    );
    assert_eq!(
        output.report().produced_stretched_output().duration(),
        Duration::from_millis(10)
    );
    assert_eq!(
        output.report().pending_processor_output().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert_eq!(
        output.report().processor_latency().frame_count(),
        AudioTempoFrameCount::ZERO
    );
}

#[test]
fn reset_clears_buffered_output_and_processor_latency_state() {
    let mut processor = FakeTempoProcessor::new(config_for_ratio(AudioTempoRatio::NORMAL))
        .with_internal_state(
            AudioTempoFrameCount::new(120),
            AudioTempoFrameCount::new(24),
        );

    let report = processor.reset().expect("fake reset should succeed");

    assert_eq!(
        report.pending_processor_output().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert_eq!(
        report.processor_latency().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert_eq!(
        report.consumed_decoded_media().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert_eq!(
        report.produced_stretched_output().frame_count(),
        AudioTempoFrameCount::ZERO
    );
}

#[test]
fn flush_drains_buffered_output_without_consuming_decoded_media() {
    let pending_processor_output = AudioTempoFrameCount::new(120);
    let mut processor = FakeTempoProcessor::new(config_for_ratio(AudioTempoRatio::NORMAL))
        .with_internal_state(pending_processor_output, AudioTempoFrameCount::new(24));

    let output = processor.flush().expect("fake flush should succeed");
    let second_output = processor
        .flush()
        .expect("second fake flush should see drained pending output");

    assert_eq!(
        output.report().consumed_decoded_media().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert_eq!(
        output.report().produced_stretched_output().frame_count(),
        pending_processor_output
    );
    assert_eq!(
        output.report().produced_stretched_output().duration(),
        Duration::from_millis(2) + Duration::from_micros(500)
    );
    assert_eq!(
        second_output
            .report()
            .produced_stretched_output()
            .frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert!(second_output.interleaved_samples().is_empty());
}

#[test]
fn processor_and_factory_errors_stay_typed_for_downcast() {
    let requested_ratio = AudioTempoRatio::new(4.0).expect("ratio should be valid");
    let factory_error = AudioTempoProcessorError::UnsupportedRatio { requested_ratio };
    let factory = FailingTempoProcessorFactory {
        error: factory_error.clone(),
    };

    let error = match factory.create_processor(config_for_ratio(requested_ratio)) {
        Ok(_) => panic!("factory should reject unsupported ratio"),
        Err(error) => error,
    };

    assert_eq!(
        error
            .downcast_ref::<AudioTempoProcessorError>()
            .expect("factory error should stay typed"),
        &factory_error
    );

    let process_error = AudioTempoProcessorError::BackendFailure {
        message: "synthetic processor failure".to_string(),
    };
    let mut processor = FakeTempoProcessor::new(config_for_ratio(AudioTempoRatio::NORMAL))
        .with_process_error(process_error.clone());
    let samples = vec![0.0; 96];

    let error = processor
        .process_decoded_media(decoded_media_for_frames(&samples))
        .expect_err("processor should return the synthetic typed error");

    assert_eq!(
        error
            .downcast_ref::<AudioTempoProcessorError>()
            .expect("processor error should stay typed"),
        &process_error
    );
}

#[test]
fn non_normal_ratio_distinguishes_consumed_media_from_produced_output_duration() {
    let double_speed = AudioTempoRatio::new(2.0).expect("ratio should be valid");
    let mut processor = FakeTempoProcessor::new(config_for_ratio(double_speed));
    let samples = vec![1.0; 960];

    let output = processor
        .process_decoded_media(decoded_media_for_frames(&samples))
        .expect("fake 2x process should succeed");

    assert_eq!(
        output.report().consumed_decoded_media().frame_count(),
        AudioTempoFrameCount::new(480)
    );
    assert_eq!(
        output.report().produced_stretched_output().frame_count(),
        AudioTempoFrameCount::new(240)
    );
    assert_eq!(
        output.report().consumed_decoded_media().duration(),
        Duration::from_millis(10)
    );
    assert_eq!(
        output.report().produced_stretched_output().duration(),
        Duration::from_millis(5)
    );
    assert_ne!(
        output.report().consumed_decoded_media().duration(),
        output.report().produced_stretched_output().duration()
    );
}

#[test]
fn set_segment_updates_ratio_without_consuming_or_producing_frames() {
    let initial_ratio = AudioTempoRatio::NORMAL;
    let updated_ratio = AudioTempoRatio::new(0.5).expect("ratio should be valid");
    let updated_segment = AudioTempoSegment::new(AudioTempoSegmentId::new(8), updated_ratio);
    let mut processor = FakeTempoProcessor::new(config_for_ratio(initial_ratio));

    let report = processor
        .set_segment(updated_segment)
        .expect("fake segment update should succeed");

    assert_eq!(report.segment_id(), updated_segment.segment_id());
    assert_eq!(report.effective_ratio(), updated_ratio);
    assert_eq!(
        report.consumed_decoded_media().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert_eq!(
        report.produced_stretched_output().frame_count(),
        AudioTempoFrameCount::ZERO
    );
}

#[test]
fn report_keeps_processor_pending_and_latency_separate_from_output_device_tail() {
    let output_device_tail_after_write =
        stereo_48k_format().frame_span(AudioTempoFrameCount::new(960));
    let processor_pending = AudioTempoFrameCount::new(120);
    let processor_latency = AudioTempoFrameCount::new(48);
    let mut processor = FakeTempoProcessor::new(config_for_ratio(AudioTempoRatio::NORMAL))
        .with_internal_state(processor_pending, processor_latency);
    let samples = vec![0.5; 960];

    let output = processor
        .process_decoded_media(decoded_media_for_frames(&samples))
        .expect("fake process should succeed");

    assert_eq!(
        output.report().pending_processor_output().frame_count(),
        processor_pending
    );
    assert_eq!(
        output.report().processor_latency().frame_count(),
        processor_latency
    );
    assert_ne!(
        output.report().pending_processor_output().duration(),
        output_device_tail_after_write.duration()
    );
    assert_ne!(
        output.report().processor_latency().duration(),
        output_device_tail_after_write.duration()
    );
}

#[test]
fn report_exposes_mapping_data_for_output_progress_to_media_progress() {
    let double_speed = AudioTempoRatio::new(2.0).expect("ratio should be valid");
    let mut processor = FakeTempoProcessor::new(config_for_ratio(double_speed));
    let samples = vec![0.0; 960];

    let output = processor
        .process_decoded_media(decoded_media_for_frames(&samples))
        .expect("fake 2x process should succeed");
    let mapping: AudioTempoOutputProgressMapping = output.report().output_progress_mapping();

    assert_eq!(output.report().segment_id(), AudioTempoSegmentId::new(7));
    assert_eq!(output.report().effective_ratio(), double_speed);
    assert_eq!(mapping.segment_id(), AudioTempoSegmentId::new(7));
    assert_eq!(mapping.effective_ratio(), double_speed);
    assert_eq!(
        mapping.consumed_decoded_media().duration(),
        Duration::from_millis(10)
    );
    assert_eq!(
        mapping.produced_stretched_output().duration(),
        Duration::from_millis(5)
    );
}
