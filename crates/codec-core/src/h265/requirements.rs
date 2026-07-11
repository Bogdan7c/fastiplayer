//! SPS metadata parsing и requirement policy для H.265/HEVC.

use super::*;

/// Парсит первый SPS из `hvcC` и возвращает v1 requirement metadata.
pub fn h265_sps_metadata_from_hevc_decoder_configuration_record(
    record_bytes: &[u8],
) -> Result<H265SpsMetadata, H265RequirementError> {
    let record = parse_hevc_decoder_configuration_record(record_bytes)
        .map_err(H265RequirementError::HevcDecoderConfigurationRecord)?;
    let sps = record
        .sequence_parameter_sets()
        .first()
        .ok_or(H265RequirementError::MissingSequenceParameterSet)?;

    parse_h265_sps_metadata(sps).map_err(H265RequirementError::SequenceParameterSet)
}

/// Ищет SPS внутри packet-а и возвращает v1 requirement metadata.
pub fn h265_sps_metadata_from_packet(
    packet_bytes: &[u8],
    packetization: H265Packetization,
) -> Result<H265SpsMetadata, H265RequirementError> {
    let nal_units =
        h265_nal_units(packet_bytes, packetization).map_err(H265RequirementError::ByteStream)?;

    for nal_unit in nal_units {
        if nal_unit.nal_unit_type() == HEVC_NAL_TYPE_SPS {
            return parse_h265_sps_metadata(nal_unit.bytes())
                .map_err(H265RequirementError::SequenceParameterSet);
        }
    }

    Err(H265RequirementError::MissingSequenceParameterSet)
}

/// Строит requirement из `hvcC`; если SPS отсутствует, использует безопасные header-level поля.
pub fn h265_decode_requirement_from_hevc_decoder_configuration_record(
    record_bytes: &[u8],
) -> Result<VideoDecodeRequirement, H265RequirementError> {
    let record = parse_hevc_decoder_configuration_record(record_bytes)
        .map_err(H265RequirementError::HevcDecoderConfigurationRecord)?;

    if let Some(sps) = record.sequence_parameter_sets().first() {
        let metadata =
            parse_h265_sps_metadata(sps).map_err(H265RequirementError::SequenceParameterSet)?;
        return Ok(video_requirement_from_h265_sps_metadata(&metadata));
    }

    let metadata = h265_header_metadata_from_hevc_decoder_configuration_record(&record)?;
    Ok(video_requirement_from_h265_header_metadata(&metadata))
}

/// Строит requirement из in-band SPS внутри access unit-а.
pub fn h265_decode_requirement_from_packet(
    packet_bytes: &[u8],
    packetization: H265Packetization,
) -> Result<VideoDecodeRequirement, H265RequirementError> {
    let metadata = h265_sps_metadata_from_packet(packet_bytes, packetization)?;
    Ok(video_requirement_from_h265_sps_metadata(&metadata))
}

