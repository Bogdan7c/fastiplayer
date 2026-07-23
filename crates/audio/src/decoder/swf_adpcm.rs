//! Project-owned decoder точного Shockwave Flash ADPCM bitstream-а.
//!
//! Каждый encoded packet содержит code-size prefix, ноль или больше полных
//! 4096-sample blocks и один допустимый partial final block. Predictor/index
//! читаются заново из каждого блока, поэтому decoder не имеет скрытого
//! cross-packet state и reset является честным no-op.

use anyhow::Result;
use thiserror::Error;

use super::{
    AudioChannelLayout, AudioDecoder, AudioDecoderConfig, AudioDecoderError, EncodedAudioPacket,
    required_audio_config_value,
};

/// Exact container identity, которую нельзя подменять IMA/MS ADPCM.
pub(super) const SWF_ADPCM_CODEC_ID: &str = "A_ADPCM_SWF";
/// Один SWF ADPCM block всегда представляет 4096 sample frames на канал.
const SAMPLES_PER_BLOCK: usize = 4096;
/// После initial sample остаётся 4095 differential codes на канал.
const CODES_PER_BLOCK: usize = SAMPLES_PER_BLOCK - 1;
/// Predictor и index занимают 16 + 6 bits на канал.
const CHANNEL_HEADER_BITS: usize = 22;
/// Максимальный индекс стандартной IMA step table.
const MAX_STEP_INDEX: usize = 88;

/// Нормативная 89-entry IMA ADPCM step table.
const STEP_SIZE_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// Typed ошибки SWF ADPCM packet boundary.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SwfAdpcmDecodeError {
    /// Packet не содержит двухбитный code-size prefix.
    #[error("SWF ADPCM code-size prefix truncated: only {available_bits} bits available")]
    TruncatedCodeSize {
        /// Доступные input bits.
        available_bits: usize,
    },
    /// Decoder поддерживает только форматы, разрешённые SWF: mono/stereo.
    #[error("SWF ADPCM supports only mono or stereo, got {channels} channels")]
    InvalidChannelCount {
        /// Channel count из track metadata.
        channels: u32,
    },
    /// Packet оборвался внутри обязательного channel header нового block-а.
    #[error(
        "SWF ADPCM block {block_index} truncated: expected {required_bits} bits, got {remaining_bits}"
    )]
    TruncatedBlock {
        /// Zero-based индекс неполного block-а.
        block_index: usize,
        /// Полная длина channel headers нового block-а.
        required_bits: usize,
        /// Оставшиеся после предыдущих blocks bits.
        remaining_bits: usize,
    },
    /// Partial final block оборвался внутри interleaved channel code group.
    #[error(
        "SWF ADPCM block {block_index} has incomplete channel code group: expected {required_bits} bits, got {remaining_bits}"
    )]
    IncompleteChannelGroup {
        /// Zero-based индекс partial block-а.
        block_index: usize,
        /// Полная длина одной code group для всех channels.
        required_bits: usize,
        /// Недостающий хвост после последней целой group.
        remaining_bits: usize,
    },
    /// Byte alignment допускает только zero padding до следующего byte boundary.
    #[error("SWF ADPCM has non-zero trailing padding in {padding_bits} alignment bits")]
    NonZeroPadding {
        /// Количество trailing alignment bits.
        padding_bits: usize,
    },
    /// Initial index не может адресовать step table.
    #[error(
        "SWF ADPCM block {block_index} channel {channel} has invalid initial step index {index}"
    )]
    InvalidInitialStepIndex {
        /// Zero-based block index.
        block_index: usize,
        /// Zero-based channel index.
        channel: usize,
        /// Прочитанный шестибитный index.
        index: usize,
    },
    /// Checked sample-count arithmetic обнаружила overflow.
    #[error("SWF ADPCM decoded sample count overflow")]
    OutputSampleCountOverflow,
    /// Allocator отказался резервировать bounded-by-input output.
    #[error("SWF ADPCM could not reserve output for {sample_count} samples")]
    OutputAllocationFailed {
        /// Проверенный interleaved sample count.
        sample_count: usize,
    },
}

/// Concrete packet-local SWF ADPCM decoder.
pub(super) struct SwfAdpcmDecoder {
    /// Track ID, которому принадлежит decoder.
    track_id: u32,
    /// Sample rate из обязательной track metadata.
    sample_rate: u32,
    /// Один или два interleaved channels.
    channels: u32,
    /// Однозначный mono/stereo layout.
    channel_layout: AudioChannelLayout,
}

