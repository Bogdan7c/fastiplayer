//! Reports и ordered output mapping neutral tempo boundary.
//!
//! Модуль отделяет accounting vocabulary от processor control API: static
//! input/output latency, фактический pending tail и produced spans не смешиваются.

use std::fmt;
use std::time::Duration;

use anyhow::Result;
use thiserror::Error;

use super::{
    AudioTempoFrameCount, AudioTempoPcmFormat, AudioTempoRatio, AudioTempoRatioInvalidReason,
    AudioTempoSegment, AudioTempoSegmentId,
};

/// Pair frame-count + duration в конкретной PCM timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioTempoFrameSpan {
    pub(super) frame_count: AudioTempoFrameCount,
    pub(super) duration: Duration,
}

impl AudioTempoFrameSpan {
    /// Возвращает frame count span-а.
    #[must_use]
    pub const fn frame_count(self) -> AudioTempoFrameCount {
        self.frame_count
    }

    /// Возвращает duration span-а.
    #[must_use]
    pub const fn duration(self) -> Duration {
        self.duration
    }
}

/// Named frame-counts, из которых строится report без позиционных bool/чисел.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioTempoReportFrameCounts {
    /// Decoded media frames, реально принятые processor-ом в этой операции.
    pub consumed_decoded_media: AudioTempoFrameCount,

    /// Stretched output frames, произведённые processor-ом в этой операции.
    pub produced_stretched_output: AudioTempoFrameCount,

    /// Output frames, которые остались внутри tempo processor-а после операции.
    pub pending_processor_output: AudioTempoFrameCount,

    /// Статическая задержка от decoded input до processing time на media-оси.
    pub input_latency: AudioTempoFrameCount,

    /// Статическая задержка от processing time до output на output-оси.
    pub output_latency: AudioTempoFrameCount,
}

impl AudioTempoReportFrameCounts {
    /// Создаёт полностью нулевой report frame-count set.
    pub const ZERO: Self = Self {
        consumed_decoded_media: AudioTempoFrameCount::ZERO,
        produced_stretched_output: AudioTempoFrameCount::ZERO,
        pending_processor_output: AudioTempoFrameCount::ZERO,
        input_latency: AudioTempoFrameCount::ZERO,
        output_latency: AudioTempoFrameCount::ZERO,
    };
}

/// Непрерывный output span, принадлежащий одному tempo segment-у.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioTempoOutputSegmentSpan {
    segment: AudioTempoSegment,
    stretched_output: AudioTempoFrameSpan,
}

/// Небольшая ordered collection span-ов без обязательной heap-аллокации.
///
/// Обычный packet содержит один segment, packet на rate boundary — два. Heap
/// нужен только для редкого случая нескольких быстрых transitions в одном output.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum AudioTempoOutputSegmentSpans {
    #[default]
    Empty,
    One(AudioTempoOutputSegmentSpan),
    Two([AudioTempoOutputSegmentSpan; 2]),
    Many(Vec<AudioTempoOutputSegmentSpan>),
}

impl AudioTempoOutputSegmentSpans {
    /// Добавляет span, сохраняя inline storage для типичного hot path-а.
    pub fn push(&mut self, span: AudioTempoOutputSegmentSpan) {
        let previous = std::mem::take(self);
        *self = match previous {
            Self::Empty => Self::One(span),
            Self::One(first) => Self::Two([first, span]),
            Self::Two([first, second]) => Self::Many(vec![first, second, span]),
            Self::Many(mut spans) => {
                spans.push(span);
                Self::Many(spans)
            }
        };
    }

    /// Возвращает ordered spans единым slice независимо от storage mode.
    #[must_use]
    pub fn as_slice(&self) -> &[AudioTempoOutputSegmentSpan] {
        match self {
            Self::Empty => &[],
            Self::One(span) => std::slice::from_ref(span),
            Self::Two(spans) => spans,
            Self::Many(spans) => spans,
        }
    }
}

