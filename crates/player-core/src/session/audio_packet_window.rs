use std::fmt;

use audio_core::AudioPacketTiming;
use media_core::{PacketPresentationWindow, TrackId};

use super::audio_playback_bounds::DecodedAudioPlaybackFrameRange;

/// Непрерывный полуоткрытый диапазон PCM frames в исходном decoded block-е.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DecodedPcmFrameRange {
    /// Первый retained frame.
    start: usize,

    /// Первый frame после retained диапазона.
    end_exclusive: usize,
}

impl DecodedPcmFrameRange {
    /// Создаёт полный диапазон decoded block-а.
    #[must_use]
    pub(super) const fn full(frame_count: usize) -> Self {
        Self {
            start: 0,
            end_exclusive: frame_count,
        }
    }

    /// Создаёт упорядоченный диапазон либо пустой диапазон в точке пересечения.
    #[must_use]
    pub(super) const fn ordered_or_empty(start: usize, end_exclusive: usize) -> Self {
        if start < end_exclusive {
            Self {
                start,
                end_exclusive,
            }
        } else {
            Self {
                start,
                end_exclusive: start,
            }
        }
    }

    /// Возвращает пересечение двух диапазонов в одном decoded-frame domain.
    #[must_use]
    pub(super) const fn intersect(self, other: Self) -> Self {
        let start = if self.start > other.start {
            self.start
        } else {
            other.start
        };
        let end_exclusive = if self.end_exclusive < other.end_exclusive {
            self.end_exclusive
        } else {
            other.end_exclusive
        };
        Self::ordered_or_empty(start, end_exclusive)
    }

    /// Проверяет отсутствие retained frames.
    #[must_use]
    pub(super) const fn is_empty(self) -> bool {
        self.start == self.end_exclusive
    }

    /// Делает единственный borrowed interleaved slice после композиции ranges.
    pub(super) fn slice_interleaved(
        self,
        decoded_samples: &[f32],
        channels: u32,
    ) -> Result<&[f32], DecodedPcmPacketClipError> {
        if channels == 0 {
            return Err(DecodedPcmPacketClipError::ZeroChannels);
        }
        let channel_count = usize::try_from(channels)
            .map_err(|_| DecodedPcmPacketClipError::ChannelCountDoesNotFitPlatform { channels })?;
        let first_sample = self.start.checked_mul(channel_count).ok_or(
            DecodedPcmPacketClipError::SampleIndexOverflow {
                frame_index: self.start,
                channels,
            },
        )?;
        let end_sample = self.end_exclusive.checked_mul(channel_count).ok_or(
            DecodedPcmPacketClipError::SampleIndexOverflow {
                frame_index: self.end_exclusive,
                channels,
            },
        )?;
        if end_sample > decoded_samples.len() {
            return Err(DecodedPcmPacketClipError::SampleRangeOutOfBounds {
                end_sample,
                sample_count: decoded_samples.len(),
            });
        }
        Ok(&decoded_samples[first_sample..end_sample])
    }
}

/// Результат применения exact packet window до global/CUE композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DecodedPcmPacketClipOutcome {
    /// Старый путь не требует ни raw timing, ни нового PCM validation.
    Unbounded,

    /// Bounded packet сохраняет этот диапазон в исходном decoded block-е.
    Retained(DecodedPcmFrameRange),

    /// Exact packet window не содержит ни одного decoded frame.
    FullyDropped,
}

/// Фаза exact arithmetic, на которой обнаружено переполнение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DecodedPcmClipArithmeticOperation {
    /// Умножение signed delta на time-base numerator.
    ScaleByTimeBaseNumerator,

    /// Умножение timestamp scale на decoded sample rate.
    ScaleBySampleRate,

    /// Добавление единицы при exact ceil division.
    RoundTowardPositiveInfinity,
}

