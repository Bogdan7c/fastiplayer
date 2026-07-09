use super::{
    AudioTempoChannelCount, AudioTempoDecodedMedia, AudioTempoFrameCount,
    AudioTempoOutputProgressMapping, AudioTempoOutputSegmentSpan, AudioTempoPcmFormat,
    AudioTempoProcessReport, AudioTempoProcessor, AudioTempoProcessorConfig,
    AudioTempoProcessorError, AudioTempoProcessorFactory, AudioTempoProcessorHandle,
    AudioTempoRatio, AudioTempoReportFrameCounts, AudioTempoSampleRateHz, AudioTempoSegment,
    AudioTempoSegmentId, AudioTempoStretchedOutput,
};
use anyhow::Result;

/// Fake хранит ровно те состояния, которые должен различать neutral boundary.
struct FakeTempoProcessor {
    pcm_format: AudioTempoPcmFormat,
    active_segment: AudioTempoSegment,
    pending_segment: AudioTempoSegment,
    pending_processor_output: AudioTempoFrameCount,
    input_latency: AudioTempoFrameCount,
    output_latency: AudioTempoFrameCount,
    process_error: Option<AudioTempoProcessorError>,
    rejected_segment: Option<AudioTempoSegmentId>,
    processed_packet_count: usize,
    primed_history_frames: AudioTempoFrameCount,
}

impl FakeTempoProcessor {
    /// Создаёт чистый processor без фактически pending output после reset/start.
    fn new(config: AudioTempoProcessorConfig) -> Self {
        Self {
            pcm_format: config.pcm_format(),
            active_segment: config.initial_segment(),
            pending_segment: config.initial_segment(),
            pending_processor_output: AudioTempoFrameCount::ZERO,
            input_latency: AudioTempoFrameCount::new(96),
            output_latency: AudioTempoFrameCount::new(48),
            process_error: None,
            rejected_segment: None,
            processed_packet_count: 0,
            primed_history_frames: AudioTempoFrameCount::ZERO,
        }
    }

    /// Настраивает фактический pending tail независимо от static latency.
    fn with_pending_output(mut self, pending_processor_output: AudioTempoFrameCount) -> Self {
        self.pending_processor_output = pending_processor_output;
        self
    }

    /// Настраивает typed process failure для проверки downcast boundary.
    fn with_process_error(mut self, process_error: AudioTempoProcessorError) -> Self {
        self.process_error = Some(process_error);
        self
    }

    /// Настраивает segment, который fake обязан отклонить атомарно.
    fn rejecting_segment(mut self, segment_id: AudioTempoSegmentId) -> Self {
        self.rejected_segment = Some(segment_id);
        self
    }

    /// Строит report и сохраняет раздельные produced/pending ordered spans.
    fn report_for_operation(
        &self,
        consumed_decoded_media: AudioTempoFrameCount,
        produced_spans: Vec<AudioTempoOutputSegmentSpan>,
        pending_spans: Vec<AudioTempoOutputSegmentSpan>,
    ) -> Result<AudioTempoProcessReport> {
        self.report_for_active_segment(
            self.active_segment,
            consumed_decoded_media,
            produced_spans,
            pending_spans,
        )
    }

    /// Строит planned report до atomic segment commit.
    fn report_for_active_segment(
        &self,
        active_segment: AudioTempoSegment,
        consumed_decoded_media: AudioTempoFrameCount,
        produced_spans: Vec<AudioTempoOutputSegmentSpan>,
        pending_spans: Vec<AudioTempoOutputSegmentSpan>,
    ) -> Result<AudioTempoProcessReport> {
        let produced_stretched_output = sum_span_frames(&produced_spans);
        let pending_processor_output = sum_span_frames(&pending_spans);

        AudioTempoProcessReport::from_frame_counts(
            self.pcm_format,
            active_segment,
            AudioTempoReportFrameCounts {
                consumed_decoded_media,
                produced_stretched_output,
                pending_processor_output,
                input_latency: self.input_latency,
                output_latency: self.output_latency,
            },
            AudioTempoOutputProgressMapping::new(produced_spans, pending_spans),
        )
    }

