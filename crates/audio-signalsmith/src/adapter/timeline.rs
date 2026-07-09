use std::collections::VecDeque;

use audio_core::{
    AudioTempoFrameCount, AudioTempoOutputSegmentSpan, AudioTempoOutputSegmentSpans,
    AudioTempoPcmFormat, AudioTempoProcessorError, AudioTempoSegment,
};

/// Frames одного segment-а во внутренней processing/output очереди.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SegmentFrames {
    segment: AudioTempoSegment,
    frame_count: u64,
}

/// Один реальный вызов `Stretch::process` на непрерывном tempo segment-е.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SignalsmithProcessChunk {
    input_frames: u64,
    output_frames: u64,
}

impl SignalsmithProcessChunk {
    /// Сохраняет input/output geometry одного backend call-а.
    fn new(input_frames: u64, output_frames: u64) -> Self {
        Self {
            input_frames,
            output_frames,
        }
    }

    /// Frames decoded/silence input-а для backend call-а.
    pub(super) const fn input_frames(self) -> u64 {
        self.input_frames
    }

    /// Frames stretched output-а для backend call-а.
    pub(super) const fn output_frames(self) -> u64 {
        self.output_frames
    }
}

/// Inline ordered process chunks: типичный packet имеет один или два segment-а.
#[derive(Debug, Default)]
pub(super) enum SignalsmithProcessChunks {
    #[default]
    Empty,
    One(SignalsmithProcessChunk),
    Two([SignalsmithProcessChunk; 2]),
    Many(Vec<SignalsmithProcessChunk>),
}

impl SignalsmithProcessChunks {
    /// Добавляет обязательный backend call, включая input chunk с нулевым output.
    fn push(&mut self, chunk: SignalsmithProcessChunk) {
        let previous = std::mem::take(self);
        *self = match previous {
            Self::Empty => Self::One(chunk),
            Self::One(first) => Self::Two([first, chunk]),
            Self::Two([first, second]) => Self::Many(vec![first, second, chunk]),
            Self::Many(mut chunks) => {
                chunks.push(chunk);
                Self::Many(chunks)
            }
        };
    }

    /// Возвращает process calls в processing-time порядке.
    pub(super) fn as_slice(&self) -> &[SignalsmithProcessChunk] {
        match self {
            Self::Empty => &[],
            Self::One(chunk) => std::slice::from_ref(chunk),
            Self::Two(chunks) => chunks,
            Self::Many(chunks) => chunks,
        }
    }
}

impl SegmentFrames {
    /// Не создаёт пустые spans: это упрощает ordered mapping invariants.
    pub(super) fn new(segment: AudioTempoSegment, frame_count: u64) -> Option<Self> {
        (frame_count > 0).then_some(Self {
            segment,
            frame_count,
        })
    }
}

/// Reusable timeline state: input-ahead queue, output-behind queue и remainders.
#[derive(Debug, Default)]
pub(super) struct SignalsmithTimelineState {
    pub(super) processing_input_queue: VecDeque<SegmentFrames>,
    pub(super) output_delay_queue: VecDeque<SegmentFrames>,
    /// Глобальный дробный output frame переносится и между packet-ами, и между segment-ами.
    output_frame_remainder: f64,
    pub(super) latency_primed: bool,
    pub(super) has_decoded_media: bool,
}

impl SignalsmithTimelineState {
    /// Копирует state в processor-owned scratch buffers без обязательной аллокации.
    pub(super) fn copy_from(&mut self, source: &Self) {
        self.processing_input_queue.clear();
        self.processing_input_queue
            .extend(source.processing_input_queue.iter().copied());
        self.output_delay_queue.clear();
        self.output_delay_queue
            .extend(source.output_delay_queue.iter().copied());
        self.output_frame_remainder = source.output_frame_remainder;
        self.latency_primed = source.latency_primed;
        self.has_decoded_media = source.has_decoded_media;
    }

