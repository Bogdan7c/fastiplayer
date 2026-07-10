//! Layout-aware преобразование decoded PCM в channel count output stream-а.
//!
//! Матрица строится один раз при создании output. Packet path только применяет
//! готовые коэффициенты и никогда не угадывает speaker positions по одному count.

use audio_core::{AudioChannelLayout, AudioChannelPosition, AudioOutputWriteError};

/// Линейный коэффициент −3 dB для center и surround contributions.
const MINUS_THREE_DB: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Один ряд stereo downmix matrix для одной input lane.
#[derive(Debug, Clone, Copy, PartialEq)]
struct StereoContribution {
    /// Contribution в левый output channel.
    left: f32,
    /// Contribution в правый output channel.
    right: f32,
}

/// Предвычисленная политика channel conversion.
#[derive(Debug, Clone, PartialEq)]
enum ChannelMixPlan {
    /// Input и output имеют одинаковое число lanes; scalar samples копируются точно.
    Passthrough,
    /// Mono sample дублируется во все output lanes.
    ReplicateMono,
    /// Stereo остаётся в первых двух conventional stream lanes, остальные молчат.
    StereoWithSilentExtraChannels,
    /// Positional multichannel PCM сводится в conventional stereo.
    StereoMatrix(Box<[StereoContribution]>),
    /// Layout нельзя преобразовать без недоказанного предположения.
    Unsupported,
}

/// Stateless mixer, принадлежащий одному concrete audio output.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChannelMixer {
    /// Authoritative layout decoded PCM.
    input_layout: AudioChannelLayout,
    /// Число lanes interleaved output stream-а.
    output_channels: usize,
    /// Выбранный при создании output план преобразования.
    plan: ChannelMixPlan,
}

impl ChannelMixer {
    /// Строит план преобразования без packet-local анализа layout-а.
    #[must_use]
    pub(crate) fn new(input_layout: AudioChannelLayout, output_channels: usize) -> Self {
        let input_channels = input_layout.channel_count() as usize;
        let plan = if output_channels == 0 {
            ChannelMixPlan::Unsupported
        } else if input_channels == output_channels {
            ChannelMixPlan::Passthrough
        } else if input_channels == 1 {
            ChannelMixPlan::ReplicateMono
        } else if input_layout == AudioChannelLayout::stereo() && output_channels > 2 {
            ChannelMixPlan::StereoWithSilentExtraChannels
        } else if output_channels == 2 {
            stereo_downmix_matrix(input_layout)
                .map(ChannelMixPlan::StereoMatrix)
                .unwrap_or(ChannelMixPlan::Unsupported)
        } else {
            ChannelMixPlan::Unsupported
        };

        Self {
            input_layout,
            output_channels,
            plan,
        }
    }

    /// Преобразует interleaved PCM в caller-owned reusable buffer.
    ///
    /// Ошибка проверяется до `output_samples.clear()`, поэтому unsupported или
    /// malformed packet не уничтожает ранее подготовленный caller buffer.
    pub(crate) fn mix_interleaved_into<'a>(
        &self,
        input_samples: &[f32],
        output_samples: &'a mut Vec<f32>,
    ) -> Result<&'a [f32], AudioOutputWriteError> {
        let input_channels = self.input_layout.channel_count() as usize;
        if self.output_channels == 0 {
            return Err(AudioOutputWriteError::InvalidChannelCount { boundary: "output" });
        }
        if input_samples.len() % input_channels != 0 {
            return Err(AudioOutputWriteError::InputNotFrameAligned {
                input_samples: input_samples.len(),
                input_channels,
            });
        }
        if self.plan == ChannelMixPlan::Unsupported {
            return Err(AudioOutputWriteError::UnsupportedChannelConversion {
                input_layout: self.input_layout,
                output_channels: self.output_channels as u32,
            });
        }

        let required_samples =
            (input_samples.len() / input_channels).saturating_mul(self.output_channels);
        output_samples.clear();
        output_samples.reserve(required_samples);

        match &self.plan {
            ChannelMixPlan::Passthrough => output_samples.extend_from_slice(input_samples),
            ChannelMixPlan::ReplicateMono => {
                for input_sample in input_samples {
                    output_samples.extend(std::iter::repeat_n(*input_sample, self.output_channels));
                }
            }
            ChannelMixPlan::StereoWithSilentExtraChannels => {
                for input_frame in input_samples.chunks_exact(2) {
                    output_samples.extend_from_slice(input_frame);
                    output_samples.extend(std::iter::repeat_n(0.0, self.output_channels - 2));
                }
            }
            ChannelMixPlan::StereoMatrix(matrix) => {
                for input_frame in input_samples.chunks_exact(input_channels) {
                    let mut left_output = 0.0_f32;
                    let mut right_output = 0.0_f32;
                    for (sample, contribution) in input_frame.iter().zip(matrix.iter()) {
                        left_output += sample * contribution.left;
                        right_output += sample * contribution.right;
                    }
                    output_samples.push(left_output);
                    output_samples.push(right_output);
                }
            }
            ChannelMixPlan::Unsupported => unreachable!("unsupported plan returned before write"),
        }

        Ok(output_samples.as_slice())
    }
}

