//! Узкая proof boundary для codec metadata поддерживаемого ISM VOD profile.

use crate::error::{
    SmoothCodecConfigurationError, SmoothManifestError, SmoothProfileIncompatibility,
};
use crate::limits::{SmoothManifestLimitKind, SmoothManifestLimits};
use crate::quality::{SmoothCodecConfiguration, SmoothCodecConfigurationOrigin, SmoothCodecFourCc};

/// Exact spellings H.264 FourCC, доказанные для базового Smooth Streaming profile.
const H264_FOUR_CC_SPELLINGS: &[&str] = &["H264", "AVC1"];

/// Exact spelling AAC-LC FourCC; HE-AAC `AACH` намеренно не является alias.
const AAC_LC_FOUR_CC_SPELLINGS: &[&str] = &["AACL"];

/// Проверяет и сохраняет H.264 SPS/PPS в исходном four-byte-start-code layout.
pub(crate) fn parse_h264_configuration(
    four_cc: &str,
    encoded_hex: &str,
    limits: &SmoothManifestLimits,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> Result<(SmoothCodecFourCc, SmoothCodecConfiguration), SmoothManifestError> {
    if !H264_FOUR_CC_SPELLINGS.contains(&four_cc) {
        return Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::UnsupportedVideoCodec,
        });
    }
    let codec = SmoothCodecFourCc::new_validated(four_cc, limits)?;
    let bytes = decode_even_hex(encoded_hex, limits, is_cancelled)?;
    validate_h264_parameter_sets(&bytes, is_cancelled)?;
    Ok((
        codec,
        SmoothCodecConfiguration::from_validated(
            bytes,
            SmoothCodecConfigurationOrigin::H264SequenceAndPictureParameterSets,
        ),
    ))
}

/// Проверяет AAC-LC fields и либо validates ASC, либо выводит его из полей явно.
pub(crate) fn parse_aac_lc_configuration(
    four_cc: &str,
    audio_tag: u16,
    sampling_rate: u32,
    channels: u16,
    encoded_hex: Option<&str>,
    limits: &SmoothManifestLimits,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> Result<(SmoothCodecFourCc, SmoothCodecConfiguration), SmoothManifestError> {
    if !AAC_LC_FOUR_CC_SPELLINGS.contains(&four_cc) {
        return Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::UnsupportedAudioCodec,
        });
    }
    if audio_tag != 255 {
        return Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::UnsupportedAudioTag,
        });
    }
    let codec = SmoothCodecFourCc::new_validated(four_cc, limits)?;
    let (bytes, origin) = match encoded_hex {
        Some(encoded_hex) if !encoded_hex.is_empty() => {
            let bytes = decode_even_hex(encoded_hex, limits, is_cancelled)?;
            validate_aac_lc_audio_specific_config(&bytes, sampling_rate, channels)?;
            (
                bytes,
                SmoothCodecConfigurationOrigin::AacAudioSpecificConfig,
            )
        }
        Some(_) => {
            return Err(SmoothManifestError::InvalidCodecConfiguration {
                reason: SmoothCodecConfigurationError::Empty,
            });
        }
        None => (
            derive_aac_lc_audio_specific_config(sampling_rate, channels)?,
            SmoothCodecConfigurationOrigin::AacDerivedFromQualityFields,
        ),
    };
    Ok((
        codec,
        SmoothCodecConfiguration::from_validated(bytes, origin),
    ))
}