    /// Считает output frames из decoded media frames для fake ratio.
    fn produced_frames_for(
        &self,
        decoded_media_frames: AudioTempoFrameCount,
    ) -> AudioTempoFrameCount {
        AudioTempoFrameCount::new(
            (decoded_media_frames.get() as f64 / self.active_segment.ratio().as_f64()).round()
                as u64,
        )
    }

    /// Возвращает один span только для ненулевого количества frames.
    fn span_for(
        &self,
        segment: AudioTempoSegment,
        frame_count: AudioTempoFrameCount,
    ) -> Vec<AudioTempoOutputSegmentSpan> {
        if frame_count == AudioTempoFrameCount::ZERO {
            Vec::new()
        } else {
            vec![AudioTempoOutputSegmentSpan::new(
                self.pcm_format,
                segment,
                frame_count,
            )]
        }
    }
}

impl AudioTempoProcessor for FakeTempoProcessor {
    fn pcm_format(&self) -> AudioTempoPcmFormat {
        self.pcm_format
    }

    fn prime_decoded_history(
        &mut self,
        decoded_history: AudioTempoDecodedMedia<'_>,
    ) -> Result<AudioTempoProcessReport> {
        if decoded_history.pcm_format() != self.pcm_format {
            return Err(AudioTempoProcessorError::PcmFormatMismatch {
                expected: self.pcm_format,
                actual: decoded_history.pcm_format(),
            }
            .into());
        }
        if self.processed_packet_count > 0
            || self.pending_processor_output != AudioTempoFrameCount::ZERO
        {
            return Err(AudioTempoProcessorError::HistoryPrimeAfterStreamStart.into());
        }

        self.primed_history_frames = decoded_history.frame_count();
        self.report_for_operation(AudioTempoFrameCount::ZERO, Vec::new(), Vec::new())
    }

    fn set_segment(&mut self, segment: AudioTempoSegment) -> Result<AudioTempoProcessReport> {
        if self.rejected_segment == Some(segment.segment_id()) {
            return Err(AudioTempoProcessorError::UnsupportedRatio {
                requested_ratio: segment.ratio(),
            }
            .into());
        }

        // Pending PCM уже принадлежит старому segment-у и не переименовывается.
        let pending_spans = self.span_for(self.pending_segment, self.pending_processor_output);
        let report = self.report_for_active_segment(
            segment,
            AudioTempoFrameCount::ZERO,
            Vec::new(),
            pending_spans,
        )?;
        self.active_segment = segment;
        Ok(report)
    }

    fn process_decoded_media_into<'output>(
        &mut self,
        decoded_media: AudioTempoDecodedMedia<'_>,
        output_buffer: &'output mut Vec<f32>,
    ) -> Result<AudioTempoStretchedOutput<'output>> {
        // Format mismatch проверяется до ошибки backend-а и до очистки caller buffer-а.
        if decoded_media.pcm_format() != self.pcm_format {
            return Err(AudioTempoProcessorError::PcmFormatMismatch {
                expected: self.pcm_format,
                actual: decoded_media.pcm_format(),
            }
            .into());
        }
        if let Some(process_error) = self.process_error.take() {
            return Err(process_error.into());
        }

        let produced_frames = self.produced_frames_for(decoded_media.frame_count());
        let output_sample_count =
            produced_frames.interleaved_sample_len(self.pcm_format.channel_count())?;
        output_buffer.clear();
        output_buffer.resize(output_sample_count, 0.0);
        if self.active_segment.ratio().is_normal() {
            output_buffer.copy_from_slice(decoded_media.interleaved_samples());
        }

        self.processed_packet_count += 1;
        self.pending_segment = self.active_segment;
        let produced_spans = self.span_for(self.active_segment, produced_frames);
        let pending_spans = self.span_for(self.pending_segment, self.pending_processor_output);
        let report =
            self.report_for_operation(decoded_media.frame_count(), produced_spans, pending_spans)?;
        AudioTempoStretchedOutput::new(output_buffer, report, self.pcm_format)
    }

    fn finish_stream_into<'output>(
        &mut self,
        output_buffer: &'output mut Vec<f32>,
    ) -> Result<AudioTempoStretchedOutput<'output>> {
        let produced_frames = self.pending_processor_output;
        let produced_spans = self.span_for(self.pending_segment, produced_frames);
        let output_sample_count =
            produced_frames.interleaved_sample_len(self.pcm_format.channel_count())?;
        output_buffer.clear();
        output_buffer.resize(output_sample_count, 0.0);
        self.pending_processor_output = AudioTempoFrameCount::ZERO;
        let report =
            self.report_for_operation(AudioTempoFrameCount::ZERO, produced_spans, Vec::new())?;
        AudioTempoStretchedOutput::new(output_buffer, report, self.pcm_format)
    }

    fn reset(&mut self) -> Result<AudioTempoProcessReport> {
        self.pending_processor_output = AudioTempoFrameCount::ZERO;
        self.processed_packet_count = 0;
        self.report_for_operation(AudioTempoFrameCount::ZERO, Vec::new(), Vec::new())
    }
}