/// Строит и статически нормализует positional multichannel → stereo matrix.
fn stereo_downmix_matrix(input_layout: AudioChannelLayout) -> Option<Box<[StereoContribution]>> {
    if !input_layout.is_positioned() {
        return None;
    }

    let mut matrix = Vec::with_capacity(input_layout.channel_count() as usize);
    for lane_index in 0..input_layout.channel_count() as usize {
        let position = input_layout.position_at(lane_index)?;
        matrix.push(stereo_contribution_for_position(position)?);
    }

    normalize_stereo_matrix_rows(&mut matrix);
    Some(matrix.into_boxed_slice())
}

/// Возвращает ненормализованные ITU-подобные stereo contributions.
fn stereo_contribution_for_position(position: AudioChannelPosition) -> Option<StereoContribution> {
    let contribution = match position {
        AudioChannelPosition::FrontLeft => StereoContribution {
            left: 1.0,
            right: 0.0,
        },
        AudioChannelPosition::FrontRight => StereoContribution {
            left: 0.0,
            right: 1.0,
        },
        AudioChannelPosition::FrontCenter | AudioChannelPosition::RearCenter => {
            StereoContribution {
                left: MINUS_THREE_DB,
                right: MINUS_THREE_DB,
            }
        }
        AudioChannelPosition::RearLeft
        | AudioChannelPosition::SideLeft
        | AudioChannelPosition::FrontLeftOfCenter
        | AudioChannelPosition::FrontLeftWide => StereoContribution {
            left: MINUS_THREE_DB,
            right: 0.0,
        },
        AudioChannelPosition::RearRight
        | AudioChannelPosition::SideRight
        | AudioChannelPosition::FrontRightOfCenter
        | AudioChannelPosition::FrontRightWide => StereoContribution {
            left: 0.0,
            right: MINUS_THREE_DB,
        },
        // LFE — не full-range канал. Без bass-management его подмешивание
        // дублирует низкие частоты, поэтому stereo matrix явно исключает LFE.
        AudioChannelPosition::LowFrequencyEffects | AudioChannelPosition::LowFrequencyEffects2 => {
            StereoContribution {
                left: 0.0,
                right: 0.0,
            }
        }
        // Для height/bottom channels в текущей политике нет доказанной матрицы.
        // Молчаливо отбрасывать их хуже, чем вернуть typed unsupported error.
        AudioChannelPosition::TopCenter
        | AudioChannelPosition::TopFrontLeft
        | AudioChannelPosition::TopFrontCenter
        | AudioChannelPosition::TopFrontRight
        | AudioChannelPosition::TopRearLeft
        | AudioChannelPosition::TopRearCenter
        | AudioChannelPosition::TopRearRight
        | AudioChannelPosition::TopSideLeft
        | AudioChannelPosition::TopSideRight
        | AudioChannelPosition::BottomFrontCenter
        | AudioChannelPosition::BottomFrontLeft
        | AudioChannelPosition::BottomFrontRight => return None,
    };

    Some(contribution)
}

