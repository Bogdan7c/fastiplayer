//! Нейтральная audio tempo boundary для будущего processor adapter-а.
//!
//! Этот модуль намеренно не зависит от concrete tempo backend-а. Он фиксирует
//! vocabulary для `player-core`: decoded media frames на входе, stretched output
//! frames на выходе, latency/pending state внутри processor-а и mapping data,
//! по которым позже можно связать output-progress с media-progress.
//! Concrete adapter лучше добавлять отдельным crate-ом `audio-timestretch`.
//! В concrete `audio` crate это допустимо только если tempo ownership не
//! смешивается с decoder и CPAL output ownership.

use std::fmt;
use std::time::Duration;

use anyhow::Result;
use thiserror::Error;

use crate::AudioOutputSpec;

const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Количество PCM frames, где один frame содержит sample для каждого канала.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioTempoFrameCount(u64);

impl AudioTempoFrameCount {
    /// Нулевое количество frames для report-ов без движения по audio timeline.
    pub const ZERO: Self = Self(0);

    /// Создаёт typed frame count без смешивания с количеством interleaved samples.
    #[must_use]
    pub const fn new(frames: u64) -> Self {
        Self(frames)
    }

    /// Возвращает количество PCM frames.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Считает ожидаемое количество interleaved samples для заданного channel count.
    pub fn interleaved_sample_len(self, channel_count: AudioTempoChannelCount) -> Result<usize> {
        let frame_count =
            usize::try_from(self.0).map_err(|_| AudioTempoProcessorError::SampleCountOverflow {
                frame_count: self.0,
                channel_count: channel_count.get(),
            })?;
        let channel_count = usize::try_from(channel_count.get()).map_err(|_| {
            AudioTempoProcessorError::SampleCountOverflow {
                frame_count: self.0,
                channel_count: channel_count.get(),
            }
        })?;

        frame_count.checked_mul(channel_count).ok_or_else(|| {
            AudioTempoProcessorError::SampleCountOverflow {
                frame_count: self.0,
                channel_count: u32::try_from(channel_count).unwrap_or(u32::MAX),
            }
            .into()
        })
    }

    fn from_interleaved_sample_len(
        sample_count: usize,
        channel_count: AudioTempoChannelCount,
    ) -> Result<Self> {
        let channel_count_usize = usize::try_from(channel_count.get()).map_err(|_| {
            AudioTempoProcessorError::SampleCountOverflow {
                frame_count: u64::MAX,
                channel_count: channel_count.get(),
            }
        })?;

        if sample_count % channel_count_usize != 0 {
            return Err(AudioTempoProcessorError::InvalidInterleavedSampleCount {
                sample_count,
                channel_count: channel_count.get(),
            }
            .into());
        }

        let frame_count = sample_count / channel_count_usize;
        Ok(Self(u64::try_from(frame_count).map_err(|_| {
            AudioTempoProcessorError::SampleCountOverflow {
                frame_count: u64::MAX,
                channel_count: channel_count.get(),
            }
        })?))
    }
}

/// Sample rate PCM stream-а, который tempo processor получает и отдаёт.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioTempoSampleRateHz(u32);

impl AudioTempoSampleRateHz {
    /// Создаёт sample-rate value и отбрасывает нулевой rate до factory/backend-а.
    pub fn new(sample_rate_hz: u32) -> Result<Self> {
        if sample_rate_hz == 0 {
            return Err(AudioTempoProcessorError::InvalidSampleRate { sample_rate_hz }.into());
        }
        Ok(Self(sample_rate_hz))
    }

    /// Возвращает sample rate в Hz.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    fn duration_for_frames(self, frame_count: AudioTempoFrameCount) -> Duration {
        let full_seconds = frame_count.get() / u64::from(self.0);
        let remainder_frames = frame_count.get() % u64::from(self.0);
        let remainder_nanos = (remainder_frames * NANOS_PER_SECOND) / u64::from(self.0);

        Duration::new(full_seconds, remainder_nanos as u32)
    }
}

/// Количество channels в interleaved PCM stream-е.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioTempoChannelCount(u32);

impl AudioTempoChannelCount {
    /// Создаёт channel-count value и не даёт принять sample count за frame count.
    pub fn new(channel_count: u32) -> Result<Self> {
        if channel_count == 0 {
            return Err(AudioTempoProcessorError::InvalidChannelCount { channel_count }.into());
        }
        Ok(Self(channel_count))
    }

    /// Возвращает количество interleaved channels.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// PCM format, на котором tempo processor принимает decoded media и отдаёт output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioTempoPcmFormat {
    sample_rate_hz: AudioTempoSampleRateHz,
    channel_count: AudioTempoChannelCount,
}