/// Typed отказ bounded clipping до любой мутации audio output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DecodedPcmPacketClipError {
    /// Bounded packet не сохранил raw container time base.
    MissingPacketTimeBase,

    /// Packet track не совпадает с authoritative window track.
    TrackMismatch {
        /// Track decoded packet-а.
        packet_track_id: TrackId,

        /// Track exact presentation window.
        window_track_id: TrackId,
    },

    /// Raw packet clock не совпадает с authoritative window clock.
    TimeBaseMismatch,

    /// Decoded output сообщил нулевой sample rate.
    ZeroSampleRate,

    /// Decoded output сообщил нулевое число каналов.
    ZeroChannels,

    /// Interleaved PCM содержит неполный frame.
    NonInterleavedSampleCount {
        /// Число decoded samples.
        sample_count: usize,

        /// Число каналов decoded output.
        channels: u32,
    },

    /// Exact rational arithmetic переполнила `i128`.
    ArithmeticOverflow {
        /// Операция, которая не представима точно.
        operation: DecodedPcmClipArithmeticOperation,
    },

    /// Число каналов не представимо индексом платформы.
    ChannelCountDoesNotFitPlatform {
        /// Исходное число каналов.
        channels: u32,
    },

    /// Frame-to-sample conversion переполнил `usize`.
    SampleIndexOverflow {
        /// Frame index до умножения.
        frame_index: usize,

        /// Число interleaved каналов.
        channels: u32,
    },

    /// Итоговый sample range вышел за decoded block.
    SampleRangeOutOfBounds {
        /// Исключённая правая sample-граница.
        end_sample: usize,

        /// Фактическое число decoded samples.
        sample_count: usize,
    },
}

impl fmt::Display for DecodedPcmPacketClipError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPacketTimeBase => {
                write!(formatter, "bounded audio packet не содержит raw time base")
            }
            Self::TrackMismatch { .. } => {
                write!(
                    formatter,
                    "audio packet track не совпадает с presentation window"
                )
            }
            Self::TimeBaseMismatch => {
                write!(
                    formatter,
                    "audio packet time base не совпадает с presentation window"
                )
            }
            Self::ZeroSampleRate => write!(formatter, "decoded audio sample rate равен нулю"),
            Self::ZeroChannels => write!(formatter, "decoded audio channel count равен нулю"),
            Self::NonInterleavedSampleCount { .. } => {
                write!(formatter, "decoded audio samples не образуют полные frames")
            }
            Self::ArithmeticOverflow { .. } => {
                write!(formatter, "exact audio packet clipping переполнил i128")
            }
            Self::ChannelCountDoesNotFitPlatform { .. } => {
                write!(formatter, "audio channel count не помещается в usize")
            }
            Self::SampleIndexOverflow { .. } => {
                write!(formatter, "PCM frame-to-sample index переполнен")
            }
            Self::SampleRangeOutOfBounds { .. } => {
                write!(formatter, "PCM sample range вышел за decoded block")
            }
        }
    }
}

impl std::error::Error for DecodedPcmPacketClipError {}

/// Пересекает packet/global ranges до единственного interleaved slice-а.
pub(super) fn compose_decoded_pcm_ranges(
    decoded_samples: &[f32],
    packet_clip: DecodedPcmPacketClipOutcome,
    global_range: DecodedAudioPlaybackFrameRange,
    channels: u32,
) -> Result<&[f32], DecodedPcmPacketClipError> {
    match global_range {
        DecodedAudioPlaybackFrameRange::PreserveOriginalSamples => match packet_clip {
            DecodedPcmPacketClipOutcome::Unbounded => Ok(decoded_samples),
            DecodedPcmPacketClipOutcome::Retained(packet_range) => {
                packet_range.slice_interleaved(decoded_samples, channels)
            }
            DecodedPcmPacketClipOutcome::FullyDropped => Ok(&[]),
        },
        DecodedAudioPlaybackFrameRange::Frames(global_frames) => {
            let retained_frames = match packet_clip {
                DecodedPcmPacketClipOutcome::Unbounded => global_frames,
                DecodedPcmPacketClipOutcome::Retained(packet_frames) => {
                    packet_frames.intersect(global_frames)
                }
                DecodedPcmPacketClipOutcome::FullyDropped => {
                    return Ok(&[]);
                }
            };
            retained_frames.slice_interleaved(decoded_samples, channels)
        }
    }
}