/// Масштабирует каждый stereo row так, чтобы сумма абсолютных contributions
/// не превышала full scale даже при синфазных `+1.0` на всех input channels.
fn normalize_stereo_matrix_rows(matrix: &mut [StereoContribution]) {
    let left_sum: f32 = matrix
        .iter()
        .map(|contribution| contribution.left.abs())
        .sum();
    let right_sum: f32 = matrix
        .iter()
        .map(|contribution| contribution.right.abs())
        .sum();
    let left_gain = 1.0 / left_sum.max(1.0);
    let right_gain = 1.0 / right_sum.max(1.0);

    for contribution in matrix {
        contribution.left *= left_gain;
        contribution.right *= right_gain;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Стандартный positional 5.1 layout в canonical lane order.
    fn surround_5_1_layout() -> AudioChannelLayout {
        AudioChannelLayout::positioned(&[
            AudioChannelPosition::FrontLeft,
            AudioChannelPosition::FrontRight,
            AudioChannelPosition::FrontCenter,
            AudioChannelPosition::LowFrequencyEffects,
            AudioChannelPosition::RearLeft,
            AudioChannelPosition::RearRight,
        ])
        .unwrap()
    }

    /// Сравнивает float с допуском только на matrix arithmetic.
    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 1.0e-6,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn stereo_passthrough_preserves_every_sample_bit() {
        let mixer = ChannelMixer::new(AudioChannelLayout::stereo(), 2);
        let input = [0.0_f32, -0.0, 0.25, -0.75, 1.0, -1.0];
        let mut output = Vec::new();

        mixer.mix_interleaved_into(&input, &mut output).unwrap();

        let input_bits: Vec<u32> = input.iter().map(|sample| sample.to_bits()).collect();
        let output_bits: Vec<u32> = output.iter().map(|sample| sample.to_bits()).collect();
        assert_eq!(output_bits, input_bits);
    }

    #[test]
    fn matching_multichannel_layout_is_also_bit_exact() {
        let mixer = ChannelMixer::new(AudioChannelLayout::surround_5_1(), 6);
        let input = [0.0_f32, -0.0, 0.25, -0.75, 1.0, -1.0];
        let mut output = Vec::new();

        mixer.mix_interleaved_into(&input, &mut output).unwrap();

        let input_bits: Vec<u32> = input.iter().map(|sample| sample.to_bits()).collect();
        let output_bits: Vec<u32> = output.iter().map(|sample| sample.to_bits()).collect();
        assert_eq!(output_bits, input_bits);
    }

    #[test]
    fn mono_is_duplicated_exactly_to_stereo() {
        let mixer = ChannelMixer::new(AudioChannelLayout::mono(), 2);
        let input = [0.25_f32, -0.5];
        let mut output = Vec::new();

        mixer.mix_interleaved_into(&input, &mut output).unwrap();

        assert_eq!(output, [0.25, 0.25, -0.5, -0.5]);
    }

    #[test]
    fn each_5_1_lane_uses_its_explicit_stereo_contribution() {
        let mixer = ChannelMixer::new(surround_5_1_layout(), 2);
        let mut output = Vec::new();
        let front_gain = 1.0 / (1.0 + 2.0 * MINUS_THREE_DB);
        let center_or_surround_gain = front_gain * MINUS_THREE_DB;
        let expected = [
            (front_gain, 0.0),
            (0.0, front_gain),
            (center_or_surround_gain, center_or_surround_gain),
            (0.0, 0.0),
            (center_or_surround_gain, 0.0),
            (0.0, center_or_surround_gain),
        ];

        for (lane_index, (expected_left, expected_right)) in expected.into_iter().enumerate() {
            let mut impulse = [0.0_f32; 6];
            impulse[lane_index] = 1.0;
            mixer.mix_interleaved_into(&impulse, &mut output).unwrap();
            assert_close(output[0], expected_left);
            assert_close(output[1], expected_right);
        }
    }

    #[test]
    fn side_surround_5_1_keeps_left_and_right_directionality() {
        let mixer = ChannelMixer::new(AudioChannelLayout::surround_5_1_side(), 2);
        let mut output = Vec::new();
        let mut side_left_impulse = [0.0_f32; 6];
        side_left_impulse[4] = 1.0;

        mixer
            .mix_interleaved_into(&side_left_impulse, &mut output)
            .unwrap();

        assert!(output[0] > 0.0);
        assert_eq!(output[1], 0.0);
    }

    #[test]
    fn normalized_5_1_matrix_stays_within_full_scale() {
        let mixer = ChannelMixer::new(surround_5_1_layout(), 2);
        let mut output = Vec::new();

        mixer.mix_interleaved_into(&[1.0; 6], &mut output).unwrap();
        assert!(output.iter().all(|sample| *sample <= 1.0));
        mixer.mix_interleaved_into(&[-1.0; 6], &mut output).unwrap();
        assert!(output.iter().all(|sample| *sample >= -1.0));
    }

    #[test]
    fn discrete_multichannel_layout_is_rejected_without_mutating_caller_buffer() {
        let mixer = ChannelMixer::new(AudioChannelLayout::discrete(6).unwrap(), 2);
        let mut output = vec![42.0];

        let error = mixer
            .mix_interleaved_into(&[0.0; 6], &mut output)
            .unwrap_err();

        assert_eq!(
            error,
            AudioOutputWriteError::UnsupportedChannelConversion {
                input_layout: AudioChannelLayout::discrete(6).unwrap(),
                output_channels: 2,
            }
        );
        assert_eq!(output, [42.0]);
    }

    #[test]
    fn multiple_frames_keep_interleaved_frame_order() {
        let mixer = ChannelMixer::new(surround_5_1_layout(), 2);
        let input = [
            1.0, 0.0, 0.0, 0.0, 0.0, 0.0, // first frame: FL
            0.0, 1.0, 0.0, 0.0, 0.0, 0.0, // second frame: FR
        ];
        let mut output = Vec::new();

        mixer.mix_interleaved_into(&input, &mut output).unwrap();

        assert!(output[0] > 0.0);
        assert_eq!(output[1], 0.0);
        assert_eq!(output[2], 0.0);
        assert!(output[3] > 0.0);
    }
}