    /// Сбрасывает actual pending/accounting, сохраняя выделенные capacity.
    pub(super) fn reset(&mut self) {
        self.processing_input_queue.clear();
        self.output_delay_queue.clear();
        self.output_frame_remainder = 0.0;
        self.latency_primed = false;
        self.has_decoded_media = false;
    }

    /// При первом реальном input создаёт обе половины DSP latency.
    pub(super) fn prime_latency(
        &mut self,
        segment: AudioTempoSegment,
        input_latency_frames: u64,
        output_latency_frames: u64,
    ) -> anyhow::Result<()> {
        if self.latency_primed {
            return Ok(());
        }

        push_segment_frames(
            &mut self.processing_input_queue,
            SegmentFrames::new(segment, input_latency_frames),
        )?;
        push_segment_frames(
            &mut self.output_delay_queue,
            SegmentFrames::new(segment, output_latency_frames),
        )?;
        self.latency_primed = true;
        Ok(())
    }

    /// Планирует output для processing spans с переносом fractional remainder.
    pub(super) fn schedule_processing_spans(
        &mut self,
        processing_spans: &AudioTempoOutputSegmentSpans,
        pcm_format: AudioTempoPcmFormat,
    ) -> anyhow::Result<(SignalsmithProcessChunks, AudioTempoOutputSegmentSpans)> {
        let mut process_chunks = SignalsmithProcessChunks::default();
        let mut scheduled_spans = AudioTempoOutputSegmentSpans::default();
        for processing_span in processing_spans.as_slice() {
            let segment = processing_span.segment();
            let input_frames = processing_span.stretched_output().frame_count().get();
            let due_output_frames =
                schedule_output_frames(&mut self.output_frame_remainder, segment, input_frames)?;
            process_chunks.push(SignalsmithProcessChunk::new(
                input_frames,
                due_output_frames,
            ));
            push_public_span(&mut scheduled_spans, segment, due_output_frames, pcm_format);
        }
        Ok((process_chunks, scheduled_spans))
    }
}

/// Переводит следующий processing span в целые output frames с global carry.
fn schedule_output_frames(
    fractional_remainder: &mut f64,
    segment: AudioTempoSegment,
    processed_media_frames: u64,
) -> anyhow::Result<u64> {
    let exact_output_frames =
        processed_media_frames as f64 / segment.ratio().as_f64() + *fractional_remainder;
    if !exact_output_frames.is_finite() || exact_output_frames > u64::MAX as f64 {
        return Err(backend_failure("scheduled output frame counter overflow").into());
    }

    let whole_output_frames = exact_output_frames.floor() as u64;
    *fractional_remainder = exact_output_frames - whole_output_frames as f64;
    Ok(whole_output_frames)
}

/// Добавляет internal span и объединяет соседние части одного segment-а.
pub(super) fn push_segment_frames(
    queue: &mut VecDeque<SegmentFrames>,
    span: Option<SegmentFrames>,
) -> anyhow::Result<()> {
    let Some(span) = span else {
        return Ok(());
    };
    if let Some(last_span) = queue.back_mut()
        && last_span.segment == span.segment
    {
        last_span.frame_count = last_span
            .frame_count
            .checked_add(span.frame_count)
            .ok_or_else(|| backend_failure("timeline segment frame count overflow"))?;
        return Ok(());
    }
    queue.push_back(span);
    Ok(())
}

/// Вынимает ровно `frame_count` frames из начала ordered internal queue.
pub(super) fn drain_segment_frames(
    queue: &mut VecDeque<SegmentFrames>,
    frame_count: u64,
    pcm_format: AudioTempoPcmFormat,
) -> anyhow::Result<AudioTempoOutputSegmentSpans> {
    let mut remaining_frames = frame_count;
    let mut drained_spans = AudioTempoOutputSegmentSpans::default();
    while remaining_frames > 0 {
        let Some(front_span) = queue.front_mut() else {
            return Err(backend_failure(format!(
                "timeline queue underflow: {remaining_frames} frames are missing"
            ))
            .into());
        };
        let drained_frames = remaining_frames.min(front_span.frame_count);
        push_public_span(
            &mut drained_spans,
            front_span.segment,
            drained_frames,
            pcm_format,
        );
        front_span.frame_count -= drained_frames;
        remaining_frames -= drained_frames;
        if front_span.frame_count == 0 {
            queue.pop_front();
        }
    }
    Ok(drained_spans)
}