/// Парсит SPS NAL unit и подтверждает v1 supported subset.
pub fn parse_h265_sps_metadata(nal_unit_bytes: &[u8]) -> Result<H265SpsMetadata, H265SpsError> {
    let nal_header = parse_nal_header(nal_unit_bytes).map_err(|error| match error {
        H265ByteStreamError::TruncatedNalHeader { nal_unit_size } => {
            H265SpsError::TruncatedNalHeader { nal_unit_size }
        }
        H265ByteStreamError::InvalidNalHeader {
            first_header_byte,
            second_header_byte,
        } => H265SpsError::InvalidNalHeader {
            first_header_byte,
            second_header_byte,
        },
        H265ByteStreamError::InvalidTemporalId => H265SpsError::InvalidTemporalId,
        other_error => unreachable!("parse_nal_header returned unexpected error: {other_error:?}"),
    })?;
    if nal_header.nal_unit_type != HEVC_NAL_TYPE_SPS {
        return Err(H265SpsError::UnexpectedNalUnitType {
            nal_unit_type: nal_header.nal_unit_type,
        });
    }

    let rbsp_bytes = ebsp_to_rbsp(&nal_unit_bytes[2..]);
    let mut bit_reader = H265BitReader::new(&rbsp_bytes);

    let _sps_video_parameter_set_id = bit_reader.read_bits(4)? as u8;
    let max_sub_layers_minus1 = bit_reader.read_bits(3)? as u8;
    if max_sub_layers_minus1 > 6 {
        return Err(H265SpsError::InvalidSubLayerCount {
            max_sub_layers_minus1,
        });
    }
    let _sps_temporal_id_nesting_flag = bit_reader.read_bit()?;
    let profile_tier_level =
        parse_profile_tier_level(&mut bit_reader, true, max_sub_layers_minus1)?;
    let _sps_seq_parameter_set_id = bit_reader.read_ue()?;
    let chroma_format_idc = bit_reader.read_ue()?;
    let separate_colour_plane_flag = if chroma_format_idc == 3 {
        bit_reader.read_bit()?
    } else {
        false
    };
    let pic_width_in_luma_samples = bit_reader.read_ue()?;
    let pic_height_in_luma_samples = bit_reader.read_ue()?;
    let conformance_window = if bit_reader.read_bit()? {
        H265ConformanceWindow {
            left: bit_reader.read_ue()?,
            right: bit_reader.read_ue()?,
            top: bit_reader.read_ue()?,
            bottom: bit_reader.read_ue()?,
        }
    } else {
        H265ConformanceWindow::default()
    };
    let bit_depth_luma = read_bit_depth_from_sps(&mut bit_reader)?;
    let bit_depth_chroma = read_bit_depth_from_sps(&mut bit_reader)?;
    let decoded_format = refine_h265_decoded_format(
        profile_tier_level,
        bit_depth_luma,
        bit_depth_chroma,
        chroma_format_idc,
    )
    .map_err(H265SpsError::from)?;
    let (width, height) = sps_display_dimensions(
        pic_width_in_luma_samples,
        pic_height_in_luma_samples,
        chroma_format_idc,
        separate_colour_plane_flag,
        conformance_window,
    )?;

    Ok(H265SpsMetadata {
        profile: decoded_format.profile,
        level_idc: profile_tier_level.level_idc,
        bit_depth: decoded_format.bit_depth,
        chroma: decoded_format.chroma,
        width,
        height,
        surface_format: decoded_format.surface_format,
    })
}

/// Строит header-level metadata из уже разобранного `hvcC`.
pub fn h265_header_metadata_from_hevc_decoder_configuration_record(
    record: &HevcDecoderConfigurationRecord,
) -> Result<H265HeaderMetadata, H265RequirementError> {
    let decoded_format = refine_h265_decoded_format(
        record.profile_tier_level,
        record.bit_depth_luma,
        record.bit_depth_chroma,
        u32::from(record.chroma_format_idc),
    )
    .map_err(H265RequirementError::from)?;

    Ok(H265HeaderMetadata {
        profile: decoded_format.profile,
        level_idc: record.profile_tier_level.level_idc,
        bit_depth: decoded_format.bit_depth,
        chroma: decoded_format.chroma,
        surface_format: decoded_format.surface_format,
    })
}

fn parse_profile_tier_level(
    bit_reader: &mut H265BitReader<'_>,
    profile_present_flag: bool,
    max_sub_layers_minus1: u8,
) -> Result<H265ProfileTierLevel, H265BitReaderError> {
    let profile_tier_level = if profile_present_flag {
        parse_profile_tier_level_profile_fields(bit_reader)?
    } else {
        H265ProfileTierLevel {
            profile_space: 0,
            tier_flag: false,
            profile_idc: 0,
            profile_compatibility_flags: 0,
            constraint_indicator_flags: [0; 6],
            level_idc: 0,
        }
    };
    let level_idc = bit_reader.read_bits(8)? as u8;
    let mut sub_layer_profile_present_flags = [false; 6];
    let mut sub_layer_level_present_flags = [false; 6];

    for sub_layer_index in 0..usize::from(max_sub_layers_minus1) {
        sub_layer_profile_present_flags[sub_layer_index] = bit_reader.read_bit()?;
        sub_layer_level_present_flags[sub_layer_index] = bit_reader.read_bit()?;
    }

    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            bit_reader.skip_bits(2)?;
        }
    }

    for sub_layer_index in 0..usize::from(max_sub_layers_minus1) {
        if sub_layer_profile_present_flags[sub_layer_index] {
            bit_reader.skip_bits(88)?;
        }
        if sub_layer_level_present_flags[sub_layer_index] {
            bit_reader.skip_bits(8)?;
        }
    }

    Ok(H265ProfileTierLevel {
        level_idc,
        ..profile_tier_level
    })
}