impl AudioTempoPcmFormat {
    /// Создаёт format из уже проверенных typed частей.
    #[must_use]
    pub const fn new(
        sample_rate_hz: AudioTempoSampleRateHz,
        channel_count: AudioTempoChannelCount,
    ) -> Self {
        Self {
            sample_rate_hz,
            channel_count,
        }
    }

    /// Создаёт tempo PCM format из decoded output spec-а audio decoder-а.
    pub fn from_audio_output_spec(spec: AudioOutputSpec) -> Result<Self> {
        Ok(Self::new(
            AudioTempoSampleRateHz::new(spec.sample_rate)?,
            AudioTempoChannelCount::new(spec.channels)?,
        ))
    }

    /// Возвращает sample rate, которым считаются media/output durations.
    #[must_use]
    pub const fn sample_rate_hz(self) -> AudioTempoSampleRateHz {
        self.sample_rate_hz
    }

    /// Возвращает channel count, которым interleaved sample count переводится в frames.
    #[must_use]
    pub const fn channel_count(self) -> AudioTempoChannelCount {
        self.channel_count
    }

    /// Создаёт span с duration, рассчитанной из frame count и sample rate.
    #[must_use]
    pub fn frame_span(self, frame_count: AudioTempoFrameCount) -> AudioTempoFrameSpan {
        AudioTempoFrameSpan {
            frame_count,
            duration: self.sample_rate_hz.duration_for_frames(frame_count),
        }
    }
}

/// Playback tempo ratio: media-progress per output-progress внутри segment-а.
///
/// `2.0x` означает, что 10 ms decoded media могут дать примерно 5 ms output.
/// `0.5x` означает, что 10 ms decoded media могут дать примерно 20 ms output.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct AudioTempoRatio {
    multiplier: f64,
}

impl AudioTempoRatio {
    /// Normal speed: media duration и output duration совпадают.
    pub const NORMAL: Self = Self { multiplier: 1.0 };

    /// Создаёт positive finite tempo ratio без project-specific playback-rate range.
    pub fn new(multiplier: f64) -> Result<Self> {
        if !multiplier.is_finite() {
            return Err(AudioTempoProcessorError::InvalidRatio {
                multiplier,
                reason: AudioTempoRatioInvalidReason::NotFinite,
            }
            .into());
        }
        if multiplier <= 0.0 {
            return Err(AudioTempoProcessorError::InvalidRatio {
                multiplier,
                reason: AudioTempoRatioInvalidReason::NonPositive,
            }
            .into());
        }
        Ok(Self { multiplier })
    }

    /// Возвращает media-progress per output-progress multiplier.
    #[must_use]
    pub const fn as_f64(self) -> f64 {
        self.multiplier
    }

    /// Проверяет normal-speed segment без знания о playback-rate owner-е.
    #[must_use]
    pub fn is_normal(self) -> bool {
        self == Self::NORMAL
    }
}

impl fmt::Display for AudioTempoRatio {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}x", self.multiplier)
    }
}

/// Причина отказа при создании `AudioTempoRatio`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioTempoRatioInvalidReason {
    /// Ratio был NaN или infinity.
    NotFinite,

    /// Ratio был нулевым или отрицательным.
    NonPositive,
}

impl fmt::Display for AudioTempoRatioInvalidReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFinite => formatter.write_str("ratio must be finite"),
            Self::NonPositive => formatter.write_str("ratio must be greater than 0"),
        }
    }
}

/// Stable id одного tempo segment-а для будущего clock/progress mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioTempoSegmentId(u64);

impl AudioTempoSegmentId {
    /// Создаёт segment id; монотонность id остаётся ответственностью caller-а.
    #[must_use]
    pub const fn new(segment_id: u64) -> Self {
        Self(segment_id)
    }

    /// Возвращает raw id для diagnostics/snapshots.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Tempo segment связывает ratio с id, по которому caller может копить mapping.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioTempoSegment {
    segment_id: AudioTempoSegmentId,
    ratio: AudioTempoRatio,
}

impl AudioTempoSegment {
    /// Создаёт segment с явно заданным id и effective ratio.
    #[must_use]
    pub const fn new(segment_id: AudioTempoSegmentId, ratio: AudioTempoRatio) -> Self {
        Self { segment_id, ratio }
    }

    /// Возвращает id segment-а.
    #[must_use]
    pub const fn segment_id(self) -> AudioTempoSegmentId {
        self.segment_id
    }