impl SwfAdpcmDecoder {
    /// Создаёт decoder только из полного mono/stereo track spec.
    pub(super) fn new(config: &AudioDecoderConfig) -> Result<Self> {
        config.validate_probe_metadata()?;
        let sample_rate = required_audio_config_value(config.sample_rate(), "sample_rate", config)?;
        let channels = required_audio_config_value(config.channels(), "channels", config)?;
        if !matches!(channels, 1 | 2) {
            return Err(SwfAdpcmDecodeError::InvalidChannelCount { channels }.into());
        }
        let channel_layout = AudioChannelLayout::from_channel_count(channels).map_err(|error| {
            AudioDecoderError::InvalidConfig {
                codec_id: config.codec_id().to_string(),
                reason: error.to_string(),
            }
        })?;

        Ok(Self {
            track_id: config.track_id(),
            sample_rate,
            channels,
            channel_layout,
        })
    }

    /// Проверяет selected-track invariant до разбора packet bytes.
    fn ensure_packet_track_matches(&self, packet: &EncodedAudioPacket<'_>) -> Result<()> {
        if packet.track_id() != self.track_id {
            anyhow::bail!(
                "Audio packet track mismatch: decoder track {}, packet track {}",
                self.track_id,
                packet.track_id()
            );
        }
        Ok(())
    }
}

impl AudioDecoder for SwfAdpcmDecoder {
    /// Декодирует весь packet атомарно; при ошибке partial PCM не возвращается.
    fn decode(&mut self, packet: &EncodedAudioPacket<'_>) -> Result<Vec<f32>> {
        self.ensure_packet_track_matches(packet)?;
        decode_swf_adpcm_packet(packet.data(), self.channels as usize).map_err(Into::into)
    }