/// Внутренняя граница, на которой decoded PCM всё ещё связан с exact packet window.
pub(super) struct DecodedPcmPacketBoundary<'samples> {
    /// PCM, который decoder вернул для одного encoded packet-а.
    decoded_samples: &'samples [f32],

    /// Exact presentation window исходного encoded packet-а.
    presentation_window: PacketPresentationWindow,
}

impl<'samples> DecodedPcmPacketBoundary<'samples> {
    /// Связывает decoded PCM с exact packet metadata.
    #[must_use]
    pub(super) const fn new(
        decoded_samples: &'samples [f32],
        presentation_window: PacketPresentationWindow,
    ) -> Self {
        Self {
            decoded_samples,
            presentation_window,
        }
    }

    /// Планирует exact clipping в исходном decoded-frame domain.
    pub(super) fn plan_clip(
        self,
        packet_track_id: TrackId,
        packet_timing: AudioPacketTiming,
        sample_rate: u32,
        channels: u32,
    ) -> Result<DecodedPcmPacketClipOutcome, DecodedPcmPacketClipError> {
        let PacketPresentationWindow::Bounded(window) = self.presentation_window else {
            return Ok(DecodedPcmPacketClipOutcome::Unbounded);
        };
        if sample_rate == 0 {
            return Err(DecodedPcmPacketClipError::ZeroSampleRate);
        }
        if channels == 0 {
            return Err(DecodedPcmPacketClipError::ZeroChannels);
        }
        let channel_count = usize::try_from(channels)
            .map_err(|_| DecodedPcmPacketClipError::ChannelCountDoesNotFitPlatform { channels })?;
        if !self.decoded_samples.len().is_multiple_of(channel_count) {
            return Err(DecodedPcmPacketClipError::NonInterleavedSampleCount {
                sample_count: self.decoded_samples.len(),
                channels,
            });
        }
        let packet_time_base = packet_timing
            .time_base()
            .ok_or(DecodedPcmPacketClipError::MissingPacketTimeBase)?;
        let window_start = window.start();
        if window_start.track_id != packet_track_id {
            return Err(DecodedPcmPacketClipError::TrackMismatch {
                packet_track_id,
                window_track_id: window_start.track_id,
            });
        }
        if packet_time_base.numer() != window_start.time_base.numer
            || packet_time_base.denom() != window_start.time_base.denom
        {
            return Err(DecodedPcmPacketClipError::TimeBaseMismatch);
        }

        let decoded_frame_count = self.decoded_samples.len() / channel_count;
        let first_frame = exact_boundary_frame_index(
            window_start.units.get(),
            packet_timing.pts_units(),
            packet_time_base.numer(),
            packet_time_base.denom(),
            sample_rate,
            decoded_frame_count,
        )?;
        let end_frame = exact_boundary_frame_index(
            window.end_exclusive().units.get(),
            packet_timing.pts_units(),
            packet_time_base.numer(),
            packet_time_base.denom(),
            sample_rate,
            decoded_frame_count,
        )?;
        let retained = DecodedPcmFrameRange::ordered_or_empty(first_frame, end_frame);
        if retained.is_empty() {
            Ok(DecodedPcmPacketClipOutcome::FullyDropped)
        } else {
            Ok(DecodedPcmPacketClipOutcome::Retained(retained))
        }
    }
}

