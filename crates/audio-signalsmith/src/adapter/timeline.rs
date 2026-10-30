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

/// Голова очередей, которая относится к DSP latency и не выходит наружу.
///
/// Первые `output_latency + input_latency / ratio` output frames Signalsmith —
/// не новое media: у свежего processor-а это тишина нарастания, после
/// `seek`-прайминга — fade-in повтор уже сыгранной истории. Сессия же считает,
/// что первый output frame звучит в media time первого нового input frame
/// (точка привязки часов после seek/старта/прайминга). Поэтому adapter выбрасывает
/// этот prefix и не учитывает его ни в produced, ни в pending (D5).
///
/// Prefix всегда стоит строго в начале FIFO-очередей: его latency frames кладутся
/// до первого реального input. Поэтому хватает двух счётчиков, а точное число
/// выброшенных output frames получается тем же планировщиком с тем же переносом
/// дробной части, что и реальные `Stretch::process` вызовы.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct DiscardedLatencyPrefix {
    /// Input frames в голове `processing_input_queue`, ещё не обработанные DSP.
    input_frames: u64,
    /// Output frames в голове `output_delay_queue`, ещё не выданные DSP.
    output_frames: u64,
}

/// Output frames, которые реально вынуты из output delay queue одним вызовом.
pub(super) struct DrainedOutput {
    /// Сколько первых frames этого вызова — latency prefix, их нужно выбросить.
    pub(super) discarded_latency_frames: u64,
    /// Ordered spans output-а, который действительно уходит caller-у.
    pub(super) emitted_spans: AudioTempoOutputSegmentSpans,
}