/// Добавляет public span; format обязателен для ненулевого frame count.
fn push_public_span(
    spans: &mut AudioTempoOutputSegmentSpans,
    segment: AudioTempoSegment,
    frame_count: u64,
    pcm_format: AudioTempoPcmFormat,
) {
    if frame_count == 0 {
        return;
    }
    spans.push(AudioTempoOutputSegmentSpan::new(
        pcm_format,
        segment,
        AudioTempoFrameCount::new(frame_count),
    ));
}

/// Ставит scheduled public spans в persistent output delay queue.
pub(super) fn append_public_spans_to_queue(
    queue: &mut VecDeque<SegmentFrames>,
    spans: &AudioTempoOutputSegmentSpans,
) -> anyhow::Result<()> {
    for span in spans.as_slice() {
        push_segment_frames(
            queue,
            SegmentFrames::new(span.segment(), span.stretched_output().frame_count().get()),
        )?;
    }
    Ok(())
}

/// Считает сумму public spans с overflow check.
pub(super) fn sum_public_span_frames(spans: &AudioTempoOutputSegmentSpans) -> anyhow::Result<u64> {
    spans.as_slice().iter().try_fold(0u64, |total, span| {
        total
            .checked_add(span.stretched_output().frame_count().get())
            .ok_or_else(|| backend_failure("public output span frame count overflow").into())
    })
}

/// Считает сумму internal queue frames с overflow check.
pub(super) fn sum_queue_frames(queue: &VecDeque<SegmentFrames>) -> anyhow::Result<u64> {
    queue.iter().try_fold(0u64, |total, span| {
        total
            .checked_add(span.frame_count)
            .ok_or_else(|| backend_failure("timeline queue frame count overflow").into())
    })
}

/// Проецирует полный actual pending tail: output delay + input-ahead backlog.
pub(super) fn project_pending_output_spans(
    state: &SignalsmithTimelineState,
    projection_output_queue: &mut VecDeque<SegmentFrames>,
    pcm_format: AudioTempoPcmFormat,
) -> anyhow::Result<AudioTempoOutputSegmentSpans> {
    let mut projected_remainder = state.output_frame_remainder;
    projection_output_queue.clear();
    projection_output_queue.extend(state.output_delay_queue.iter().copied());

    for pending_input_span in &state.processing_input_queue {
        let projected_output_frames = schedule_output_frames(
            &mut projected_remainder,
            pending_input_span.segment,
            pending_input_span.frame_count,
        )?;
        push_segment_frames(
            projection_output_queue,
            SegmentFrames::new(pending_input_span.segment, projected_output_frames),
        )?;
    }

    let pending_frames = sum_queue_frames(projection_output_queue)?;
    drain_segment_frames(projection_output_queue, pending_frames, pcm_format)
}

/// Склеивает process+flush spans в один ordered EOF result.
pub(super) fn concatenate_public_spans(
    first: AudioTempoOutputSegmentSpans,
    second: AudioTempoOutputSegmentSpans,
    pcm_format: AudioTempoPcmFormat,
) -> anyhow::Result<AudioTempoOutputSegmentSpans> {
    let mut queue = VecDeque::new();
    append_public_spans_to_queue(&mut queue, &first)?;
    append_public_spans_to_queue(&mut queue, &second)?;
    let frame_count = sum_queue_frames(&queue)?;
    drain_segment_frames(&mut queue, frame_count, pcm_format)
}

/// Создаёт единообразный typed backend error.
pub(super) fn backend_failure(message: impl Into<String>) -> AudioTempoProcessorError {
    AudioTempoProcessorError::BackendFailure {
        message: message.into(),
    }
}