impl From<Vec<AudioTempoOutputSegmentSpan>> for AudioTempoOutputSegmentSpans {
    fn from(mut spans: Vec<AudioTempoOutputSegmentSpan>) -> Self {
        match spans.len() {
            0 => Self::Empty,
            1 => Self::One(spans.remove(0)),
            2 => Self::Two([spans.remove(0), spans.remove(0)]),
            _ => Self::Many(spans),
        }
    }
}

impl AudioTempoOutputSegmentSpan {
    /// Создаёт output span на output timeline заданного PCM format-а.
    #[must_use]
    pub fn new(
        pcm_format: AudioTempoPcmFormat,
        segment: AudioTempoSegment,
        frame_count: AudioTempoFrameCount,
    ) -> Self {
        Self {
            segment,
            stretched_output: pcm_format.frame_span(frame_count),
        }
    }

    /// Возвращает segment, по ratio которого output clock переводится в media time.
    #[must_use]
    pub const fn segment(self) -> AudioTempoSegment {
        self.segment
    }

    /// Возвращает длину span-а на stretched-output timeline.
    #[must_use]
    pub const fn stretched_output(self) -> AudioTempoFrameSpan {
        self.stretched_output
    }
}

/// Ordered mapping output progress к media progress без знания concrete DSP.
///
/// Один вызов может сначала вернуть хвост старого segment-а, а затем samples
/// нового. Поэтому mapping намеренно не сворачивается в один `effective_ratio`.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioTempoOutputProgressMapping {
    produced_output_segments: AudioTempoOutputSegmentSpans,
    pending_output_segments: AudioTempoOutputSegmentSpans,
}

impl AudioTempoOutputProgressMapping {
    /// Создаёт mapping из ordered produced и pending spans.
    #[must_use]
    pub fn new(
        produced_output_segments: impl Into<AudioTempoOutputSegmentSpans>,
        pending_output_segments: impl Into<AudioTempoOutputSegmentSpans>,
    ) -> Self {
        Self {
            produced_output_segments: produced_output_segments.into(),
            pending_output_segments: pending_output_segments.into(),
        }
    }

    /// Возвращает spans PCM, произведённого именно этой операцией.
    #[must_use]
    pub fn produced_output_segments(&self) -> &[AudioTempoOutputSegmentSpan] {
        self.produced_output_segments.as_slice()
    }

    /// Возвращает фактический ordered tail, который processor ещё не вернул.
    #[must_use]
    pub fn pending_output_segments(&self) -> &[AudioTempoOutputSegmentSpan] {
        self.pending_output_segments.as_slice()
    }
}

/// Report одной tempo processor operation и processor state на конец операции.
///
/// Report не моделирует tail уже записанного `PlayerAudioOutput::write_samples`.
/// Этот tail принадлежит audio output/clock boundary и не должен смешиваться с
/// static latency или `pending_processor_output`.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioTempoProcessReport {
    active_segment: AudioTempoSegment,
    consumed_decoded_media: AudioTempoFrameSpan,
    produced_stretched_output: AudioTempoFrameSpan,
    pending_processor_output: AudioTempoFrameSpan,
    input_latency: AudioTempoFrameSpan,
    output_latency: AudioTempoFrameSpan,
    output_progress_mapping: AudioTempoOutputProgressMapping,
}

impl AudioTempoProcessReport {
    /// Строит report и проверяет суммы ordered produced/pending spans.
    pub fn from_frame_counts(
        pcm_format: AudioTempoPcmFormat,
        active_segment: AudioTempoSegment,
        frame_counts: AudioTempoReportFrameCounts,
        output_progress_mapping: AudioTempoOutputProgressMapping,
    ) -> Result<Self> {
        validate_output_segment_frame_total(
            output_progress_mapping.produced_output_segments(),
            frame_counts.produced_stretched_output,
            AudioTempoOutputSegmentCollection::Produced,
        )?;
        validate_output_segment_frame_total(
            output_progress_mapping.pending_output_segments(),
            frame_counts.pending_processor_output,
            AudioTempoOutputSegmentCollection::Pending,
        )?;

        let consumed_decoded_media = pcm_format.frame_span(frame_counts.consumed_decoded_media);
        let produced_stretched_output =
            pcm_format.frame_span(frame_counts.produced_stretched_output);

        Ok(Self {
            active_segment,
            consumed_decoded_media,
            produced_stretched_output,
            pending_processor_output: pcm_format.frame_span(frame_counts.pending_processor_output),
            input_latency: pcm_format.frame_span(frame_counts.input_latency),
            output_latency: pcm_format.frame_span(frame_counts.output_latency),
            output_progress_mapping,
        })
    }

