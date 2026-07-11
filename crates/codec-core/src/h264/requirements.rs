//! SPS metadata parsing и requirement policy для H.264.

use super::*;

/// Парсит первый SPS из `avcC` и возвращает v1 requirement metadata.
pub fn h264_sps_metadata_from_avc_decoder_configuration_record(
    record_bytes: &[u8],
) -> Result<H264SpsMetadata, H264RequirementError> {
    let record = parse_avc_decoder_configuration_record(record_bytes)
        .map_err(H264RequirementError::AvcDecoderConfigurationRecord)?;
    let sps = record
        .sequence_parameter_sets()
        .first()
        .ok_or(H264RequirementError::MissingSequenceParameterSet)?;

    parse_h264_sps_metadata(sps).map_err(H264RequirementError::SequenceParameterSet)
}

/// Ищет SPS внутри packet-а и возвращает v1 requirement metadata.
pub fn h264_sps_metadata_from_packet(
    packet_bytes: &[u8],
    packetization: H264Packetization,
) -> Result<H264SpsMetadata, H264RequirementError> {
    let nal_units =
        h264_nal_units(packet_bytes, packetization).map_err(H264RequirementError::ByteStream)?;

    for nal_unit in nal_units {
        if nal_unit.nal_unit_type() == H264_NAL_TYPE_SPS {
            return parse_h264_sps_metadata(nal_unit.bytes())
                .map_err(H264RequirementError::SequenceParameterSet);
        }
    }

    Err(H264RequirementError::MissingSequenceParameterSet)
}

/// Парсит SPS NAL unit и подтверждает v1 supported subset: 8-bit 4:2:0 CBP/Main/High.
pub fn parse_h264_sps_metadata(nal_unit_bytes: &[u8]) -> Result<H264SpsMetadata, H264SpsError> {
    let nal_header = parse_nal_header(nal_unit_bytes).map_err(|error| match error {
        H264ByteStreamError::EmptyNalUnit => H264SpsError::EmptyNalUnit,
        H264ByteStreamError::InvalidNalHeader { header } => {
            H264SpsError::InvalidNalHeader { header }
        }
        other_error => unreachable!("parse_nal_header returned unexpected error: {other_error:?}"),
    })?;
    if nal_header.nal_unit_type != H264_NAL_TYPE_SPS {
        return Err(H264SpsError::UnexpectedNalUnitType {
            nal_unit_type: nal_header.nal_unit_type,
        });
    }

    let rbsp_bytes = ebsp_to_rbsp(&nal_unit_bytes[1..]);
    let mut bit_reader = H264BitReader::new(&rbsp_bytes);

    let profile_idc = bit_reader.read_u8()?;
    let constraint_flags = bit_reader.read_u8()?;
    let level_idc = bit_reader.read_u8()?;
    let profile =
        validate_h264_profile(profile_idc, constraint_flags).map_err(|unsupported_profile| {
            H264SpsError::UnsupportedProfile {
                profile_idc,
                constraint_flags,
                reason: unsupported_profile.reason,
            }
        })?;
    let _seq_parameter_set_id = bit_reader.read_ue()?;

    let high_syntax = profile_uses_high_syntax(profile_idc);
    let mut chroma_format_idc = 1;
    let mut separate_colour_plane_flag = false;
    let mut bit_depth_luma = 8;
    let mut bit_depth_chroma = 8;

    if high_syntax {
        chroma_format_idc = bit_reader.read_ue()?;
        if chroma_format_idc == 3 {
            separate_colour_plane_flag = bit_reader.read_bit()?;
        }
        bit_depth_luma = read_bit_depth_from_sps(&mut bit_reader)?;
        bit_depth_chroma = read_bit_depth_from_sps(&mut bit_reader)?;
        let _qpprime_y_zero_transform_bypass_flag = bit_reader.read_bit()?;
        if bit_reader.read_bit()? {
            skip_scaling_matrices(&mut bit_reader, chroma_format_idc)?;
        }
    }

    if bit_depth_luma != 8 || bit_depth_chroma != 8 {
        return Err(H264SpsError::UnsupportedBitDepth {
            bit_depth: bit_depth_luma.max(bit_depth_chroma),
        });
    }
    let bit_depth = BitDepth::Eight;

    let chroma = match chroma_format_idc {
        1 => ChromaSubsampling::Yuv420,
        unsupported_chroma => {
            return Err(H264SpsError::UnsupportedChroma {
                chroma_format_idc: unsupported_chroma,
            });
        }
    };

    let _log2_max_frame_num_minus4 = bit_reader.read_ue()?;
    let pic_order_cnt_type = bit_reader.read_ue()?;
    skip_pic_order_count_fields(&mut bit_reader, pic_order_cnt_type)?;
    let _max_num_ref_frames = bit_reader.read_ue()?;
    let _gaps_in_frame_num_value_allowed_flag = bit_reader.read_bit()?;

    let pic_width_in_mbs_minus1 = bit_reader.read_ue()?;
    let pic_height_in_map_units_minus1 = bit_reader.read_ue()?;
    let frame_mbs_only_flag = bit_reader.read_bit()?;
    if !frame_mbs_only_flag {
        let _mb_adaptive_frame_field_flag = bit_reader.read_bit()?;
    }
    let _direct_8x8_inference_flag = bit_reader.read_bit()?;

    let crop = if bit_reader.read_bit()? {
        FrameCropOffsets {
            left: bit_reader.read_ue()?,
            right: bit_reader.read_ue()?,
            top: bit_reader.read_ue()?,
            bottom: bit_reader.read_ue()?,
        }
    } else {
        FrameCropOffsets::default()
    };

    let color = if bit_reader.has_more_bits() && bit_reader.read_bit()? {
        parse_vui_color_metadata(&mut bit_reader)?
    } else {
        None
    };
    let (width, height) = sps_display_dimensions(
        pic_width_in_mbs_minus1,
        pic_height_in_map_units_minus1,
        frame_mbs_only_flag,
        chroma_format_idc,
        separate_colour_plane_flag,
        crop,
    )?;

    Ok(H264SpsMetadata {
        profile,
        level_idc,
        bit_depth,
        chroma,
        width,
        height,
        color,
    })
}