fn parse_profile_tier_level_profile_fields(
    bit_reader: &mut H265BitReader<'_>,
) -> Result<H265ProfileTierLevel, H265BitReaderError> {
    let profile_space = bit_reader.read_bits(2)? as u8;
    let tier_flag = bit_reader.read_bit()?;
    let profile_idc = bit_reader.read_bits(5)? as u8;
    let mut profile_compatibility_flags = 0_u32;
    for profile_index in 0..32 {
        if bit_reader.read_bit()? {
            profile_compatibility_flags |= 1_u32 << profile_index;
        }
    }
    let mut constraint_indicator_flags = [0_u8; 6];
    for constraint_byte in &mut constraint_indicator_flags {
        *constraint_byte = bit_reader.read_bits(8)? as u8;
    }

    Ok(H265ProfileTierLevel {
        profile_space,
        tier_flag,
        profile_idc,
        profile_compatibility_flags,
        constraint_indicator_flags,
        level_idc: 0,
    })
}

fn read_bit_depth_from_sps(bit_reader: &mut H265BitReader<'_>) -> Result<u8, H265SpsError> {
    let bit_depth_minus8 = bit_reader.read_ue()?;
    bit_depth_minus8
        .checked_add(8)
        .and_then(|value| u8::try_from(value).ok())
        .ok_or(H265SpsError::UnsupportedBitDepth {
            bit_depth_luma: u8::MAX,
            bit_depth_chroma: u8::MAX,
        })
}

#[derive(Debug, Clone, Copy, Default)]
struct H265ConformanceWindow {
    left: u32,
    right: u32,
    top: u32,
    bottom: u32,
}

fn sps_display_dimensions(
    pic_width_in_luma_samples: u32,
    pic_height_in_luma_samples: u32,
    chroma_format_idc: u32,
    separate_colour_plane_flag: bool,
    conformance_window: H265ConformanceWindow,
) -> Result<(u32, u32), H265SpsError> {
    if pic_width_in_luma_samples == 0 || pic_height_in_luma_samples == 0 {
        return Err(H265SpsError::InvalidDimensions);
    }

    let (sub_width_c, sub_height_c) =
        chroma_subsampling_units(chroma_format_idc, separate_colour_plane_flag);
    let crop_width = conformance_window
        .left
        .checked_add(conformance_window.right)
        .and_then(|crop_sum| crop_sum.checked_mul(sub_width_c))
        .ok_or(H265SpsError::InvalidDimensions)?;
    let crop_height = conformance_window
        .top
        .checked_add(conformance_window.bottom)
        .and_then(|crop_sum| crop_sum.checked_mul(sub_height_c))
        .ok_or(H265SpsError::InvalidDimensions)?;
    let width = pic_width_in_luma_samples
        .checked_sub(crop_width)
        .ok_or(H265SpsError::InvalidDimensions)?;
    let height = pic_height_in_luma_samples
        .checked_sub(crop_height)
        .ok_or(H265SpsError::InvalidDimensions)?;

    if width == 0 || height == 0 {
        return Err(H265SpsError::InvalidDimensions);
    }

    Ok((width, height))
}