    /// Cross-packet codec state отсутствует, поэтому reset ничего не мутирует.
    fn reset(&mut self) -> Result<()> {
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u32 {
        self.channels
    }

    fn channel_layout(&self) -> Option<AudioChannelLayout> {
        Some(self.channel_layout)
    }
}

/// Mutable state одного канала живёт только внутри текущего block-а.
#[derive(Debug, Clone, Copy)]
struct ChannelState {
    /// Последний reconstructed PCM16 sample в расширенном i32 domain.
    predictor: i32,
    /// Текущий индекс стандартной 89-entry step table.
    step_index: usize,
}

/// Декодирует exact packet с allocation, строго ограниченной input length.
fn decode_swf_adpcm_packet(
    packet_bytes: &[u8],
    channels: usize,
) -> Result<Vec<f32>, SwfAdpcmDecodeError> {
    if !matches!(channels, 1 | 2) {
        return Err(SwfAdpcmDecodeError::InvalidChannelCount {
            channels: u32::try_from(channels).unwrap_or(u32::MAX),
        });
    }
    let total_bits = packet_bytes
        .len()
        .checked_mul(8)
        .ok_or(SwfAdpcmDecodeError::OutputSampleCountOverflow)?;
    if total_bits < 2 {
        return Err(SwfAdpcmDecodeError::TruncatedCodeSize {
            available_bits: total_bits,
        });
    }

    let mut bit_reader = MsbBitReader::new(packet_bytes);
    let bits_per_code = bit_reader.read_bits(2) as usize + 2;
    let output_sample_capacity = total_bits / bits_per_code;
    let mut decoded_samples = Vec::new();
    decoded_samples
        .try_reserve_exact(output_sample_capacity)
        .map_err(|_| SwfAdpcmDecodeError::OutputAllocationFailed {
            sample_count: output_sample_capacity,
        })?;

    let channel_headers_bits = CHANNEL_HEADER_BITS
        .checked_mul(channels)
        .ok_or(SwfAdpcmDecodeError::OutputSampleCountOverflow)?;
    let channel_code_group_bits = bits_per_code
        .checked_mul(channels)
        .ok_or(SwfAdpcmDecodeError::OutputSampleCountOverflow)?;
    let mut block_index = 0_usize;
    loop {
        let remaining_before_header = bit_reader.remaining_bits();
        if remaining_before_header < channel_headers_bits {
            if block_index == 0 || remaining_before_header > 7 {
                return Err(SwfAdpcmDecodeError::TruncatedBlock {
                    block_index,
                    required_bits: channel_headers_bits,
                    remaining_bits: remaining_before_header,
                });
            }
            validate_zero_padding(&mut bit_reader, remaining_before_header)?;
            break;
        }
        let available_code_groups =
            (remaining_before_header - channel_headers_bits) / channel_code_group_bits;
        let code_group_count = available_code_groups.min(CODES_PER_BLOCK);
        decode_block(
            &mut bit_reader,
            bits_per_code,
            channels,
            block_index,
            code_group_count,
            &mut decoded_samples,
        )?;
        if code_group_count < CODES_PER_BLOCK {
            let trailing_bits = bit_reader.remaining_bits();
            if trailing_bits > 7 {
                return Err(SwfAdpcmDecodeError::IncompleteChannelGroup {
                    block_index,
                    required_bits: channel_code_group_bits,
                    remaining_bits: trailing_bits,
                });
            }
            validate_zero_padding(&mut bit_reader, trailing_bits)?;
            break;
        }
        block_index = block_index
            .checked_add(1)
            .ok_or(SwfAdpcmDecodeError::OutputSampleCountOverflow)?;
    }
    debug_assert_eq!(bit_reader.remaining_bits(), 0);
    Ok(decoded_samples)
}

/// Проверяет, что остаток является только нулевым byte-alignment padding.
fn validate_zero_padding(
    bit_reader: &mut MsbBitReader<'_>,
    padding_bits: usize,
) -> Result<(), SwfAdpcmDecodeError> {
    if padding_bits != 0 && bit_reader.read_bits(padding_bits) != 0 {
        return Err(SwfAdpcmDecodeError::NonZeroPadding { padding_bits });
    }
    Ok(())
}

/// Декодирует один block и публикует PCM только в caller-owned temporary Vec.
fn decode_block(
    bit_reader: &mut MsbBitReader<'_>,
    bits_per_code: usize,
    channels: usize,
    block_index: usize,
    code_group_count: usize,
    decoded_samples: &mut Vec<f32>,
) -> Result<(), SwfAdpcmDecodeError> {
    let mut channel_states = Vec::with_capacity(channels);
    for channel in 0..channels {
        let predictor = bit_reader.read_signed_16();
        let step_index = bit_reader.read_bits(6) as usize;
        if step_index > MAX_STEP_INDEX {
            return Err(SwfAdpcmDecodeError::InvalidInitialStepIndex {
                block_index,
                channel,
                index: step_index,
            });
        }
        channel_states.push(ChannelState {
            predictor,
            step_index,
        });
        decoded_samples.push(sample_to_f32(predictor));
    }

    for _ in 0..code_group_count {
        for channel_state in &mut channel_states {
            let code = bit_reader.read_bits(bits_per_code) as usize;
            let predictor = decode_code(channel_state, bits_per_code, code);
            decoded_samples.push(sample_to_f32(predictor));
        }
    }
    Ok(())
}

/// Применяет одну SWF-расширенную IMA code к channel state.
fn decode_code(channel_state: &mut ChannelState, bits_per_code: usize, code: usize) -> i32 {
    let sign_mask = 1_usize << (bits_per_code - 1);
    let magnitude = code & (sign_mask - 1);
    let mut shifted_step = STEP_SIZE_TABLE[channel_state.step_index];
    let mut contribution_mask = 1_usize << (bits_per_code - 2);
    let mut difference = 0_i32;
    loop {
        if magnitude & contribution_mask != 0 {
            difference += shifted_step;
        }
        shifted_step >>= 1;
        contribution_mask >>= 1;
        if contribution_mask == 0 {
            break;
        }
    }
    difference += shifted_step;
    if code & sign_mask == 0 {
        channel_state.predictor += difference;
    } else {
        channel_state.predictor -= difference;
    }
    channel_state.predictor = channel_state
        .predictor
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX));

    let adjusted_index =
        channel_state.step_index as i32 + i32::from(index_adjustment(bits_per_code, magnitude));
    channel_state.step_index = adjusted_index.clamp(0, MAX_STEP_INDEX as i32) as usize;
    channel_state.predictor
}

/// Возвращает exact SWF index table entry для code magnitude.
fn index_adjustment(bits_per_code: usize, magnitude: usize) -> i8 {
    const INDEX_2: [i8; 2] = [-1, 2];
    const INDEX_3: [i8; 4] = [-1, -1, 2, 4];
    const INDEX_4: [i8; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];
    const INDEX_5: [i8; 16] = [-1, -1, -1, -1, -1, -1, -1, -1, 1, 2, 4, 6, 8, 10, 13, 16];
    match bits_per_code {
        2 => INDEX_2[magnitude],
        3 => INDEX_3[magnitude],
        4 => INDEX_4[magnitude],
        5 => INDEX_5[magnitude],
        _ => unreachable!("двухбитный prefix допускает только 2..=5 bits"),
    }
}

/// Конвертирует signed PCM16 domain в neutral f32 domain.
fn sample_to_f32(sample: i32) -> f32 {
    sample as f32 / 32768.0
}

