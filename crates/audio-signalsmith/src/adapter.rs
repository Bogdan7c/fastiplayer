use std::collections::VecDeque;

use audio_core::{
    AudioTempoChannelCount, AudioTempoDecodedMedia, AudioTempoFrameCount,
    AudioTempoOutputProgressMapping, AudioTempoOutputSegmentSpans, AudioTempoPcmFormat,
    AudioTempoProcessReport, AudioTempoProcessor, AudioTempoProcessorConfig,
    AudioTempoProcessorError, AudioTempoProcessorFactory, AudioTempoProcessorHandle,
    AudioTempoReportFrameCounts, AudioTempoSegment, AudioTempoStretchedOutput,
};
use signalsmith_stretch::Stretch;

use self::timeline::{
    SegmentFrames, SignalsmithProcessChunks, SignalsmithTimelineState,
    append_public_spans_to_queue, backend_failure, concatenate_public_spans, drain_segment_frames,
    project_pending_output_spans, push_segment_frames, sum_public_span_frames, sum_queue_frames,
};

mod timeline;

/// Factory runtime adapter-а над Signalsmith Stretch.
#[derive(Debug, Default, Clone, Copy)]
pub struct SignalsmithTempoProcessorFactory;

impl AudioTempoProcessorFactory for SignalsmithTempoProcessorFactory {
    fn create_processor(
        &self,
        config: AudioTempoProcessorConfig,
    ) -> anyhow::Result<AudioTempoProcessorHandle> {
        Ok(Box::new(SignalsmithTempoProcessor::new(config)))
    }
}

/// Полностью подготовленная operation до мутации backend/caller buffer-а.
struct SignalsmithOperationPlan {
    report: AudioTempoProcessReport,
    process_chunks: SignalsmithProcessChunks,
    process_output_frames: AudioTempoFrameCount,
    total_output_frames: AudioTempoFrameCount,
}

/// Concrete owner Signalsmith DSP и его latency-aware timeline model.
///
/// `timeline_state` хранит два независимых лага upstream API: decoded input
/// опережает processing time на `input_latency`, а output отстаёт от него на
/// `output_latency`. Поэтому rate transition не переименовывает уже принятый
/// старый DSP segment и может вернуть ordered old/new output spans.
pub struct SignalsmithTempoProcessor {
    stretch: Stretch,
    pcm_format: AudioTempoPcmFormat,
    active_segment: AudioTempoSegment,
    input_latency_frames: AudioTempoFrameCount,
    output_latency_frames: AudioTempoFrameCount,
    timeline_state: SignalsmithTimelineState,
    scratch_timeline_state: SignalsmithTimelineState,
    projection_output_queue: VecDeque<SegmentFrames>,
    eof_silence_buffer: Vec<f32>,
    history_primed: bool,
}

impl SignalsmithTempoProcessor {
    /// Создаёт processor с default-пресетом Signalsmith под PCM format.
    #[must_use]
    pub fn new(config: AudioTempoProcessorConfig) -> Self {
        let pcm_format = config.pcm_format();
        let stretch = Stretch::preset_default(
            pcm_format.channel_count().get(),
            pcm_format.sample_rate_hz().get(),
        );
        let input_latency_frames = AudioTempoFrameCount::new(stretch.input_latency() as u64);
        let output_latency_frames = AudioTempoFrameCount::new(stretch.output_latency() as u64);

        Self {
            stretch,
            pcm_format,
            active_segment: config.initial_segment(),
            input_latency_frames,
            output_latency_frames,
            timeline_state: SignalsmithTimelineState::default(),
            scratch_timeline_state: SignalsmithTimelineState::default(),
            projection_output_queue: VecDeque::new(),
            eof_silence_buffer: Vec::new(),
            history_primed: false,
        }
    }

    /// Валидирует packet format до любой мутации hot-path state-а.
    fn validate_decoded_media_format(
        &self,
        decoded_media: AudioTempoDecodedMedia<'_>,
    ) -> anyhow::Result<()> {
        if decoded_media.pcm_format() != self.pcm_format {
            return Err(AudioTempoProcessorError::PcmFormatMismatch {
                expected: self.pcm_format,
                actual: decoded_media.pcm_format(),
            }
            .into());
        }
        Ok(())
    }