fn chroma_subsampling_units(
    chroma_format_idc: u32,
    separate_colour_plane_flag: bool,
) -> (u32, u32) {
    if chroma_format_idc == 0 || separate_colour_plane_flag {
        return (1, 1);
    }

    match chroma_format_idc {
        1 => (2, 2),
        2 => (2, 1),
        3 => (1, 1),
        _ => (1, 1),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct H265DecodedFormat {
    profile: H265Profile,
    bit_depth: BitDepth,
    chroma: ChromaSubsampling,
    surface_format: VideoFramePixelLayout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum H265FormatError {
    Profile {
        profile_idc: u8,
        profile_compatibility_flags: u32,
        reason: &'static str,
    },
    BitDepth {
        bit_depth_luma: u8,
        bit_depth_chroma: u8,
    },
    Chroma {
        chroma_format_idc: u32,
    },
    ProfileFormat {
        profile: H265Profile,
        bit_depth: BitDepth,
        chroma: ChromaSubsampling,
    },
}

impl From<H265FormatError> for H265SpsError {
    fn from(error: H265FormatError) -> Self {
        match error {
            H265FormatError::Profile {
                profile_idc,
                profile_compatibility_flags,
                reason,
            } => Self::UnsupportedProfile {
                profile_idc,
                profile_compatibility_flags,
                reason,
            },
            H265FormatError::BitDepth {
                bit_depth_luma,
                bit_depth_chroma,
            } => Self::UnsupportedBitDepth {
                bit_depth_luma,
                bit_depth_chroma,
            },
            H265FormatError::Chroma { chroma_format_idc } => {
                Self::UnsupportedChroma { chroma_format_idc }
            }
            H265FormatError::ProfileFormat {
                profile,
                bit_depth,
                chroma,
            } => Self::UnsupportedProfileFormat {
                profile,
                bit_depth,
                chroma,
            },
        }
    }
}

impl From<H265FormatError> for H265RequirementError {
    fn from(error: H265FormatError) -> Self {
        match error {
            H265FormatError::Profile {
                profile_idc,
                profile_compatibility_flags,
                reason,
            } => Self::UnsupportedProfile {
                profile_idc,
                profile_compatibility_flags,
                reason,
            },
            H265FormatError::BitDepth {
                bit_depth_luma,
                bit_depth_chroma,
            } => Self::UnsupportedBitDepth {
                bit_depth_luma,
                bit_depth_chroma,
            },
            H265FormatError::Chroma { chroma_format_idc } => {
                Self::UnsupportedChroma { chroma_format_idc }
            }
            H265FormatError::ProfileFormat {
                profile,
                bit_depth,
                chroma,
            } => Self::UnsupportedProfileFormat {
                profile,
                bit_depth,
                chroma,
            },
        }
    }
}

fn refine_h265_decoded_format(
    profile_tier_level: H265ProfileTierLevel,
    bit_depth_luma: u8,
    bit_depth_chroma: u8,
    chroma_format_idc: u32,
) -> Result<H265DecodedFormat, H265FormatError> {
    let bit_depth = normalize_h265_bit_depth(bit_depth_luma, bit_depth_chroma)?;
    let chroma = normalize_h265_chroma(chroma_format_idc)?;

    if profile_tier_level.is_profile_compatible(9) {
        return Err(H265FormatError::Profile {
            profile_idc: profile_tier_level.profile_idc,
            profile_compatibility_flags: profile_tier_level.profile_compatibility_flags,
            reason: "HEVC Screen Content Coding profiles are reserved for a future zero-copy path",
        });
    }

    let profile = if bit_depth == BitDepth::Eight && profile_tier_level.is_profile_compatible(1) {
        H265Profile::Main
    } else if bit_depth == BitDepth::Ten && profile_tier_level.is_profile_compatible(2) {
        H265Profile::Main10
    } else if profile_tier_level.is_profile_compatible(1) {
        return Err(H265FormatError::ProfileFormat {
            profile: H265Profile::Main,
            bit_depth,
            chroma,
        });
    } else if profile_tier_level.is_profile_compatible(2) {
        return Err(H265FormatError::ProfileFormat {
            profile: H265Profile::Main10,
            bit_depth,
            chroma,
        });
    } else {
        return Err(H265FormatError::Profile {
            profile_idc: profile_tier_level.profile_idc,
            profile_compatibility_flags: profile_tier_level.profile_compatibility_flags,
            reason: "profile is not part of the H.265 v1 zero-copy subset",
        });
    };

    let surface_format = video_frame_pixel_layout_from_codec_fields(bit_depth, chroma).ok_or(
        H265FormatError::ProfileFormat {
            profile,
            bit_depth,
            chroma,
        },
    )?;

    Ok(H265DecodedFormat {
        profile,
        bit_depth,
        chroma,
        surface_format,
    })
}

fn normalize_h265_bit_depth(
    bit_depth_luma: u8,
    bit_depth_chroma: u8,
) -> Result<BitDepth, H265FormatError> {
    if bit_depth_luma != bit_depth_chroma {
        return Err(H265FormatError::BitDepth {
            bit_depth_luma,
            bit_depth_chroma,
        });
    }

    match BitDepth::from_bits(bit_depth_luma) {
        Some(BitDepth::Eight) => Ok(BitDepth::Eight),
        Some(BitDepth::Ten) => Ok(BitDepth::Ten),
        Some(BitDepth::Twelve) | None => Err(H265FormatError::BitDepth {
            bit_depth_luma,
            bit_depth_chroma,
        }),
    }
}

fn normalize_h265_chroma(chroma_format_idc: u32) -> Result<ChromaSubsampling, H265FormatError> {
    match chroma_format_idc {
        1 => Ok(ChromaSubsampling::Yuv420),
        0 | 2 | 3 => Err(H265FormatError::Chroma { chroma_format_idc }),
        _ => Err(H265FormatError::Chroma { chroma_format_idc }),
    }
}

fn video_requirement_from_h265_sps_metadata(metadata: &H265SpsMetadata) -> VideoDecodeRequirement {
    VideoDecodeRequirement::new(VideoCodec::H265)
        .with_profile(VideoProfile::H265(metadata.profile))
        .with_bit_depth(metadata.bit_depth)
        .with_chroma(metadata.chroma)
        .with_resolution(metadata.width, metadata.height)
}

fn video_requirement_from_h265_header_metadata(
    metadata: &H265HeaderMetadata,
) -> VideoDecodeRequirement {
    VideoDecodeRequirement::new(VideoCodec::H265)
        .with_profile(VideoProfile::H265(metadata.profile))
        .with_bit_depth(metadata.bit_depth)
        .with_chroma(metadata.chroma)
}

fn ebsp_to_rbsp(ebsp_bytes: &[u8]) -> Vec<u8> {
    let mut rbsp_bytes = Vec::with_capacity(ebsp_bytes.len());
    let mut consecutive_zero_count = 0_u8;

    for &byte in ebsp_bytes {
        if consecutive_zero_count >= 2 && byte == 0x03 {
            consecutive_zero_count = 0;
            continue;
        }

        rbsp_bytes.push(byte);
        if byte == 0 {
            consecutive_zero_count = consecutive_zero_count.saturating_add(1);
        } else {
            consecutive_zero_count = 0;
        }
    }

    rbsp_bytes
}

struct H265BitReader<'a> {
    rbsp_bytes: &'a [u8],
    bit_offset: usize,
}

impl<'a> H265BitReader<'a> {
    fn new(rbsp_bytes: &'a [u8]) -> Self {
        Self {
            rbsp_bytes,
            bit_offset: 0,
        }
    }

    fn read_bit(&mut self) -> Result<bool, H265BitReaderError> {
        Ok(self.read_bits(1)? != 0)
    }

    fn read_bits(&mut self, bit_count: usize) -> Result<u32, H265BitReaderError> {
        let remaining_bits = self.remaining_bits();
        if remaining_bits < bit_count {
            return Err(H265BitReaderError::UnexpectedEnd {
                requested_bits: bit_count,
                remaining_bits,
            });
        }

        let mut value = 0_u32;
        for _ in 0..bit_count {
            let byte_offset = self.bit_offset / 8;
            let bit_in_byte = 7 - (self.bit_offset % 8);
            let bit = (self.rbsp_bytes[byte_offset] >> bit_in_byte) & 1;
            value = (value << 1) | u32::from(bit);
            self.bit_offset += 1;
        }

        Ok(value)
    }

    fn skip_bits(&mut self, bit_count: usize) -> Result<(), H265BitReaderError> {
        let remaining_bits = self.remaining_bits();
        if remaining_bits < bit_count {
            return Err(H265BitReaderError::UnexpectedEnd {
                requested_bits: bit_count,
                remaining_bits,
            });
        }
        self.bit_offset += bit_count;
        Ok(())
    }

    fn read_ue(&mut self) -> Result<u32, H265BitReaderError> {
        let mut leading_zero_bits = 0_u8;
        while !self.read_bit()? {
            leading_zero_bits = leading_zero_bits
                .checked_add(1)
                .ok_or(H265BitReaderError::ExpGolombOverflow)?;
            if leading_zero_bits >= 32 {
                return Err(H265BitReaderError::ExpGolombOverflow);
            }
        }

        if leading_zero_bits == 0 {
            return Ok(0);
        }

        let suffix = self.read_bits(usize::from(leading_zero_bits))?;
        Ok((1_u32 << leading_zero_bits) - 1 + suffix)
    }

    fn remaining_bits(&self) -> usize {
        self.rbsp_bytes.len() * 8 - self.bit_offset
    }
}