/// Requirement-level ошибка H.264 adapter-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H264RequirementError {
    /// `avcC` record не разобран.
    AvcDecoderConfigurationRecord(AvcDecoderConfigurationRecordError),

    /// Access unit framing не разобран.
    ByteStream(H264ByteStreamError),

    /// SPS отсутствует.
    MissingSequenceParameterSet,

    /// SPS найден, но не принят v1 parser-ом.
    SequenceParameterSet(H264SpsError),
}

impl fmt::Display for H264RequirementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AvcDecoderConfigurationRecord(error) => error.fmt(formatter),
            Self::ByteStream(error) => error.fmt(formatter),
            Self::MissingSequenceParameterSet => write!(formatter, "H.264 SPS is missing"),
            Self::SequenceParameterSet(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for H264RequirementError {}

pub(super) fn validate_h264_profile(
    profile_idc: u8,
    constraint_flags: u8,
) -> Result<H264Profile, UnsupportedH264Profile> {
    match profile_idc {
        66 if constraint_flags & 0b0100_0000 != 0 => Ok(H264Profile::ConstrainedBaseline),
        66 => Err(UnsupportedH264Profile {
            reason: "Baseline without constrained-baseline flag is not enabled in v1",
        }),
        77 => Ok(H264Profile::Main),
        100 => Ok(H264Profile::High),
        110 => Err(UnsupportedH264Profile {
            reason: "High 10 profile is reserved for a future zero-copy path",
        }),
        122 => Err(UnsupportedH264Profile {
            reason: "High 4:2:2 profile is reserved for a future zero-copy path",
        }),
        244 => Err(UnsupportedH264Profile {
            reason: "High 4:4:4 profile is reserved for a future zero-copy path",
        }),
        118 => Err(UnsupportedH264Profile {
            reason: "Multiview High profile is not part of H.264 v1",
        }),
        128 => Err(UnsupportedH264Profile {
            reason: "Stereo High profile is not part of H.264 v1",
        }),
        _ => Err(UnsupportedH264Profile {
            reason: "profile is not part of H.264 v1",
        }),
    }
}

fn profile_uses_high_syntax(profile_idc: u8) -> bool {
    matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UnsupportedH264Profile {
    pub(super) reason: &'static str,
}

fn read_bit_depth_from_sps(bit_reader: &mut H264BitReader<'_>) -> Result<u8, H264SpsError> {
    let bit_depth_minus8 = bit_reader.read_ue()?;
    let bit_depth = bit_depth_minus8
        .checked_add(8)
        .and_then(|value| u8::try_from(value).ok())
        .ok_or(H264SpsError::UnsupportedBitDepth { bit_depth: u8::MAX })?;
    Ok(bit_depth)
}

fn skip_scaling_matrices(
    bit_reader: &mut H264BitReader<'_>,
    chroma_format_idc: u32,
) -> Result<(), H264SpsError> {
    let scaling_list_count = if chroma_format_idc != 3 { 8 } else { 12 };
    for scaling_list_index in 0..scaling_list_count {
        if bit_reader.read_bit()? {
            let scaling_list_size = if scaling_list_index < 6 { 16 } else { 64 };
            skip_scaling_list(bit_reader, scaling_list_size)?;
        }
    }

    Ok(())
}

fn skip_scaling_list(
    bit_reader: &mut H264BitReader<'_>,
    scaling_list_size: usize,
) -> Result<(), H264SpsError> {
    let mut last_scale = 8_i32;
    let mut next_scale = 8_i32;

    for _ in 0..scaling_list_size {
        if next_scale != 0 {
            let delta_scale = bit_reader.read_se()?;
            next_scale = (last_scale + delta_scale + 256).rem_euclid(256);
        }
        if next_scale != 0 {
            last_scale = next_scale;
        }
    }

    Ok(())
}

fn skip_pic_order_count_fields(
    bit_reader: &mut H264BitReader<'_>,
    pic_order_cnt_type: u32,
) -> Result<(), H264SpsError> {
    match pic_order_cnt_type {
        0 => {
            let _log2_max_pic_order_cnt_lsb_minus4 = bit_reader.read_ue()?;
        }
        1 => {
            let _delta_pic_order_always_zero_flag = bit_reader.read_bit()?;
            let _offset_for_non_ref_pic = bit_reader.read_se()?;
            let _offset_for_top_to_bottom_field = bit_reader.read_se()?;
            let offset_cycle_count = bit_reader.read_ue()?;
            for _ in 0..offset_cycle_count {
                let _offset_for_ref_frame = bit_reader.read_se()?;
            }
        }
        _ => {}
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
struct FrameCropOffsets {
    left: u32,
    right: u32,
    top: u32,
    bottom: u32,
}

fn sps_display_dimensions(
    pic_width_in_mbs_minus1: u32,
    pic_height_in_map_units_minus1: u32,
    frame_mbs_only_flag: bool,
    chroma_format_idc: u32,
    separate_colour_plane_flag: bool,
    crop: FrameCropOffsets,
) -> Result<(u32, u32), H264SpsError> {
    let width_in_mbs = pic_width_in_mbs_minus1
        .checked_add(1)
        .ok_or(H264SpsError::InvalidDimensions)?;
    let height_in_map_units = pic_height_in_map_units_minus1
        .checked_add(1)
        .ok_or(H264SpsError::InvalidDimensions)?;
    let frame_height_multiplier = if frame_mbs_only_flag { 1 } else { 2 };
    let coded_width = width_in_mbs
        .checked_mul(16)
        .ok_or(H264SpsError::InvalidDimensions)?;
    let coded_height = height_in_map_units
        .checked_mul(16)
        .and_then(|height| height.checked_mul(frame_height_multiplier))
        .ok_or(H264SpsError::InvalidDimensions)?;

    let (crop_unit_x, crop_unit_y) = crop_units(
        chroma_format_idc,
        separate_colour_plane_flag,
        frame_mbs_only_flag,
    );
    let crop_width = crop
        .left
        .checked_add(crop.right)
        .and_then(|crop_sum| crop_sum.checked_mul(crop_unit_x))
        .ok_or(H264SpsError::InvalidDimensions)?;
    let crop_height = crop
        .top
        .checked_add(crop.bottom)
        .and_then(|crop_sum| crop_sum.checked_mul(crop_unit_y))
        .ok_or(H264SpsError::InvalidDimensions)?;
    let width = coded_width
        .checked_sub(crop_width)
        .ok_or(H264SpsError::InvalidDimensions)?;
    let height = coded_height
        .checked_sub(crop_height)
        .ok_or(H264SpsError::InvalidDimensions)?;

    if width == 0 || height == 0 {
        return Err(H264SpsError::InvalidDimensions);
    }

    Ok((width, height))
}

fn crop_units(
    chroma_format_idc: u32,
    separate_colour_plane_flag: bool,
    frame_mbs_only_flag: bool,
) -> (u32, u32) {
    if chroma_format_idc == 0 || separate_colour_plane_flag {
        return (1, if frame_mbs_only_flag { 1 } else { 2 });
    }

    let frame_height_multiplier = if frame_mbs_only_flag { 1 } else { 2 };
    match chroma_format_idc {
        1 => (2, 2 * frame_height_multiplier),
        2 => (2, frame_height_multiplier),
        3 => (1, frame_height_multiplier),
        _ => (1, frame_height_multiplier),
    }
}

fn parse_vui_color_metadata(
    bit_reader: &mut H264BitReader<'_>,
) -> Result<Option<VideoColorMetadata>, H264SpsError> {
    if bit_reader.read_bit()? {
        let aspect_ratio_idc = bit_reader.read_bits(8)?;
        if aspect_ratio_idc == 255 {
            let _sar_width = bit_reader.read_bits(16)?;
            let _sar_height = bit_reader.read_bits(16)?;
        }
    }

    if bit_reader.read_bit()? {
        let _overscan_appropriate_flag = bit_reader.read_bit()?;
    }

    if !bit_reader.read_bit()? {
        return Ok(None);
    }

    let _video_format = bit_reader.read_bits(3)?;
    let full_range = bit_reader.read_bit()?;
    if !bit_reader.read_bit()? {
        return Ok(None);
    }

    let primaries = ColorPrimaries::from_h273_value(u64::from(bit_reader.read_bits(8)?));
    let transfer = TransferFunction::from_h273_value(u64::from(bit_reader.read_bits(8)?));
    let matrix = MatrixCoefficients::from_h273_value(u64::from(bit_reader.read_bits(8)?));
    let range = if full_range {
        ColorRange::Full
    } else {
        ColorRange::Limited
    };

    Ok(Some(VideoColorMetadata::bitstream(
        range, matrix, primaries, transfer,
    )))
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

struct H264BitReader<'a> {
    rbsp_bytes: &'a [u8],
    bit_offset: usize,
}

impl<'a> H264BitReader<'a> {
    fn new(rbsp_bytes: &'a [u8]) -> Self {
        Self {
            rbsp_bytes,
            bit_offset: 0,
        }
    }

    fn has_more_bits(&self) -> bool {
        self.bit_offset < self.rbsp_bytes.len() * 8
    }

    fn read_bit(&mut self) -> Result<bool, H264BitReaderError> {
        Ok(self.read_bits(1)? != 0)
    }

    fn read_u8(&mut self) -> Result<u8, H264BitReaderError> {
        Ok(self.read_bits(8)? as u8)
    }

    fn read_bits(&mut self, bit_count: u8) -> Result<u32, H264BitReaderError> {
        let remaining_bits = self.remaining_bits();
        if remaining_bits < usize::from(bit_count) {
            return Err(H264BitReaderError::UnexpectedEnd {
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

    fn read_ue(&mut self) -> Result<u32, H264BitReaderError> {
        let mut leading_zero_bits = 0_u8;
        while !self.read_bit()? {
            leading_zero_bits = leading_zero_bits
                .checked_add(1)
                .ok_or(H264BitReaderError::ExpGolombOverflow)?;
            if leading_zero_bits >= 32 {
                return Err(H264BitReaderError::ExpGolombOverflow);
            }
        }

        if leading_zero_bits == 0 {
            return Ok(0);
        }

        let suffix = self.read_bits(leading_zero_bits)?;
        Ok((1_u32 << leading_zero_bits) - 1 + suffix)
    }

    fn read_se(&mut self) -> Result<i32, H264BitReaderError> {
        let unsigned_value = self.read_ue()?;
        let signed_magnitude = unsigned_value.div_ceil(2);
        let signed_magnitude = i32::try_from(signed_magnitude)
            .map_err(|_| H264BitReaderError::SignedExpGolombOverflow)?;

        if unsigned_value % 2 == 0 {
            signed_magnitude
                .checked_neg()
                .ok_or(H264BitReaderError::SignedExpGolombOverflow)
        } else {
            Ok(signed_magnitude)
        }
    }

    fn remaining_bits(&self) -> usize {
        self.rbsp_bytes.len() * 8 - self.bit_offset
    }
}