    /// Возвращает effective ratio, применённый processor-ом.
    #[must_use]
    pub const fn ratio(self) -> AudioTempoRatio {
        self.ratio
    }
}

/// Конфигурация создания tempo processor-а без concrete backend dependency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioTempoProcessorConfig {
    pcm_format: AudioTempoPcmFormat,
    initial_segment: AudioTempoSegment,
}

impl AudioTempoProcessorConfig {
    /// Создаёт config для processor-а, который не меняет sample rate/channels.
    #[must_use]
    pub const fn new(pcm_format: AudioTempoPcmFormat, initial_segment: AudioTempoSegment) -> Self {
        Self {
            pcm_format,
            initial_segment,
        }
    }

    /// Возвращает PCM format decoded media input-а и stretched output-а.
    #[must_use]
    pub const fn pcm_format(self) -> AudioTempoPcmFormat {
        self.pcm_format
    }

    /// Возвращает первый tempo segment, с которым processor стартует.
    #[must_use]
    pub const fn initial_segment(self) -> AudioTempoSegment {
        self.initial_segment
    }
}

/// Decoded media PCM, который tempo processor ещё не растягивал.
#[derive(Debug, Clone, Copy)]
pub struct AudioTempoDecodedMedia<'a> {
    interleaved_samples: &'a [f32],
    frame_count: AudioTempoFrameCount,
}

impl<'a> AudioTempoDecodedMedia<'a> {
    /// Создаёт decoded-media input и валидирует `samples.len() % channels == 0`.
    pub fn from_interleaved_samples(
        interleaved_samples: &'a [f32],
        pcm_format: AudioTempoPcmFormat,
    ) -> Result<Self> {
        Ok(Self {
            interleaved_samples,
            frame_count: AudioTempoFrameCount::from_interleaved_sample_len(
                interleaved_samples.len(),
                pcm_format.channel_count(),
            )?,
        })
    }

    /// Возвращает исходные interleaved decoded PCM samples.
    #[must_use]
    pub const fn interleaved_samples(self) -> &'a [f32] {
        self.interleaved_samples
    }

    /// Возвращает decoded media frames, а не sample count.
    #[must_use]
    pub const fn frame_count(self) -> AudioTempoFrameCount {
        self.frame_count
    }
}

/// Pair frame-count + duration в конкретной PCM timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioTempoFrameSpan {
    frame_count: AudioTempoFrameCount,
    duration: Duration,
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

    /// Algorithmic latency tempo processor-а после операции.
    pub processor_latency: AudioTempoFrameCount,
}

impl AudioTempoReportFrameCounts {
    /// Создаёт полностью нулевой report frame-count set.
    pub const ZERO: Self = Self {
        consumed_decoded_media: AudioTempoFrameCount::ZERO,
        produced_stretched_output: AudioTempoFrameCount::ZERO,
        pending_processor_output: AudioTempoFrameCount::ZERO,
        processor_latency: AudioTempoFrameCount::ZERO,
    };
}

/// Данные, по которым caller может связать output progress с media progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioTempoOutputProgressMapping {
    segment_id: AudioTempoSegmentId,
    effective_ratio: AudioTempoRatio,
    consumed_decoded_media: AudioTempoFrameSpan,
    produced_stretched_output: AudioTempoFrameSpan,
}

impl AudioTempoOutputProgressMapping {
    /// Возвращает tempo segment, к которому относится mapping.
    #[must_use]
    pub const fn segment_id(self) -> AudioTempoSegmentId {
        self.segment_id
    }

    /// Возвращает media-progress per output-progress ratio.
    #[must_use]
    pub const fn effective_ratio(self) -> AudioTempoRatio {
        self.effective_ratio
    }

    /// Возвращает decoded media span, накопленный в этой processor operation.
    #[must_use]
    pub const fn consumed_decoded_media(self) -> AudioTempoFrameSpan {
        self.consumed_decoded_media
    }

    /// Возвращает stretched output span, накопленный в этой processor operation.
    #[must_use]
    pub const fn produced_stretched_output(self) -> AudioTempoFrameSpan {
        self.produced_stretched_output
    }
}

/// Report одной tempo processor operation и processor state на конец операции.
///
/// Report не моделирует tail уже записанного `PlayerAudioOutput::write_samples`.
/// Этот tail принадлежит audio output/clock boundary и не должен смешиваться с
/// `processor_latency` или `pending_processor_output`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioTempoProcessReport {
    segment_id: AudioTempoSegmentId,
    effective_ratio: AudioTempoRatio,
    consumed_decoded_media: AudioTempoFrameSpan,
    produced_stretched_output: AudioTempoFrameSpan,
    pending_processor_output: AudioTempoFrameSpan,
    processor_latency: AudioTempoFrameSpan,
    output_progress_mapping: AudioTempoOutputProgressMapping,
}