/// Reusable timeline state: input-ahead queue, output-behind queue и remainders.
#[derive(Debug, Default)]
pub(super) struct SignalsmithTimelineState {
    pub(super) processing_input_queue: VecDeque<SegmentFrames>,
    pub(super) output_delay_queue: VecDeque<SegmentFrames>,
    /// Глобальный дробный output frame переносится и между packet-ами, и между segment-ами.
    output_frame_remainder: f64,
    /// Ещё не выброшенная часть DSP latency prefix-а.
    latency_prefix: DiscardedLatencyPrefix,
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
        self.latency_prefix = source.latency_prefix;
        self.latency_primed = source.latency_primed;
        self.has_decoded_media = source.has_decoded_media;
    }

    /// Сбрасывает actual pending/accounting, сохраняя выделенные capacity.
    pub(super) fn reset(&mut self) {
        self.processing_input_queue.clear();
        self.output_delay_queue.clear();
        self.output_frame_remainder = 0.0;
        self.latency_prefix = DiscardedLatencyPrefix::default();
        self.latency_primed = false;
        self.has_decoded_media = false;
    }

    /// При первом реальном input создаёт обе половины DSP latency.
    ///
    /// Обе половины сразу помечаются как выбрасываемый prefix. Это происходит один
    /// раз на lifecycle: смена скорости у уже работающего processor-а сюда не
    /// попадает и нового выброса не создаёт.
    pub(super) fn prime_latency(
        &mut self,
        segment: AudioTempoSegment,
        input_latency_frames: u64,
        output_latency_frames: u64,
    ) -> anyhow::Result<()> {
        if self.latency_primed {
            return Ok(());
        }

        // Output-половина известна сразу; input-половина станет output frames
        // только когда планировщик реально её обработает (с переносом дроби).
        self.latency_prefix = DiscardedLatencyPrefix {
            input_frames: input_latency_frames,
            output_frames: output_latency_frames,
        };
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

    /// Продвигает processing time на `input_frames` из головы input queue.
    ///
    /// Возвращает backend calls и сколько output frames они произведут. Если в
    /// голове ещё лежит latency prefix, он планируется отдельным куском: так его
    /// точная доля output (с переносом дробной части) попадает в prefix output
    /// queue и потом выбрасывается ровно по своей границе.
    pub(super) fn advance_processing_time(
        &mut self,
        input_frames: u64,
        pcm_format: AudioTempoPcmFormat,
    ) -> anyhow::Result<(SignalsmithProcessChunks, u64)> {
        let prefix_input_frames = self.latency_prefix.input_frames.min(input_frames);
        let mut process_chunks = SignalsmithProcessChunks::default();

        let prefix_output_frames =
            self.schedule_and_enqueue(prefix_input_frames, &mut process_chunks, pcm_format)?;
        self.latency_prefix.input_frames -= prefix_input_frames;
        self.latency_prefix.output_frames = self
            .latency_prefix
            .output_frames
            .checked_add(prefix_output_frames)
            .ok_or_else(|| backend_failure("latency prefix output frame count overflow"))?;

        let media_output_frames = self.schedule_and_enqueue(
            input_frames - prefix_input_frames,
            &mut process_chunks,
            pcm_format,
        )?;
        let scheduled_output_frames = prefix_output_frames
            .checked_add(media_output_frames)
            .ok_or_else(|| backend_failure("scheduled output frame count overflow"))?;
        Ok((process_chunks, scheduled_output_frames))
    }

    /// Планирует `input_frames` из головы input queue и ставит их output в delay queue.
    fn schedule_and_enqueue(
        &mut self,
        input_frames: u64,
        process_chunks: &mut SignalsmithProcessChunks,
        pcm_format: AudioTempoPcmFormat,
    ) -> anyhow::Result<u64> {
        let processing_spans =
            drain_segment_frames(&mut self.processing_input_queue, input_frames, pcm_format)?;
        let (span_chunks, scheduled_spans) =
            self.schedule_processing_spans(&processing_spans, pcm_format)?;
        for chunk in span_chunks.as_slice() {
            process_chunks.push(*chunk);
        }
        append_public_spans_to_queue(&mut self.output_delay_queue, &scheduled_spans)?;
        sum_public_span_frames(&scheduled_spans)
    }

    /// Вынимает `output_frames`, которые DSP выдаёт сейчас, отделяя latency prefix.
    ///
    /// Prefix стоит строго в начале output queue, поэтому выброшенные frames всегда
    /// идут первыми в буфере вызова, а caller получает только новый media output.
    pub(super) fn drain_produced_output(
        &mut self,
        output_frames: u64,
        pcm_format: AudioTempoPcmFormat,
    ) -> anyhow::Result<DrainedOutput> {
        let discarded_latency_frames = self.latency_prefix.output_frames.min(output_frames);
        drain_segment_frames(
            &mut self.output_delay_queue,
            discarded_latency_frames,
            pcm_format,
        )?;
        self.latency_prefix.output_frames -= discarded_latency_frames;
        let emitted_spans = drain_segment_frames(
            &mut self.output_delay_queue,
            output_frames - discarded_latency_frames,
            pcm_format,
        )?;
        Ok(DrainedOutput {
            discarded_latency_frames,
            emitted_spans,
        })
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
///
/// Ещё не выброшенный latency prefix в pending не входит: caller его никогда не
/// получит, а сессия строит по pending spans media-time будущего output-а.
pub(super) fn project_pending_output_spans(
    state: &SignalsmithTimelineState,
    projection_output_queue: &mut VecDeque<SegmentFrames>,
    pcm_format: AudioTempoPcmFormat,
) -> anyhow::Result<AudioTempoOutputSegmentSpans> {
    let mut projected_remainder = state.output_frame_remainder;
    projection_output_queue.clear();
    projection_output_queue.extend(state.output_delay_queue.iter().copied());
    let mut prefix_input_frames_left = state.latency_prefix.input_frames;
    let mut prefix_output_frames = state.latency_prefix.output_frames;

    for pending_input_span in &state.processing_input_queue {
        // Prefix и media планируются раздельно — так же, как `advance_processing_time`.
        let segment = pending_input_span.segment;
        let span_prefix_input_frames = prefix_input_frames_left.min(pending_input_span.frame_count);
        prefix_input_frames_left -= span_prefix_input_frames;

        let projected_prefix_frames =
            schedule_output_frames(&mut projected_remainder, segment, span_prefix_input_frames)?;
        prefix_output_frames = prefix_output_frames
            .checked_add(projected_prefix_frames)
            .ok_or_else(|| backend_failure("projected latency prefix frame count overflow"))?;
        push_segment_frames(
            projection_output_queue,
            SegmentFrames::new(segment, projected_prefix_frames),
        )?;

        let projected_media_frames = schedule_output_frames(
            &mut projected_remainder,
            segment,
            pending_input_span.frame_count - span_prefix_input_frames,
        )?;
        push_segment_frames(
            projection_output_queue,
            SegmentFrames::new(segment, projected_media_frames),
        )?;
    }

    let pending_frames = sum_queue_frames(projection_output_queue)?;
    drain_segment_frames(projection_output_queue, prefix_output_frames, pcm_format)?;
    drain_segment_frames(
        projection_output_queue,
        pending_frames - prefix_output_frames,
        pcm_format,
    )
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