    /// Возвращает active segment на конец операции.
    #[must_use]
    pub const fn active_segment(&self) -> AudioTempoSegment {
        self.active_segment
    }

    /// Совместимый getter id active segment-а; output attribution берётся из mapping.
    #[must_use]
    pub const fn segment_id(&self) -> AudioTempoSegmentId {
        self.active_segment.segment_id()
    }

    /// Совместимый getter ratio active segment-а; output может содержать старый tail.
    #[must_use]
    pub const fn effective_ratio(&self) -> AudioTempoRatio {
        self.active_segment.ratio()
    }

    /// Возвращает consumed decoded media span этой processor operation.
    #[must_use]
    pub const fn consumed_decoded_media(&self) -> AudioTempoFrameSpan {
        self.consumed_decoded_media
    }

    /// Возвращает produced stretched output span этой processor operation.
    #[must_use]
    pub const fn produced_stretched_output(&self) -> AudioTempoFrameSpan {
        self.produced_stretched_output
    }

    /// Возвращает output, buffered внутри tempo processor-а после operation.
    #[must_use]
    pub const fn pending_processor_output(&self) -> AudioTempoFrameSpan {
        self.pending_processor_output
    }

    /// Возвращает static input latency на decoded-media timeline.
    #[must_use]
    pub const fn input_latency(&self) -> AudioTempoFrameSpan {
        self.input_latency
    }

    /// Возвращает static output latency на stretched-output timeline.
    #[must_use]
    pub const fn output_latency(&self) -> AudioTempoFrameSpan {
        self.output_latency
    }

    /// Возвращает ordered mapping produced и фактически pending output.
    #[must_use]
    pub const fn output_progress_mapping(&self) -> &AudioTempoOutputProgressMapping {
        &self.output_progress_mapping
    }
}

/// Проверяет, что сумма ordered spans совпадает с агрегатом report-а.
fn validate_output_segment_frame_total(
    spans: &[AudioTempoOutputSegmentSpan],
    expected_total: AudioTempoFrameCount,
    collection: AudioTempoOutputSegmentCollection,
) -> Result<()> {
    let actual_total = spans.iter().try_fold(0u64, |total, span| {
        total
            .checked_add(span.stretched_output().frame_count().get())
            .ok_or(AudioTempoProcessorError::OutputSegmentFrameCountOverflow { collection })
    })?;

    if actual_total != expected_total.get() {
        return Err(AudioTempoProcessorError::OutputSegmentFrameCountMismatch {
            collection,
            expected_frames: expected_total.get(),
            actual_frames: actual_total,
        }
        .into());
    }

    Ok(())
}

/// Stretched PCM view вместе с report-ом той же processor operation.
///
/// Samples принадлежат caller-owned reusable buffer; result не владеет `Vec` и
/// не заставляет processor аллоцировать новый buffer на каждый packet.
#[derive(Debug, PartialEq)]
pub struct AudioTempoStretchedOutput<'a> {
    interleaved_samples: &'a [f32],
    report: AudioTempoProcessReport,
}

impl<'a> AudioTempoStretchedOutput<'a> {
    /// Создаёт output и проверяет, что samples соответствуют produced output frames.
    pub fn new(
        interleaved_samples: &'a [f32],
        report: AudioTempoProcessReport,
        pcm_format: AudioTempoPcmFormat,
    ) -> Result<Self> {
        let expected_sample_count = report
            .produced_stretched_output()
            .frame_count()
            .interleaved_sample_len(pcm_format.channel_count())?;

        if interleaved_samples.len() != expected_sample_count {
            return Err(AudioTempoProcessorError::OutputFrameCountMismatch {
                expected_sample_count,
                actual_sample_count: interleaved_samples.len(),
                reported_output_frames: report.produced_stretched_output().frame_count().get(),
                channel_count: pcm_format.channel_count().get(),
            }
            .into());
        }

        Ok(Self {
            interleaved_samples,
            report,
        })
    }