/// Factory failure проверяет, что anyhow не стирает typed tempo error.
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

/// Возвращает общий stereo 48 kHz format focused tests.
fn stereo_48k_format() -> AudioTempoPcmFormat {
    AudioTempoPcmFormat::new(
        AudioTempoSampleRateHz::new(48_000).expect("valid sample rate"),
        AudioTempoChannelCount::new(2).expect("valid channel count"),
    )
}

/// Возвращает mono format для typed mismatch test-а.
fn mono_48k_format() -> AudioTempoPcmFormat {
    AudioTempoPcmFormat::new(
        AudioTempoSampleRateHz::new(48_000).expect("valid sample rate"),
        AudioTempoChannelCount::new(1).expect("valid channel count"),
    )
}

/// Собирает config с предсказуемым segment id.
fn config_for_ratio(ratio: AudioTempoRatio) -> AudioTempoProcessorConfig {
    AudioTempoProcessorConfig::new(
        stereo_48k_format(),
        AudioTempoSegment::new(AudioTempoSegmentId::new(7), ratio),
    )
}

/// Создаёт stereo decoded packet и сохраняет format внутри value object-а.
fn decoded_stereo(interleaved_samples: &[f32]) -> AudioTempoDecodedMedia<'_> {
    AudioTempoDecodedMedia::from_interleaved_samples(interleaved_samples, stereo_48k_format())
        .expect("frame-aligned stereo PCM")
}

/// Складывает frames ordered spans для тестовой сборки report-а.
fn sum_span_frames(spans: &[AudioTempoOutputSegmentSpan]) -> AudioTempoFrameCount {
    AudioTempoFrameCount::new(
        spans
            .iter()
            .map(|span| span.stretched_output().frame_count().get())
            .sum(),
    )
}

#[test]
fn passthrough_ratio_preserves_samples_and_reuses_caller_buffer() {
    let config = config_for_ratio(AudioTempoRatio::NORMAL);
    let mut processor = FakeTempoProcessor::new(config);
    let samples = [0.25, -0.25, 0.5, -0.5];
    let decoded = decoded_stereo(&samples);
    let mut output_buffer = Vec::with_capacity(32);
    let allocation_address = output_buffer.as_ptr();

    let output = processor
        .process_decoded_media_into(decoded, &mut output_buffer)
        .expect("passthrough process should succeed");

    assert_eq!(output.interleaved_samples(), samples);
    assert_eq!(
        output.report().consumed_decoded_media().frame_count().get(),
        2
    );
    assert_eq!(
        output
            .report()
            .produced_stretched_output()
            .frame_count()
            .get(),
        2
    );
    assert_eq!(
        output
            .report()
            .output_progress_mapping()
            .produced_output_segments()
            .len(),
        1
    );
    drop(output);
    assert_eq!(output_buffer.as_ptr(), allocation_address);
}