/// Bounded MSB-first reader; вызывается только после exact packet-size проверки.
struct MsbBitReader<'packet> {
    /// Неизменяемые encoded packet bytes.
    bytes: &'packet [u8],
    /// Следующая unread bit position от начала packet-а.
    bit_position: usize,
}

impl<'packet> MsbBitReader<'packet> {
    fn new(bytes: &'packet [u8]) -> Self {
        Self {
            bytes,
            bit_position: 0,
        }
    }

    fn read_bits(&mut self, bit_count: usize) -> u32 {
        debug_assert!(bit_count <= 32);
        debug_assert!(self.remaining_bits() >= bit_count);
        let mut value = 0_u32;
        for _ in 0..bit_count {
            let byte = self.bytes[self.bit_position / 8];
            let bit_in_byte = 7 - (self.bit_position % 8);
            value = (value << 1) | u32::from((byte >> bit_in_byte) & 1);
            self.bit_position += 1;
        }
        value
    }

    fn read_signed_16(&mut self) -> i32 {
        i32::from(self.read_bits(16) as u16 as i16)
    }

    fn remaining_bits(&self) -> usize {
        self.bytes.len() * 8 - self.bit_position
    }
}

#[cfg(test)]
mod tests {
    use audio_core::{AudioDecoder, AudioDecoderConfig, AudioPacketTiming, EncodedAudioPacket};

    use super::{
        CODES_PER_BLOCK, ChannelState, MAX_STEP_INDEX, SWF_ADPCM_CODEC_ID, SwfAdpcmDecodeError,
        SwfAdpcmDecoder, decode_code, decode_swf_adpcm_packet,
    };