    /// Возвращает interleaved stretched PCM samples.
    #[must_use]
    pub fn interleaved_samples(&self) -> &[f32] {
        self.interleaved_samples
    }

    /// Возвращает report той же processor operation.
    #[must_use]
    pub const fn report(&self) -> &AudioTempoProcessReport {
        &self.report
    }
}

/// Typed errors tempo boundary, которые caller может downcast-ить из anyhow.
#[derive(Debug, Clone, Error, PartialEq)]
pub enum AudioTempoProcessorError {
    /// Sample rate равен нулю и не может задавать PCM timeline.
    #[error("Invalid audio tempo sample rate {sample_rate_hz}: sample rate must be greater than 0")]
    InvalidSampleRate { sample_rate_hz: u32 },

    /// Channel count равен нулю и не позволяет отличить frames от samples.
    #[error(
        "Invalid audio tempo channel count {channel_count}: channel count must be greater than 0"
    )]
    InvalidChannelCount { channel_count: u32 },

    /// Ratio не finite positive.
    #[error("Invalid audio tempo ratio {multiplier}: {reason}")]
    InvalidRatio {
        multiplier: f64,
        reason: AudioTempoRatioInvalidReason,
    },

    /// Interleaved sample count не делится на channel count без остатка.
    #[error(
        "Interleaved PCM sample count {sample_count} is not divisible by channel count {channel_count}"
    )]
    InvalidInterleavedSampleCount {
        sample_count: usize,
        channel_count: u32,
    },

    /// Frame count нельзя безопасно перевести в количество interleaved samples.
    #[error(
        "Audio tempo sample count overflows for {frame_count} frames and {channel_count} channels"
    )]
    SampleCountOverflow {
        frame_count: u64,
        channel_count: u32,
    },

    /// Decoded packet имеет другой PCM format, чем processor config.
    #[error("Audio tempo PCM format mismatch: expected {expected:?}, got {actual:?}")]
    PcmFormatMismatch {
        expected: AudioTempoPcmFormat,
        actual: AudioTempoPcmFormat,
    },

    /// Position-free history prime вызван после начала текущего DSP stream-а.
    #[error("Audio tempo history can only be primed before decoded stream processing starts")]
    HistoryPrimeAfterStreamStart,

    /// Backend/factory не поддерживает запрошенный ratio.
    #[error("Audio tempo ratio {requested_ratio} is not supported by this processor")]
    UnsupportedRatio { requested_ratio: AudioTempoRatio },

    /// Processor вернул samples, которые не совпадают с reported produced frames.
    #[error(
        "Audio tempo output mismatch: reported {reported_output_frames} frames with {channel_count} channels expects {expected_sample_count} samples, got {actual_sample_count}"
    )]
    OutputFrameCountMismatch {
        expected_sample_count: usize,
        actual_sample_count: usize,
        reported_output_frames: u64,
        channel_count: u32,
    },

    /// Сумма ordered output spans переполнила счётчик frames.
    #[error("Audio tempo {collection} output segment frame count overflow")]
    OutputSegmentFrameCountOverflow {
        collection: AudioTempoOutputSegmentCollection,
    },

    /// Ordered spans не совпадают с агрегированным produced/pending count.
    #[error(
        "Audio tempo {collection} output mapping mismatch: expected {expected_frames} frames, got {actual_frames}"
    )]
    OutputSegmentFrameCountMismatch {
        collection: AudioTempoOutputSegmentCollection,
        expected_frames: u64,
        actual_frames: u64,
    },

    /// Concrete backend сообщил failure без потери typed tempo boundary.
    #[error("Audio tempo backend failed: {message}")]
    BackendFailure { message: String },
}

/// Какая ordered span collection нарушила report invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioTempoOutputSegmentCollection {
    Produced,
    Pending,
}

impl fmt::Display for AudioTempoOutputSegmentCollection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Produced => formatter.write_str("produced"),
            Self::Pending => formatter.write_str("pending"),
        }
    }
}