/// Декодирует только bounded, чётную ASCII hex строку без промежуточного unbounded роста.
fn decode_even_hex(
    encoded_hex: &str,
    limits: &SmoothManifestLimits,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> Result<Box<[u8]>, SmoothManifestError> {
    if encoded_hex.is_empty() {
        return Err(SmoothManifestError::InvalidCodecConfiguration {
            reason: SmoothCodecConfigurationError::Empty,
        });
    }
    if !encoded_hex.len().is_multiple_of(2) {
        return Err(SmoothManifestError::InvalidCodecConfiguration {
            reason: SmoothCodecConfigurationError::OddHexLength,
        });
    }
    let decoded_len = encoded_hex.len() / 2;
    if decoded_len > limits.maximum_codec_bytes() {
        return Err(SmoothManifestError::LimitExceeded {
            limit: SmoothManifestLimitKind::CodecBytes,
            maximum: limits.maximum_codec_bytes(),
        });
    }
    let mut decoded = Vec::with_capacity(decoded_len);
    for pair in encoded_hex.as_bytes().chunks_exact(2) {
        check_cancelled(is_cancelled)?;
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded.into_boxed_slice())
}

/// Преобразует один ASCII hex nibble без locale и Unicode aliases.
fn hex_nibble(byte: u8) -> Result<u8, SmoothManifestError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(SmoothManifestError::InvalidCodecConfiguration {
            reason: SmoothCodecConfigurationError::InvalidHexDigit,
        }),
    }
}

/// Принимает ровно один SPS и один PPS, каждый с canonical four-byte start code.
fn validate_h264_parameter_sets(
    bytes: &[u8],
    is_cancelled: &mut dyn FnMut() -> bool,
) -> Result<(), SmoothManifestError> {
    const START_CODE: &[u8] = &[0, 0, 0, 1];
    let mut cursor = 0usize;
    let mut sequence_parameter_set_seen = false;
    let mut picture_parameter_set_seen = false;
    while cursor < bytes.len() {
        check_cancelled(is_cancelled)?;
        if !bytes[cursor..].starts_with(START_CODE) {
            return Err(codec_error(
                SmoothCodecConfigurationError::UnexpectedH264NalUnit,
            ));
        }
        let payload_start = cursor + START_CODE.len();
        let next_start = bytes[payload_start..]
            .windows(START_CODE.len())
            .position(|window| window == START_CODE)
            .map_or(bytes.len(), |relative| payload_start + relative);
        let payload = &bytes[payload_start..next_start];
        let Some(header) = payload.first().copied() else {
            return Err(codec_error(
                SmoothCodecConfigurationError::UnexpectedH264NalUnit,
            ));
        };
        match header & 0x1f {
            7 if sequence_parameter_set_seen => {
                return Err(codec_error(
                    SmoothCodecConfigurationError::DuplicateH264SequenceParameterSet,
                ));
            }
            7 => sequence_parameter_set_seen = true,
            8 if picture_parameter_set_seen => {
                return Err(codec_error(
                    SmoothCodecConfigurationError::DuplicateH264PictureParameterSet,
                ));
            }
            8 => picture_parameter_set_seen = true,
            _ => {
                return Err(codec_error(
                    SmoothCodecConfigurationError::UnexpectedH264NalUnit,
                ));
            }
        }
        cursor = next_start;
    }
    if !sequence_parameter_set_seen {
        return Err(codec_error(
            SmoothCodecConfigurationError::MissingH264SequenceParameterSet,
        ));
    }
    if !picture_parameter_set_seen {
        return Err(codec_error(
            SmoothCodecConfigurationError::MissingH264PictureParameterSet,
        ));
    }
    Ok(())
}

/// Читает минимальные AudioSpecificConfig fields, достаточные для proof `mp4a.40.2`.
fn validate_aac_lc_audio_specific_config(
    bytes: &[u8],
    expected_sampling_rate: u32,
    expected_channels: u16,
) -> Result<(), SmoothManifestError> {
    let mut bits = BitReader::new(bytes);
    let object_type = bits.read(5)?;
    if object_type != 2 {
        return Err(codec_error(
            SmoothCodecConfigurationError::AacObjectTypeMismatch,
        ));
    }
    let frequency_index = bits.read(4)?;
    let sampling_rate = if frequency_index == 15 {
        bits.read(24)?
    } else {
        aac_sampling_rate(frequency_index).ok_or_else(|| {
            codec_error(SmoothCodecConfigurationError::InvalidAacAudioSpecificConfig)
        })?
    };
    let channel_configuration = bits.read(4)?;
    if sampling_rate != expected_sampling_rate {
        return Err(codec_error(
            SmoothCodecConfigurationError::AacSamplingRateMismatch,
        ));
    }
    if channel_configuration != u32::from(expected_channels) {
        return Err(codec_error(
            SmoothCodecConfigurationError::AacChannelCountMismatch,
        ));
    }
    Ok(())
}