    /// Готовит обычный process на scratch state, чтобы typed error был atomic.
    fn prepare_process_plan(
        &mut self,
        consumed_decoded_frames: AudioTempoFrameCount,
    ) -> anyhow::Result<SignalsmithOperationPlan> {
        self.scratch_timeline_state.copy_from(&self.timeline_state);
        self.scratch_timeline_state.prime_latency(
            self.active_segment,
            self.input_latency_frames.get(),
            self.output_latency_frames.get(),
        )?;
        push_segment_frames(
            &mut self.scratch_timeline_state.processing_input_queue,
            SegmentFrames::new(self.active_segment, consumed_decoded_frames.get()),
        )?;

        let processing_spans = drain_segment_frames(
            &mut self.scratch_timeline_state.processing_input_queue,
            consumed_decoded_frames.get(),
            self.pcm_format,
        )?;
        let (process_chunks, scheduled_output_spans) = self
            .scratch_timeline_state
            .schedule_processing_spans(&processing_spans, self.pcm_format)?;
        let process_output_frames = sum_public_span_frames(&scheduled_output_spans)?;
        append_public_spans_to_queue(
            &mut self.scratch_timeline_state.output_delay_queue,
            &scheduled_output_spans,
        )?;
        let produced_output_spans = drain_segment_frames(
            &mut self.scratch_timeline_state.output_delay_queue,
            process_output_frames,
            self.pcm_format,
        )?;

        self.scratch_timeline_state.has_decoded_media = true;
        let pending_output_spans = project_pending_output_spans(
            &self.scratch_timeline_state,
            &mut self.projection_output_queue,
            self.pcm_format,
        )?;
        let pending_output_frames = sum_public_span_frames(&pending_output_spans)?;
        let report = self.build_report(
            self.active_segment,
            consumed_decoded_frames,
            AudioTempoFrameCount::new(process_output_frames),
            AudioTempoFrameCount::new(pending_output_frames),
            produced_output_spans,
            pending_output_spans,
        )?;

        Ok(SignalsmithOperationPlan {
            report,
            process_chunks,
            process_output_frames: AudioTempoFrameCount::new(process_output_frames),
            total_output_frames: AudioTempoFrameCount::new(process_output_frames),
        })
    }

    /// Готовит единый EOF result: input-latency silence process + output flush.
    fn prepare_finish_plan(&mut self) -> anyhow::Result<SignalsmithOperationPlan> {
        self.scratch_timeline_state.copy_from(&self.timeline_state);
        let processing_spans = drain_segment_frames(
            &mut self.scratch_timeline_state.processing_input_queue,
            self.input_latency_frames.get(),
            self.pcm_format,
        )?;
        let (process_chunks, scheduled_output_spans) = self
            .scratch_timeline_state
            .schedule_processing_spans(&processing_spans, self.pcm_format)?;
        let process_output_frames = sum_public_span_frames(&scheduled_output_spans)?;
        append_public_spans_to_queue(
            &mut self.scratch_timeline_state.output_delay_queue,
            &scheduled_output_spans,
        )?;

        // Process silence сначала выдаёт начало pending queue, flush — весь остаток.
        let process_output_spans = drain_segment_frames(
            &mut self.scratch_timeline_state.output_delay_queue,
            process_output_frames,
            self.pcm_format,
        )?;
        let flush_output_frames =
            sum_queue_frames(&self.scratch_timeline_state.output_delay_queue)?;
        if flush_output_frames != self.output_latency_frames.get() {
            return Err(backend_failure(format!(
                "output delay queue has {flush_output_frames} frames before flush, expected {}",
                self.output_latency_frames.get()
            ))
            .into());
        }
        let flush_output_spans = drain_segment_frames(
            &mut self.scratch_timeline_state.output_delay_queue,
            flush_output_frames,
            self.pcm_format,
        )?;
        let produced_output_spans =
            concatenate_public_spans(process_output_spans, flush_output_spans, self.pcm_format)?;
        let total_output_frames = process_output_frames
            .checked_add(flush_output_frames)
            .ok_or_else(|| backend_failure("EOF output frame count overflow"))?;

        let report = self.build_report(
            self.active_segment,
            AudioTempoFrameCount::ZERO,
            AudioTempoFrameCount::new(total_output_frames),
            AudioTempoFrameCount::ZERO,
            produced_output_spans,
            AudioTempoOutputSegmentSpans::default(),
        )?;

        Ok(SignalsmithOperationPlan {
            report,
            process_chunks,
            process_output_frames: AudioTempoFrameCount::new(process_output_frames),
            total_output_frames: AudioTempoFrameCount::new(total_output_frames),
        })
    }