#[test]
fn decoded_media_preserves_format_and_mismatch_is_non_mutating() {
    let config = config_for_ratio(AudioTempoRatio::NORMAL);
    let mut processor = FakeTempoProcessor::new(config);
    let mono_samples = [0.25, -0.25];
    let decoded =
        AudioTempoDecodedMedia::from_interleaved_samples(&mono_samples, mono_48k_format())
            .expect("valid mono packet");
    let mut output_buffer = vec![91.0, 92.0];

    let error = processor
        .process_decoded_media_into(decoded, &mut output_buffer)
        .expect_err("format mismatch must fail");

    assert_eq!(
        error.downcast_ref::<AudioTempoProcessorError>(),
        Some(&AudioTempoProcessorError::PcmFormatMismatch {
            expected: stereo_48k_format(),
            actual: mono_48k_format(),
        })
    );
    assert_eq!(decoded.pcm_format(), mono_48k_format());
    assert_eq!(output_buffer, [91.0, 92.0]);
    assert_eq!(processor.processed_packet_count, 0);
}

#[test]
fn history_prime_has_no_output_or_pending_accounting() {
    let config = config_for_ratio(AudioTempoRatio::NORMAL);
    let mut processor = FakeTempoProcessor::new(config);
    let history_samples = vec![0.25f32; 960 * 2];
    let history = decoded_stereo(&history_samples);

    let report = processor
        .prime_decoded_history(history)
        .expect("fresh processor should accept history");

    assert_eq!(
        processor.primed_history_frames,
        AudioTempoFrameCount::new(960)
    );
    assert_eq!(
        report.consumed_decoded_media().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert_eq!(
        report.produced_stretched_output().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert_eq!(
        report.pending_processor_output().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert!(
        report
            .output_progress_mapping()
            .produced_output_segments()
            .is_empty()
    );
    assert!(
        report
            .output_progress_mapping()
            .pending_output_segments()
            .is_empty()
    );
}

#[test]
fn reset_keeps_static_latencies_but_clears_actual_pending_output() {
    let config = config_for_ratio(AudioTempoRatio::NORMAL);
    let mut processor =
        FakeTempoProcessor::new(config).with_pending_output(AudioTempoFrameCount::new(144));

    let report = processor.reset().expect("reset should succeed");

    assert_eq!(
        report.pending_processor_output().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert_eq!(
        report.input_latency().frame_count(),
        AudioTempoFrameCount::new(96)
    );
    assert_eq!(
        report.output_latency().frame_count(),
        AudioTempoFrameCount::new(48)
    );
    assert!(
        report
            .output_progress_mapping()
            .pending_output_segments()
            .is_empty()
    );
}

#[test]
fn finish_returns_all_pending_pcm_once_and_reports_zero_pending_afterward() {
    let config = config_for_ratio(AudioTempoRatio::NORMAL);
    let mut processor =
        FakeTempoProcessor::new(config).with_pending_output(AudioTempoFrameCount::new(3));
    let mut output_buffer = Vec::with_capacity(16);

    let output = processor
        .finish_stream_into(&mut output_buffer)
        .expect("finish should succeed");

    assert_eq!(output.interleaved_samples().len(), 6);
    assert_eq!(
        output
            .report()
            .produced_stretched_output()
            .frame_count()
            .get(),
        3
    );
    assert_eq!(
        output.report().pending_processor_output().frame_count(),
        AudioTempoFrameCount::ZERO
    );
    assert_eq!(
        output
            .report()
            .output_progress_mapping()
            .produced_output_segments()[0]
            .stretched_output()
            .frame_count()
            .get(),
        3
    );
}

#[test]
fn rejected_segment_change_is_atomic_and_preserves_old_pending_span() {
    let initial_segment = config_for_ratio(AudioTempoRatio::NORMAL).initial_segment();
    let rejected_segment = AudioTempoSegment::new(
        AudioTempoSegmentId::new(8),
        AudioTempoRatio::new(2.0).expect("valid ratio"),
    );
    let mut processor = FakeTempoProcessor::new(config_for_ratio(AudioTempoRatio::NORMAL))
        .with_pending_output(AudioTempoFrameCount::new(24))
        .rejecting_segment(rejected_segment.segment_id());

    let error = processor
        .set_segment(rejected_segment)
        .expect_err("configured segment must be rejected");

    assert!(error.downcast_ref::<AudioTempoProcessorError>().is_some());
    assert_eq!(processor.active_segment, initial_segment);
    assert_eq!(processor.pending_segment, initial_segment);
    assert_eq!(
        processor.pending_processor_output,
        AudioTempoFrameCount::new(24)
    );
}

#[test]
fn accepted_segment_change_reports_old_pending_tail_in_order() {
    let initial_segment = config_for_ratio(AudioTempoRatio::NORMAL).initial_segment();
    let updated_segment = AudioTempoSegment::new(
        AudioTempoSegmentId::new(8),
        AudioTempoRatio::new(2.0).expect("valid ratio"),
    );
    let mut processor = FakeTempoProcessor::new(config_for_ratio(AudioTempoRatio::NORMAL))
        .with_pending_output(AudioTempoFrameCount::new(24));

    let report = processor
        .set_segment(updated_segment)
        .expect("segment change should succeed");

    assert_eq!(report.active_segment(), updated_segment);
    let pending_spans = report.output_progress_mapping().pending_output_segments();
    assert_eq!(pending_spans.len(), 1);
    assert_eq!(pending_spans[0].segment(), initial_segment);
    assert_eq!(pending_spans[0].stretched_output().frame_count().get(), 24);
}

#[test]
fn report_validates_ordered_span_totals() {
    let format = stereo_48k_format();
    let segment = config_for_ratio(AudioTempoRatio::NORMAL).initial_segment();
    let mapping = AudioTempoOutputProgressMapping::new(
        vec![AudioTempoOutputSegmentSpan::new(
            format,
            segment,
            AudioTempoFrameCount::new(3),
        )],
        Vec::new(),
    );

    let error = AudioTempoProcessReport::from_frame_counts(
        format,
        segment,
        AudioTempoReportFrameCounts {
            consumed_decoded_media: AudioTempoFrameCount::new(3),
            produced_stretched_output: AudioTempoFrameCount::new(4),
            pending_processor_output: AudioTempoFrameCount::ZERO,
            input_latency: AudioTempoFrameCount::new(96),
            output_latency: AudioTempoFrameCount::new(48),
        },
        mapping,
    )
    .expect_err("aggregate and ordered spans must agree");

    assert!(matches!(
        error.downcast_ref::<AudioTempoProcessorError>(),
        Some(AudioTempoProcessorError::OutputSegmentFrameCountMismatch { .. })
    ));
}

#[test]
fn processor_and_factory_errors_stay_typed_for_downcast() {
    let requested_ratio = AudioTempoRatio::new(4.0).expect("valid ratio");
    let factory_error = AudioTempoProcessorError::UnsupportedRatio { requested_ratio };
    let factory = FailingTempoProcessorFactory {
        error: factory_error.clone(),
    };

    let error = match factory.create_processor(config_for_ratio(requested_ratio)) {
        Ok(_) => panic!("factory must fail"),
        Err(error) => error,
    };
    assert_eq!(
        error.downcast_ref::<AudioTempoProcessorError>(),
        Some(&factory_error)
    );

    let process_error = AudioTempoProcessorError::BackendFailure {
        message: "fake process failure".to_owned(),
    };
    let mut processor = FakeTempoProcessor::new(config_for_ratio(AudioTempoRatio::NORMAL))
        .with_process_error(process_error.clone());
    let mut output_buffer = vec![5.0];
    let error = processor
        .process_decoded_media_into(decoded_stereo(&[0.0, 0.0]), &mut output_buffer)
        .expect_err("process must fail");
    assert_eq!(
        error.downcast_ref::<AudioTempoProcessorError>(),
        Some(&process_error)
    );
    assert_eq!(output_buffer, [5.0]);
}

#[test]
fn non_normal_ratio_separates_media_and_output_durations() {
    let double_speed = AudioTempoRatio::new(2.0).expect("valid ratio");
    let mut processor = FakeTempoProcessor::new(config_for_ratio(double_speed));
    let samples = vec![0.0; 960 * 2];
    let mut output_buffer = Vec::new();

    let output = processor
        .process_decoded_media_into(decoded_stereo(&samples), &mut output_buffer)
        .expect("process should succeed");

    assert_eq!(
        output
            .report()
            .consumed_decoded_media()
            .duration()
            .as_millis(),
        20
    );
    assert_eq!(
        output
            .report()
            .produced_stretched_output()
            .duration()
            .as_millis(),
        10
    );
    assert_eq!(output.report().effective_ratio(), double_speed);
}