    /// Test-only MSB writer строит fixture по структуре SWF specification.
    struct BitWriter {
        bytes: Vec<u8>,
        bit_position: usize,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                bit_position: 0,
            }
        }

        fn push_bits(&mut self, value: u32, bit_count: usize) {
            for shift in (0..bit_count).rev() {
                if self.bit_position.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                let bit = ((value >> shift) & 1) as u8;
                let byte_index = self.bit_position / 8;
                let bit_in_byte = 7 - (self.bit_position % 8);
                self.bytes[byte_index] |= bit << bit_in_byte;
                self.bit_position += 1;
            }
        }
    }

    /// Строит независимые blocks с заданным числом целых channel code groups.
    fn fixture(
        bits_per_code: usize,
        predictors: &[i16],
        initial_index: u8,
        code: u32,
        block_code_group_counts: &[usize],
    ) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.push_bits((bits_per_code - 2) as u32, 2);
        for &code_group_count in block_code_group_counts {
            for predictor in predictors {
                writer.push_bits(u32::from(*predictor as u16), 16);
                writer.push_bits(u32::from(initial_index), 6);
            }
            for _ in 0..code_group_count {
                for _ in predictors {
                    writer.push_bits(code, bits_per_code);
                }
            }
        }
        writer.bytes
    }

    /// Golden vectors фиксируют FFmpeg/reference rounding для всех code widths.
    #[test]
    fn reference_vectors_cover_two_through_five_bit_mono_and_stereo() {
        for (
            bits_per_code,
            code,
            expected_difference,
            mono_groups,
            mono_samples,
            stereo_groups,
            stereo_samples,
        ) in [
            (2, 1, 10, 4, 5, 1, 6),
            (3, 3, 11, 8, 9, 3, 8),
            (4, 7, 11, 2, 3, 1, 4),
            (5, 15, 11, 8, 9, 3, 8),
        ] {
            let mono =
                decode_swf_adpcm_packet(&fixture(bits_per_code, &[0], 0, code, &[mono_groups]), 1)
                    .expect("valid partial mono block");
            assert_eq!(mono.len(), mono_samples);
            assert_eq!(mono[0], 0.0);
            assert_eq!(mono[1], expected_difference as f32 / 32768.0);

            let stereo = decode_swf_adpcm_packet(
                &fixture(bits_per_code, &[100, -100], 0, code, &[stereo_groups]),
                2,
            )
            .expect("valid partial stereo block");
            assert_eq!(stereo.len(), stereo_samples);
            assert_eq!(
                &stereo[..4],
                &[
                    100.0 / 32768.0,
                    -100.0 / 32768.0,
                    (100 + expected_difference) as f32 / 32768.0,
                    (-100 + expected_difference) as f32 / 32768.0,
                ]
            );
        }
    }

    /// Partial final block принимает только целые groups и сохраняет interleave.
    #[test]
    fn partial_final_stereo_block_is_bounded_and_interleaved() {
        let decoded = decode_swf_adpcm_packet(&fixture(4, &[100, -100], 0, 0, &[17]), 2)
            .expect("valid partial stereo block");
        assert_eq!(decoded.len(), 36);
        assert_eq!(decoded[0], 100.0 / 32768.0);
        assert_eq!(decoded[1], -100.0 / 32768.0);
    }

    /// Full block может предшествовать partial final block без hidden carry state.
    #[test]
    fn full_and_partial_multiple_blocks_restart_channel_state() {
        let decoded = decode_swf_adpcm_packet(&fixture(2, &[25], 0, 0, &[CODES_PER_BLOCK, 2]), 1)
            .expect("full plus partial blocks");
        assert_eq!(decoded.len(), 4099);
        assert_eq!(decoded[4096], 25.0 / 32768.0);
        assert_eq!(decoded[4097], 28.0 / 32768.0);
        assert_eq!(decoded[4098], 31.0 / 32768.0);
    }

    /// Byte-aligned tail длиннее padding, но короче stereo group, отклоняется.
    #[test]
    fn incomplete_stereo_channel_group_is_typed() {
        let mut writer = BitWriter::new();
        writer.push_bits(3, 2);
        for _ in 0..2 {
            writer.push_bits(0, 16);
            writer.push_bits(0, 6);
        }
        writer.push_bits(0, 11);
        assert!(matches!(
            decode_swf_adpcm_packet(&writer.bytes, 2),
            Err(SwfAdpcmDecodeError::IncompleteChannelGroup {
                block_index: 0,
                required_bits: 10,
                remaining_bits: 8,
            })
        ));
    }

    /// Padding bits обязаны быть нулевыми.
    #[test]
    fn non_zero_alignment_padding_is_typed() {
        let mut bytes = fixture(5, &[0], 0, 0, &[1]);
        *bytes.last_mut().expect("fixture byte") |= 1;
        assert_eq!(
            decode_swf_adpcm_packet(&bytes, 1),
            Err(SwfAdpcmDecodeError::NonZeroPadding { padding_bits: 3 })
        );
    }

    /// Packet без полного первого channel header возвращает typed truncation.
    #[test]
    fn truncated_first_block_header_is_typed() {
        assert!(matches!(
            decode_swf_adpcm_packet(&[0], 1),
            Err(SwfAdpcmDecodeError::TruncatedBlock {
                block_index: 0,
                required_bits: 22,
                remaining_bits: 6,
            })
        ));
    }

    /// Predictor и step index saturate в нормативных диапазонах без integer wrap.
    #[test]
    fn predictor_and_step_index_are_bounded() {
        let mut upper_state = ChannelState {
            predictor: i32::from(i16::MAX),
            step_index: MAX_STEP_INDEX,
        };
        assert_eq!(decode_code(&mut upper_state, 5, 15), i32::from(i16::MAX));
        assert_eq!(upper_state.step_index, MAX_STEP_INDEX);

        let mut lower_state = ChannelState {
            predictor: i32::from(i16::MIN),
            step_index: 0,
        };
        assert_eq!(decode_code(&mut lower_state, 2, 3), i32::from(i16::MIN));
        assert_eq!(lower_state.step_index, 2);
    }

    /// Multichannel metadata отклоняется exact SWF error-ом до packet decode.
    #[test]
    fn invalid_channel_count_is_typed() {
        let config = AudioDecoderConfig::new(7, SWF_ADPCM_CODEC_ID, 44_100, 3);
        let error = match SwfAdpcmDecoder::new(&config) {
            Ok(_) => panic!("three-channel SWF ADPCM must be rejected"),
            Err(error) => error,
        };
        assert_eq!(
            error.downcast_ref::<SwfAdpcmDecodeError>(),
            Some(&SwfAdpcmDecodeError::InvalidChannelCount { channels: 3 })
        );
    }

    /// Reset ничего не меняет: одинаковый packet декодируется идентично.
    #[test]
    fn reset_has_no_hidden_cross_packet_state() {
        let config = AudioDecoderConfig::new(7, SWF_ADPCM_CODEC_ID, 44_100, 1);
        let mut decoder = SwfAdpcmDecoder::new(&config).expect("decoder");
        let bytes = fixture(4, &[0], 0, 7, &[CODES_PER_BLOCK]);
        let packet = EncodedAudioPacket::new(7, AudioPacketTiming::unknown(), &bytes);
        let first = decoder.decode(&packet).expect("first decode");
        decoder.reset().expect("no-op reset");
        let second = decoder.decode(&packet).expect("second decode");
        assert_eq!(first, second);
    }
}