/// Считает `ceil((boundary - packet_pts) * numer * rate / denom)` без потери точности.
fn exact_boundary_frame_index(
    boundary_units: i64,
    packet_pts_units: i64,
    time_base_numer: u32,
    time_base_denom: u32,
    sample_rate: u32,
    decoded_frame_count: usize,
) -> Result<usize, DecodedPcmPacketClipError> {
    let mut delta = i128::from(boundary_units) - i128::from(packet_pts_units);
    let mut numerator_factor = u128::from(time_base_numer);
    let mut sample_rate_factor = u128::from(sample_rate);
    let mut denominator = u128::from(time_base_denom);

    let delta_divisor = greatest_common_divisor(delta.unsigned_abs(), denominator);
    delta /= delta_divisor as i128;
    denominator /= delta_divisor;

    let numerator_divisor = greatest_common_divisor(numerator_factor, denominator);
    numerator_factor /= numerator_divisor;
    denominator /= numerator_divisor;

    let sample_rate_divisor = greatest_common_divisor(sample_rate_factor, denominator);
    sample_rate_factor /= sample_rate_divisor;
    denominator /= sample_rate_divisor;

    let scaled_by_time_base = delta.checked_mul(numerator_factor as i128).ok_or(
        DecodedPcmPacketClipError::ArithmeticOverflow {
            operation: DecodedPcmClipArithmeticOperation::ScaleByTimeBaseNumerator,
        },
    )?;
    let exact_numerator = scaled_by_time_base
        .checked_mul(sample_rate_factor as i128)
        .ok_or(DecodedPcmPacketClipError::ArithmeticOverflow {
            operation: DecodedPcmClipArithmeticOperation::ScaleBySampleRate,
        })?;
    let exact_denominator = denominator as i128;
    let frame_index = ceil_signed_ratio(exact_numerator, exact_denominator)?;

    if frame_index <= 0 {
        return Ok(0);
    }
    if frame_index >= decoded_frame_count as i128 {
        return Ok(decoded_frame_count);
    }
    Ok(frame_index as usize)
}

/// Делит signed numerator на положительный denominator с округлением к `+∞`.
fn ceil_signed_ratio(
    numerator: i128,
    positive_denominator: i128,
) -> Result<i128, DecodedPcmPacketClipError> {
    let truncated = numerator / positive_denominator;
    let remainder = numerator % positive_denominator;
    if remainder > 0 {
        truncated
            .checked_add(1)
            .ok_or(DecodedPcmPacketClipError::ArithmeticOverflow {
                operation: DecodedPcmClipArithmeticOperation::RoundTowardPositiveInfinity,
            })
    } else {
        Ok(truncated)
    }
}