    /// Строит report, где static latencies и actual pending не смешаны.
    fn build_report(
        &self,
        active_segment: AudioTempoSegment,
        consumed_decoded_media: AudioTempoFrameCount,
        produced_stretched_output: AudioTempoFrameCount,
        pending_processor_output: AudioTempoFrameCount,
        produced_output_segments: AudioTempoOutputSegmentSpans,
        pending_output_segments: AudioTempoOutputSegmentSpans,
    ) -> anyhow::Result<AudioTempoProcessReport> {
        AudioTempoProcessReport::from_frame_counts(
            self.pcm_format,
            active_segment,
            AudioTempoReportFrameCounts {
                consumed_decoded_media,
                produced_stretched_output,
                pending_processor_output,
                input_latency: self.input_latency_frames,
                output_latency: self.output_latency_frames,
            },
            AudioTempoOutputProgressMapping::new(produced_output_segments, pending_output_segments),
        )
    }

    /// Строит no-output report для reset, empty process или repeated finish.
    fn empty_report(&self) -> anyhow::Result<AudioTempoProcessReport> {
        self.build_report(
            self.active_segment,
            AudioTempoFrameCount::ZERO,
            AudioTempoFrameCount::ZERO,
            AudioTempoFrameCount::ZERO,
            AudioTempoOutputSegmentSpans::default(),
            AudioTempoOutputSegmentSpans::default(),
        )
    }

    /// Строит pending-only report, используемый atomic set_segment boundary.
    fn segment_change_report(
        &mut self,
        requested_segment: AudioTempoSegment,
    ) -> anyhow::Result<AudioTempoProcessReport> {
        let pending_output_spans = project_pending_output_spans(
            &self.timeline_state,
            &mut self.projection_output_queue,
            self.pcm_format,
        )?;
        let pending_output_frames = sum_public_span_frames(&pending_output_spans)?;
        self.build_report(
            requested_segment,
            AudioTempoFrameCount::ZERO,
            AudioTempoFrameCount::ZERO,
            AudioTempoFrameCount::new(pending_output_frames),
            AudioTempoOutputSegmentSpans::default(),
            pending_output_spans,
        )
    }

    /// Коммитит заранее проверенный scratch timeline после infallible DSP process.
    fn commit_scratch_timeline(&mut self) {
        std::mem::swap(&mut self.timeline_state, &mut self.scratch_timeline_state);
    }
}

impl AudioTempoProcessor for SignalsmithTempoProcessor {
    fn pcm_format(&self) -> AudioTempoPcmFormat {
        self.pcm_format
    }

    fn prime_decoded_history(
        &mut self,
        decoded_history: AudioTempoDecodedMedia<'_>,
    ) -> anyhow::Result<AudioTempoProcessReport> {
        self.validate_decoded_media_format(decoded_history)?;
        if self.timeline_state.has_decoded_media || self.timeline_state.latency_primed {
            return Err(AudioTempoProcessorError::HistoryPrimeAfterStreamStart.into());
        }

        // `seek` — position-free pre-roll API upstream; history не попадает в output/accounting.
        self.stretch.seek(
            decoded_history.interleaved_samples(),
            self.active_segment.ratio().as_f64(),
        );
        self.history_primed = true;
        self.empty_report()
    }

    fn set_segment(
        &mut self,
        segment: AudioTempoSegment,
    ) -> anyhow::Result<AudioTempoProcessReport> {
        // Report строится до commit: любой Err оставляет old segment/state нетронутым.
        let report = self.segment_change_report(segment)?;
        if self.history_primed
            && !self.timeline_state.has_decoded_media
            && segment != self.active_segment
        {
            // Seek hint относился к старому ratio; безопаснее отбросить pre-roll, чем применить неверно.
            self.stretch.reset();
            self.history_primed = false;
        }
        self.active_segment = segment;
        Ok(report)
    }