/// Строит canonical двухбайтовый ASC для standard indexed sample rate.
fn derive_aac_lc_audio_specific_config(
    sampling_rate: u32,
    channels: u16,
) -> Result<Box<[u8]>, SmoothManifestError> {
    let frequency_index = (0u32..=12)
        .find(|index| aac_sampling_rate(*index) == Some(sampling_rate))
        .ok_or_else(|| codec_error(SmoothCodecConfigurationError::InvalidAacAudioSpecificConfig))?;
    if channels == 0 || channels > 15 {
        return Err(codec_error(
            SmoothCodecConfigurationError::AacChannelCountMismatch,
        ));
    }
    let packed = (2u16 << 11)
        | (u16::try_from(frequency_index).expect("AAC frequency index <= 12") << 7)
        | (channels << 3);
    Ok(packed.to_be_bytes().into())
}

/// Возвращает standard ISO/IEC 14496-3 frequency по четырёхбитному index.
const fn aac_sampling_rate(index: u32) -> Option<u32> {
    match index {
        0 => Some(96_000),
        1 => Some(88_200),
        2 => Some(64_000),
        3 => Some(48_000),
        4 => Some(44_100),
        5 => Some(32_000),
        6 => Some(24_000),
        7 => Some(22_050),
        8 => Some(16_000),
        9 => Some(12_000),
        10 => Some(11_025),
        11 => Some(8_000),
        12 => Some(7_350),
        _ => None,
    }
}

/// Малый MSB-first reader не знает codec policy и никогда не читает за границей.
struct BitReader<'bytes> {
    bytes: &'bytes [u8],
    bit_offset: usize,
}

impl<'bytes> BitReader<'bytes> {
    /// Создаёт cursor над уже bounded payload.
    const fn new(bytes: &'bytes [u8]) -> Self {
        Self {
            bytes,
            bit_offset: 0,
        }
    }

    /// Читает до 24 bits, возвращая typed malformed codec error на truncation.
    fn read(&mut self, bit_count: usize) -> Result<u32, SmoothManifestError> {
        let final_offset = self.bit_offset.checked_add(bit_count).ok_or_else(|| {
            codec_error(SmoothCodecConfigurationError::InvalidAacAudioSpecificConfig)
        })?;
        if final_offset > self.bytes.len().saturating_mul(8) {
            return Err(codec_error(
                SmoothCodecConfigurationError::InvalidAacAudioSpecificConfig,
            ));
        }
        let mut value = 0u32;
        while self.bit_offset < final_offset {
            let byte = self.bytes[self.bit_offset / 8];
            let bit = (byte >> (7 - self.bit_offset % 8)) & 1;
            value = (value << 1) | u32::from(bit);
            self.bit_offset += 1;
        }
        Ok(value)
    }
}

/// Общий constructor сохраняет codec taxonomy в одном месте.
const fn codec_error(reason: SmoothCodecConfigurationError) -> SmoothManifestError {
    SmoothManifestError::InvalidCodecConfiguration { reason }
}

/// Cancellation остаётся отдельным outcome и не маскируется codec error-ом.
fn check_cancelled(is_cancelled: &mut dyn FnMut() -> bool) -> Result<(), SmoothManifestError> {
    if is_cancelled() {
        Err(SmoothManifestError::Cancelled)
    } else {
        Ok(())
    }
}