/// Находит НОД для сокращения rational factors до checked multiplication.
fn greatest_common_divisor(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[cfg(test)]
mod tests {
    use audio_core::{AudioPacketTimeBase, AudioPacketTiming};
    use media_core::{
        ExactPresentationWindow, PacketPresentationWindow, TimeBase, TrackId, TrackTimestamp,
    };

    use super::{
        DecodedPcmClipArithmeticOperation, DecodedPcmFrameRange, DecodedPcmPacketBoundary,
        DecodedPcmPacketClipError, DecodedPcmPacketClipOutcome,
    };

    /// Строит bounded window для произвольного track clock-а.
    fn bounded_window(
        track_id: TrackId,
        start: i64,
        end_exclusive: i64,
        numer: u32,
        denom: u32,
    ) -> PacketPresentationWindow {
        let time_base = TimeBase::new(numer, denom).expect("valid test time base");
        PacketPresentationWindow::Bounded(
            ExactPresentationWindow::new(
                TrackTimestamp::new(track_id, start, time_base),
                TrackTimestamp::new(track_id, end_exclusive, time_base),
            )
            .expect("valid test window"),
        )
    }

    /// Строит raw packet timing в том же clock-е.
    fn timing(pts_units: i64, numer: u32, denom: u32) -> AudioPacketTiming {
        AudioPacketTiming::from_track_units(
            AudioPacketTimeBase::new(numer, denom).expect("valid audio time base"),
            pts_units,
            None,
            None,
        )
    }

    /// Извлекает retained range из успешного bounded outcome.
    fn retained_range(outcome: DecodedPcmPacketClipOutcome) -> DecodedPcmFrameRange {
        match outcome {
            DecodedPcmPacketClipOutcome::Retained(range) => range,
            unexpected => panic!("ожидался retained range, получено {unexpected:?}"),
        }
    }

    #[test]
    fn unbounded_preserves_old_behavior_without_timing_or_format_validation() {
        let samples = [0.25, -0.5, 0.75];
        let outcome = DecodedPcmPacketBoundary::new(&samples, PacketPresentationWindow::Unbounded)
            .plan_clip(TrackId::new(2), AudioPacketTiming::unknown(), 0, 0)
            .expect("unbounded не требует новых metadata");

        assert_eq!(outcome, DecodedPcmPacketClipOutcome::Unbounded);
    }

    #[test]
    fn exact_and_inexact_half_open_boundaries_round_up() {
        let samples = [0.0; 20];
        let exact =
            DecodedPcmPacketBoundary::new(&samples, bounded_window(TrackId::new(2), 2, 8, 1, 4))
                .plan_clip(TrackId::new(2), timing(0, 1, 4), 4, 1)
                .expect("exact boundaries");
        let inexact =
            DecodedPcmPacketBoundary::new(&samples, bounded_window(TrackId::new(2), 1, 7, 1, 3))
                .plan_clip(TrackId::new(2), timing(0, 1, 3), 4, 1)
                .expect("inexact boundaries");

        assert_eq!(
            retained_range(exact),
            DecodedPcmFrameRange::ordered_or_empty(2, 8)
        );
        assert_eq!(
            retained_range(inexact),
            DecodedPcmFrameRange::ordered_or_empty(2, 10)
        );
    }

    #[test]
    fn negative_packet_pts_and_nontrivial_rate_scale_exactly() {
        let samples = [0.0; 30];
        let outcome =
            DecodedPcmPacketBoundary::new(&samples, bounded_window(TrackId::new(2), 1, 5, 2, 3))
                .plan_clip(TrackId::new(2), timing(-2, 2, 3), 5, 1)
                .expect("negative PTS остаётся exact signed");

        assert_eq!(
            retained_range(outcome),
            DecodedPcmFrameRange::ordered_or_empty(10, 24)
        );
    }

    #[test]
    fn windows_fully_before_or_after_decoded_packet_drop_all_frames() {
        let samples = [0.0; 8];
        let before =
            DecodedPcmPacketBoundary::new(&samples, bounded_window(TrackId::new(2), 1, 2, 1, 1))
                .plan_clip(TrackId::new(2), timing(10, 1, 1), 1, 1)
                .expect("window before packet");
        let after =
            DecodedPcmPacketBoundary::new(&samples, bounded_window(TrackId::new(2), 20, 21, 1, 1))
                .plan_clip(TrackId::new(2), timing(10, 1, 1), 1, 1)
                .expect("window after packet");

        assert_eq!(before, DecodedPcmPacketClipOutcome::FullyDropped);
        assert_eq!(after, DecodedPcmPacketClipOutcome::FullyDropped);
    }

    #[test]
    fn stereo_and_multichannel_ranges_slice_only_complete_frames() {
        let stereo = (0..12).map(|value| value as f32).collect::<Vec<_>>();
        let surround = (0..24).map(|value| value as f32).collect::<Vec<_>>();
        let stereo_range = retained_range(
            DecodedPcmPacketBoundary::new(&stereo, bounded_window(TrackId::new(2), 1, 5, 1, 1))
                .plan_clip(TrackId::new(2), timing(0, 1, 1), 1, 2)
                .expect("stereo range"),
        );
        let surround_range = retained_range(
            DecodedPcmPacketBoundary::new(&surround, bounded_window(TrackId::new(2), 1, 3, 1, 1))
                .plan_clip(TrackId::new(2), timing(0, 1, 1), 1, 6)
                .expect("surround range"),
        );

        assert_eq!(
            stereo_range.slice_interleaved(&stereo, 2).unwrap(),
            &stereo[2..10]
        );
        assert_eq!(
            surround_range.slice_interleaved(&surround, 6).unwrap(),
            &surround[6..18]
        );
    }

    #[test]
    fn invalid_bounded_metadata_and_pcm_layout_are_typed() {
        let samples = [0.0; 4];
        let window = bounded_window(TrackId::new(2), 0, 1, 1, 48_000);

        assert_eq!(
            DecodedPcmPacketBoundary::new(&samples, window).plan_clip(
                TrackId::new(2),
                AudioPacketTiming::unknown(),
                48_000,
                2,
            ),
            Err(DecodedPcmPacketClipError::MissingPacketTimeBase)
        );
        assert!(matches!(
            DecodedPcmPacketBoundary::new(&samples, window).plan_clip(
                TrackId::new(3),
                timing(0, 1, 48_000),
                48_000,
                2,
            ),
            Err(DecodedPcmPacketClipError::TrackMismatch { .. })
        ));
        assert_eq!(
            DecodedPcmPacketBoundary::new(&samples, window).plan_clip(
                TrackId::new(2),
                timing(0, 1, 24_000),
                48_000,
                2,
            ),
            Err(DecodedPcmPacketClipError::TimeBaseMismatch)
        );
        assert_eq!(
            DecodedPcmPacketBoundary::new(&samples, window).plan_clip(
                TrackId::new(2),
                timing(0, 1, 48_000),
                0,
                2,
            ),
            Err(DecodedPcmPacketClipError::ZeroSampleRate)
        );
        assert_eq!(
            DecodedPcmPacketBoundary::new(&samples, window).plan_clip(
                TrackId::new(2),
                timing(0, 1, 48_000),
                48_000,
                0,
            ),
            Err(DecodedPcmPacketClipError::ZeroChannels)
        );
        assert!(matches!(
            DecodedPcmPacketBoundary::new(&[0.0; 5], window).plan_clip(
                TrackId::new(2),
                timing(0, 1, 48_000),
                48_000,
                2,
            ),
            Err(DecodedPcmPacketClipError::NonInterleavedSampleCount { .. })
        ));
    }

    #[test]
    fn exact_arithmetic_overflow_is_typed_without_saturation() {
        let samples = [0.0; 2];
        let window = bounded_window(TrackId::new(2), i64::MAX - 1, i64::MAX, u32::MAX, 1);
        let error = DecodedPcmPacketBoundary::new(&samples, window)
            .plan_clip(TrackId::new(2), timing(i64::MIN, u32::MAX, 1), u32::MAX, 1)
            .expect_err("exact product не помещается в i128");

        assert!(matches!(
            error,
            DecodedPcmPacketClipError::ArithmeticOverflow {
                operation: DecodedPcmClipArithmeticOperation::ScaleBySampleRate
                    | DecodedPcmClipArithmeticOperation::ScaleByTimeBaseNumerator
            }
        ));
    }

    #[test]
    fn smooth_audio_proofs_remove_2048_and_zero_frames() {
        let first_samples = vec![0.0; 192_512 * 2];
        let second_samples = vec![0.0; 191_488 * 2];
        let first = DecodedPcmPacketBoundary::new(
            &first_samples,
            bounded_window(TrackId::new(1), 0, 39_680_000, 1, 10_000_000),
        )
        .plan_clip(TrackId::new(1), timing(0, 1, 10_000_000), 48_000, 2)
        .expect("first Smooth fragment");
        let second = DecodedPcmPacketBoundary::new(
            &second_samples,
            bounded_window(TrackId::new(1), 39_680_000, 79_573_333, 1, 10_000_000),
        )
        .plan_clip(
            TrackId::new(1),
            timing(39_680_000, 1, 10_000_000),
            48_000,
            2,
        )
        .expect("second Smooth fragment");

        assert_eq!(
            retained_range(first),
            DecodedPcmFrameRange::ordered_or_empty(0, 190_464)
        );
        assert_eq!(retained_range(second), DecodedPcmFrameRange::full(191_488));
        assert_eq!(192_512 - 190_464, 2_048);
        assert_eq!(191_488 - 191_488, 0);
    }

    #[test]
    fn frame_ranges_intersect_in_original_domain() {
        let packet = DecodedPcmFrameRange::ordered_or_empty(1, 4);
        let global = DecodedPcmFrameRange::ordered_or_empty(2, 5);
        let samples = (0..12).map(|sample| sample as f32).collect::<Vec<_>>();

        assert_eq!(
            packet.intersect(global),
            DecodedPcmFrameRange::ordered_or_empty(2, 4)
        );
        assert_eq!(
            packet
                .intersect(global)
                .slice_interleaved(&samples, 2)
                .expect("composed range"),
            &samples[4..8]
        );
        assert_eq!(
            DecodedPcmFrameRange::ordered_or_empty(1, 5)
                .intersect(DecodedPcmFrameRange::ordered_or_empty(0, 3)),
            DecodedPcmFrameRange::ordered_or_empty(1, 3)
        );
        assert_eq!(
            DecodedPcmFrameRange::ordered_or_empty(1, 3)
                .intersect(DecodedPcmFrameRange::ordered_or_empty(0, 5)),
            DecodedPcmFrameRange::ordered_or_empty(1, 3)
        );
    }
}