    fn process_decoded_media_into<'output>(
        &mut self,
        decoded_media: AudioTempoDecodedMedia<'_>,
        output_buffer: &'output mut Vec<f32>,
    ) -> anyhow::Result<AudioTempoStretchedOutput<'output>> {
        self.validate_decoded_media_format(decoded_media)?;
        if decoded_media.frame_count() == AudioTempoFrameCount::ZERO {
            let report = self.segment_change_report(self.active_segment)?;
            output_buffer.clear();
            return AudioTempoStretchedOutput::new(output_buffer, report, self.pcm_format);
        }

        let plan = self.prepare_process_plan(decoded_media.frame_count())?;
        let output_sample_count = plan
            .total_output_frames
            .interleaved_sample_len(self.pcm_format.channel_count())?;
        output_buffer.clear();
        output_buffer.resize(output_sample_count, 0.0);

        process_ordered_chunks(
            &mut self.stretch,
            decoded_media.interleaved_samples(),
            output_buffer,
            &plan.process_chunks,
            self.pcm_format.channel_count(),
        )?;
        self.commit_scratch_timeline();
        AudioTempoStretchedOutput::new(output_buffer, plan.report, self.pcm_format)
    }

    fn finish_stream_into<'output>(
        &mut self,
        output_buffer: &'output mut Vec<f32>,
    ) -> anyhow::Result<AudioTempoStretchedOutput<'output>> {
        if !self.timeline_state.has_decoded_media {
            if self.history_primed {
                // Terminal finish обязан закрыть даже position-free pre-roll lifecycle.
                self.stretch.reset();
                self.timeline_state.reset();
                self.scratch_timeline_state.reset();
                self.history_primed = false;
            }
            output_buffer.clear();
            return AudioTempoStretchedOutput::new(
                output_buffer,
                self.empty_report()?,
                self.pcm_format,
            );
        }

        let plan = self.prepare_finish_plan()?;
        let channel_count = self.pcm_format.channel_count();
        let silence_sample_count = self
            .input_latency_frames
            .interleaved_sample_len(channel_count)?;
        let process_output_sample_count = plan
            .process_output_frames
            .interleaved_sample_len(channel_count)?;
        let total_output_sample_count = plan
            .total_output_frames
            .interleaved_sample_len(channel_count)?;

        self.eof_silence_buffer.clear();
        self.eof_silence_buffer.resize(silence_sample_count, 0.0);
        self.eof_silence_buffer.fill(0.0);
        output_buffer.clear();
        output_buffer.resize(total_output_sample_count, 0.0);
        let (processing_output, flush_output) =
            output_buffer.split_at_mut(process_output_sample_count);

        // Upstream lifecycle: сначала processing time до EOF, потом synthesis tail.
        process_ordered_chunks(
            &mut self.stretch,
            &self.eof_silence_buffer,
            processing_output,
            &plan.process_chunks,
            channel_count,
        )?;
        self.stretch.flush(flush_output);
        self.timeline_state.reset();
        self.scratch_timeline_state.reset();
        self.history_primed = false;
        AudioTempoStretchedOutput::new(output_buffer, plan.report, self.pcm_format)
    }

    fn reset(&mut self) -> anyhow::Result<AudioTempoProcessReport> {
        self.stretch.reset();
        self.timeline_state.reset();
        self.scratch_timeline_state.reset();
        self.history_primed = false;
        self.empty_report()
    }
}

/// Выполняет реальные Signalsmith calls на каждой ordered segment boundary.
///
/// Один усреднённый `process(input, output)` здесь неверен: Signalsmith задаёт
/// stretch ratio именно отношением длин конкретного вызова.
fn process_ordered_chunks(
    stretch: &mut Stretch,
    input_samples: &[f32],
    output_samples: &mut [f32],
    process_chunks: &SignalsmithProcessChunks,
    channel_count: AudioTempoChannelCount,
) -> anyhow::Result<()> {
    let (input_frame_count, output_frame_count) = process_chunks.as_slice().iter().try_fold(
        (0u64, 0u64),
        |(input_total, output_total), chunk| {
            Ok::<_, anyhow::Error>((
                input_total
                    .checked_add(chunk.input_frames())
                    .ok_or_else(|| backend_failure("process chunk input frame overflow"))?,
                output_total
                    .checked_add(chunk.output_frames())
                    .ok_or_else(|| backend_failure("process chunk output frame overflow"))?,
            ))
        },
    )?;
    let expected_input_samples =
        AudioTempoFrameCount::new(input_frame_count).interleaved_sample_len(channel_count)?;
    let expected_output_samples =
        AudioTempoFrameCount::new(output_frame_count).interleaved_sample_len(channel_count)?;
    if input_samples.len() != expected_input_samples
        || output_samples.len() != expected_output_samples
    {
        return Err(backend_failure(format!(
            "process chunk geometry mismatch: input {} != {expected_input_samples}, output {} != {expected_output_samples}",
            input_samples.len(),
            output_samples.len()
        ))
        .into());
    }

    let mut input_sample_offset = 0usize;
    let mut output_sample_offset = 0usize;
    for chunk in process_chunks.as_slice() {
        let chunk_input_samples = AudioTempoFrameCount::new(chunk.input_frames())
            .interleaved_sample_len(channel_count)?;
        let chunk_output_samples = AudioTempoFrameCount::new(chunk.output_frames())
            .interleaved_sample_len(channel_count)?;
        let next_input_sample_offset = input_sample_offset + chunk_input_samples;
        let next_output_sample_offset = output_sample_offset + chunk_output_samples;
        stretch.process(
            &input_samples[input_sample_offset..next_input_sample_offset],
            &mut output_samples[output_sample_offset..next_output_sample_offset],
        );
        input_sample_offset = next_input_sample_offset;
        output_sample_offset = next_output_sample_offset;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
