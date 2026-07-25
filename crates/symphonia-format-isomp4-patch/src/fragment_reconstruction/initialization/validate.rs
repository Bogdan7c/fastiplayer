//! Codec-specific validation до box planning.

use super::error::{
    FragmentCodecConfigurationIssue, FragmentCodecKind, FragmentInitializationError,
};
use super::model::{
    FragmentAacAudioSpecificConfig, FragmentAacChannelCount, FragmentAacSampleRate,
};

/// Валидирует чистый SPS/PPS NAL unit и его `avcC` length field.
pub(super) fn validate_h264_parameter_set(
    bytes: &[u8],
    expected_nal_unit_type: u8,
    require_profile_fields: bool,
) -> Result<(), FragmentInitializationError> {
    if bytes.is_empty() {
        return Err(invalid_h264(FragmentCodecConfigurationIssue::Empty));
    }
    if bytes.len() > usize::from(u16::MAX) {
        return Err(invalid_h264(
            FragmentCodecConfigurationIssue::ParameterSetTooLarge,
        ));
    }
    if bytes.windows(3).any(|window| window == [0, 0, 1]) {
        return Err(invalid_h264(
            FragmentCodecConfigurationIssue::AnnexBStartCode,
        ));
    }
    if bytes[0] & 0x80 != 0 {
        return Err(invalid_h264(
            FragmentCodecConfigurationIssue::H264ForbiddenZeroBit,
        ));
    }

    let actual_nal_unit_type = bytes[0] & 0x1f;
    if actual_nal_unit_type != expected_nal_unit_type {
        return Err(invalid_h264(
            FragmentCodecConfigurationIssue::UnexpectedNalUnitType {
                expected: expected_nal_unit_type,
                actual: actual_nal_unit_type,
            },
        ));
    }
    if require_profile_fields && bytes.len() < 4 {
        return Err(invalid_h264(
            FragmentCodecConfigurationIssue::TruncatedSequenceParameterSet,
        ));
    }
    if !require_profile_fields && bytes.len() < 2 {
        return Err(invalid_h264(
            FragmentCodecConfigurationIssue::TruncatedPictureParameterSet,
        ));
    }
    Ok(())
}

/// Валидирует узкий двухбайтовый AAC-LC ASC.
pub(super) fn validate_aac_specific_config(
    bytes: &[u8],
) -> Result<(), FragmentInitializationError> {
    if bytes.is_empty() {
        return Err(invalid_aac(FragmentCodecConfigurationIssue::Empty));
    }
    if bytes.len() != 2 {
        return Err(invalid_aac(
            FragmentCodecConfigurationIssue::InvalidAudioSpecificConfigLength,
        ));
    }

    let object_type = bytes[0] >> 3;
    if object_type != 2 {
        return Err(invalid_aac(
            FragmentCodecConfigurationIssue::UnsupportedAacObjectType {
                actual: object_type,
            },
        ));
    }

    let frequency_index = ((bytes[0] & 0x07) << 1) | (bytes[1] >> 7);
    if aac_sampling_rate(frequency_index).is_none() {
        return Err(invalid_aac(
            FragmentCodecConfigurationIssue::UnsupportedAacSamplingFrequency,
        ));
    }

    let channel_configuration = (bytes[1] >> 3) & 0x0f;
    if aac_channel_count(channel_configuration).is_none() {
        return Err(invalid_aac(
            FragmentCodecConfigurationIssue::UnsupportedAacChannelConfiguration {
                actual: channel_configuration,
            },
        ));
    }

    if bytes[1] & 0x07 != 0 {
        return Err(invalid_aac(
            FragmentCodecConfigurationIssue::UnsupportedAacExtension,
        ));
    }
    Ok(())
}

/// Доказывает совпадение typed AAC metadata и ASC.
pub(super) fn validate_aac_lc_configuration(
    sample_rate: FragmentAacSampleRate,
    channel_count: FragmentAacChannelCount,
    audio_specific_config: FragmentAacAudioSpecificConfig<'_>,
) -> Result<(), FragmentInitializationError> {
    let bytes = audio_specific_config.as_bytes();
    let frequency_index = ((bytes[0] & 0x07) << 1) | (bytes[1] >> 7);
    let declared_sampling_rate = aac_sampling_rate(frequency_index).ok_or_else(|| {
        invalid_aac(FragmentCodecConfigurationIssue::UnsupportedAacSamplingFrequency)
    })?;
    if declared_sampling_rate != sample_rate.get() {
        return Err(incompatible_aac(
            FragmentCodecConfigurationIssue::AacSampleRateMismatch,
        ));
    }

    let channel_configuration = (bytes[1] >> 3) & 0x0f;
    let declared_channel_count = aac_channel_count(channel_configuration).ok_or_else(|| {
        invalid_aac(
            FragmentCodecConfigurationIssue::UnsupportedAacChannelConfiguration {
                actual: channel_configuration,
            },
        )
    })?;
    if declared_channel_count != channel_count.get() {
        return Err(incompatible_aac(
            FragmentCodecConfigurationIssue::AacChannelCountMismatch,
        ));
    }
    Ok(())
}

/// Таблица MPEG-4 indexed samplingFrequencyIndex.
fn aac_sampling_rate(index: u8) -> Option<u32> {
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

/// Каноническое число channels для indexed channelConfiguration.
fn aac_channel_count(configuration: u8) -> Option<u16> {
    match configuration {
        1 => Some(1),
        2 => Some(2),
        3 => Some(3),
        4 => Some(4),
        5 => Some(5),
        6 => Some(6),
        7 => Some(8),
        _ => None,
    }
}

fn invalid_h264(issue: FragmentCodecConfigurationIssue) -> FragmentInitializationError {
    FragmentInitializationError::InvalidCodecConfiguration {
        codec: FragmentCodecKind::H264Avc1,
        issue,
    }
}

fn invalid_aac(issue: FragmentCodecConfigurationIssue) -> FragmentInitializationError {
    FragmentInitializationError::InvalidCodecConfiguration {
        codec: FragmentCodecKind::AacLowComplexity,
        issue,
    }
}

fn incompatible_aac(issue: FragmentCodecConfigurationIssue) -> FragmentInitializationError {
    FragmentInitializationError::IncompatibleCodecConfiguration {
        codec: FragmentCodecKind::AacLowComplexity,
        issue,
    }
}