impl AudioTempoProcessReport {
    /// Строит report из named frame-counts и PCM format-а.
    #[must_use]
    pub fn from_frame_counts(
        pcm_format: AudioTempoPcmFormat,
        segment: AudioTempoSegment,
        frame_counts: AudioTempoReportFrameCounts,
    ) -> Self {
        let consumed_decoded_media = pcm_format.frame_span(frame_counts.consumed_decoded_media);
        let produced_stretched_output =
            pcm_format.frame_span(frame_counts.produced_stretched_output);

        Self {
            segment_id: segment.segment_id(),
            effective_ratio: segment.ratio(),
            consumed_decoded_media,
            produced_stretched_output,
            pending_processor_output: pcm_format.frame_span(frame_counts.pending_processor_output),
            processor_latency: pcm_format.frame_span(frame_counts.processor_latency),
            output_progress_mapping: AudioTempoOutputProgressMapping {
                segment_id: segment.segment_id(),
                effective_ratio: segment.ratio(),
                consumed_decoded_media,
                produced_stretched_output,
            },
        }
    }

    /// Возвращает tempo segment id report-а.
    #[must_use]
    pub const fn segment_id(self) -> AudioTempoSegmentId {
        self.segment_id
    }

    /// Возвращает effective ratio, реально связанный с report-ом.
    #[must_use]
    pub const fn effective_ratio(self) -> AudioTempoRatio {
        self.effective_ratio
    }

    /// Возвращает consumed decoded media span этой processor operation.
    #[must_use]
    pub const fn consumed_decoded_media(self) -> AudioTempoFrameSpan {
        self.consumed_decoded_media
    }

    /// Возвращает produced stretched output span этой processor operation.
    #[must_use]
    pub const fn produced_stretched_output(self) -> AudioTempoFrameSpan {
        self.produced_stretched_output
    }

    /// Возвращает output, buffered внутри tempo processor-а после operation.
    #[must_use]
    pub const fn pending_processor_output(self) -> AudioTempoFrameSpan {
        self.pending_processor_output
    }

    /// Возвращает algorithmic latency tempo processor-а после operation.
    #[must_use]
    pub const fn processor_latency(self) -> AudioTempoFrameSpan {
        self.processor_latency
    }

    /// Возвращает минимальный mapping для output-progress -> media-progress.
    #[must_use]
    pub const fn output_progress_mapping(self) -> AudioTempoOutputProgressMapping {
        self.output_progress_mapping
    }
}

/// Stretched PCM output вместе с report-ом той же processor operation.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioTempoStretchedOutput {
    interleaved_samples: Vec<f32>,
    report: AudioTempoProcessReport,
}

impl AudioTempoStretchedOutput {
    /// Создаёт output и проверяет, что samples соответствуют produced output frames.
    pub fn new(
        interleaved_samples: Vec<f32>,
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
        &self.interleaved_samples
    }

    /// Возвращает report той же processor operation.
    #[must_use]
    pub const fn report(&self) -> AudioTempoProcessReport {
        self.report
    }
}

/// Владеющий handle neutral tempo processor-а.
pub type AudioTempoProcessorHandle = Box<dyn AudioTempoProcessor + Send>;

/// Нейтральный audio tempo processor без знания о concrete tempo crate.
pub trait AudioTempoProcessor: Send {
    /// Принимает decoded media PCM и возвращает stretched output PCM.
    fn process_decoded_media(
        &mut self,
        decoded_media: AudioTempoDecodedMedia<'_>,
    ) -> Result<AudioTempoStretchedOutput>;

    /// Дренирует buffered output processor-а без принятия новых decoded media frames.
    fn flush(&mut self) -> Result<AudioTempoStretchedOutput>;

    /// Сбрасывает algorithmic state после seek/discontinuity и не пишет output samples.
    fn reset(&mut self) -> Result<AudioTempoProcessReport>;
}

/// Нейтральная factory tempo processor-а для composition layer-а.
pub trait AudioTempoProcessorFactory: Send + Sync {
    /// Создаёт tempo processor под decoded PCM format и initial segment.
    fn create_processor(
        &self,
        config: AudioTempoProcessorConfig,
    ) -> Result<AudioTempoProcessorHandle>;
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

    /// Concrete backend сообщил failure без потери typed tempo boundary.
    #[error("Audio tempo backend failed: {message}")]
    BackendFailure { message: String },
}

#[cfg(test)]
mod tests;
