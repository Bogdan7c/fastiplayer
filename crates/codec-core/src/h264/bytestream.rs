//! Разбор и преобразование H.264 byte-stream packets без backend-specific state.

use std::fmt;

use super::{
    H264NalLengthSize, H264Packetization, H264PacketizationError,
    parse_avc_decoder_configuration_record, parse_avc3_decoder_configuration_record,
};

const H264_START_CODE_3: &[u8] = &[0x00, 0x00, 0x01];
const H264_START_CODE_4: &[u8] = &[0x00, 0x00, 0x00, 0x01];
const H264_NAL_TYPE_MASK: u8 = 0b0001_1111;
const H264_NAL_REF_IDC_MASK: u8 = 0b0110_0000;
const H264_NAL_FORBIDDEN_ZERO_MASK: u8 = 0b1000_0000;
const H264_NAL_TYPE_IDR_SLICE: u8 = 5;

/// Один H.264 NAL unit без Annex B start code и без AVCC length-prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct H264NalUnit<'a> {
    header: H264NalHeader,
    bytes: &'a [u8],
}

impl<'a> H264NalUnit<'a> {
    /// Возвращает raw NAL bytes вместе с однобайтовым header-ом.
    #[must_use]
    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    /// Возвращает `nal_unit_type`.
    #[must_use]
    pub const fn nal_unit_type(self) -> u8 {
        self.header.nal_unit_type
    }

    /// Возвращает `nal_ref_idc`.
    #[must_use]
    pub const fn nal_ref_idc(self) -> u8 {
        self.header.nal_ref_idc
    }
}

/// Parsed NAL header fields, общие для byte-stream и sibling SPS parser-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct H264NalHeader {
    pub(super) nal_unit_type: u8,
    pub(super) nal_ref_idc: u8,
}

/// Ошибка packet/access-unit framing parser-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H264ByteStreamError {
    /// Packet пустой или не содержит NAL units.
    NoNalUnits,

    /// Annex B packet не содержит start code.
    MissingStartCode,

    /// NAL unit пустой.
    EmptyNalUnit,

    /// Forbidden zero bit в NAL header-е установлен.
    InvalidNalHeader {
        /// Raw header byte.
        header: u8,
    },

    /// AVCC length-prefix оборван.
    TruncatedAvccNalLength {
        /// Размер length-prefix для этого packet-а.
        nal_length_size: H264NalLengthSize,

        /// Сколько байтов осталось в packet-е.
        remaining_bytes: usize,
    },

    /// AVCC NAL unit объявил нулевую длину.
    ZeroLengthAvccNalUnit,

    /// AVCC NAL unit выходит за границы packet-а.
    TruncatedAvccNalUnit {
        /// Объявленная длина NAL unit-а.
        declared_size: usize,

        /// Сколько байтов осталось в packet-е.
        remaining_bytes: usize,
    },

    /// Packetization не удалось доказать.
    Packetization(H264PacketizationError),
}

impl fmt::Display for H264ByteStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoNalUnits => write!(formatter, "H.264 packet contains no NAL units"),
            Self::MissingStartCode => write!(formatter, "Annex B H.264 packet has no start code"),
            Self::EmptyNalUnit => write!(formatter, "H.264 packet contains an empty NAL unit"),
            Self::InvalidNalHeader { header } => {
                write!(formatter, "invalid H.264 NAL header 0x{header:02x}")
            }
            Self::TruncatedAvccNalLength {
                nal_length_size,
                remaining_bytes,
            } => write!(
                formatter,
                "truncated AVCC NAL length: expected {} bytes, remaining {remaining_bytes}",
                nal_length_size.bytes()
            ),
            Self::ZeroLengthAvccNalUnit => {
                write!(
                    formatter,
                    "AVCC H.264 packet contains a zero-length NAL unit"
                )
            }
            Self::TruncatedAvccNalUnit {
                declared_size,
                remaining_bytes,
            } => write!(
                formatter,
                "truncated AVCC NAL unit: declared {declared_size} bytes, remaining {remaining_bytes}"
            ),
            Self::Packetization(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for H264ByteStreamError {}

impl From<H264PacketizationError> for H264ByteStreamError {
    fn from(error: H264PacketizationError) -> Self {
        Self::Packetization(error)
    }
}

/// Политика добавления SPS/PPS при конвертации access unit-а в Annex B.
pub enum H264ParameterSetInjection<'a> {
    /// Не добавлять parameter sets, только перепаковать NAL units.
    None,

    /// Явно поставить SPS/PPS перед access unit-ом.
    ///
    /// Эта политика нужна для backend-ов, которые принимают Annex B stream и
    /// ожидают parameter sets рядом с IDR/decode-start packet-ом. Adapter не
    /// решает сам, когда инжектить SPS/PPS: caller выбирает lifecycle policy.
    BeforeAccessUnit {
        /// SPS units без start code.
        sequence_parameter_sets: &'a [Vec<u8>],

        /// PPS units без start code.
        picture_parameter_sets: &'a [Vec<u8>],
    },
}

/// Выводит H.264 packetization из codec private и/или packet bytes.
pub fn infer_h264_packetization(
    codec_private: Option<&[u8]>,
    packet_bytes: &[u8],
) -> Result<H264Packetization, H264ByteStreamError> {
    if let Some(codec_private) = codec_private.filter(|bytes| !bytes.is_empty()) {
        if let Ok(record) = parse_avc_decoder_configuration_record(codec_private) {
            return Ok(record.packetization());
        }
        if let Ok(record) = parse_avc3_decoder_configuration_record(codec_private) {
            return Ok(H264Packetization::from_avc3_decoder_configuration_record(
                &record,
            ));
        }
    }

    if contains_annex_b_start_code(packet_bytes) {
        return Ok(H264Packetization::AnnexB);
    }

    Err(H264PacketizationError::UnknownPacketization.into())
}

/// Возвращает NAL units access unit-а через явно указанную packetization model.
pub fn h264_nal_units<'a>(
    packet_bytes: &'a [u8],
    packetization: H264Packetization,
) -> Result<Vec<H264NalUnit<'a>>, H264ByteStreamError> {
    match packetization {
        H264Packetization::AnnexB => annex_b_nal_units(packet_bytes),
        H264Packetization::AvccLengthPrefixed { nal_length_size }
        | H264Packetization::AvccLengthPrefixedWithInBandParameterSets { nal_length_size } => {
            avcc_nal_units(packet_bytes, nal_length_size)
        }
    }
}

/// Конвертирует H.264 access unit в Annex B и применяет caller-owned SPS/PPS injection policy.
pub fn h264_access_unit_to_annex_b(
    packet_bytes: &[u8],
    packetization: H264Packetization,
    parameter_set_injection: H264ParameterSetInjection<'_>,
) -> Result<Vec<u8>, H264ByteStreamError> {
    let mut annex_b_bytes = Vec::new();
    h264_access_unit_to_annex_b_into(
        packet_bytes,
        packetization,
        parameter_set_injection,
        &mut annex_b_bytes,
    )?;
    Ok(annex_b_bytes)
}

/// Конвертирует H.264 access unit в caller-owned Annex B buffer без новой output allocation.
///
/// Функция очищает `output`, сохраняет его capacity и оставляет его пустым при ошибке.
pub fn h264_access_unit_to_annex_b_into(
    packet_bytes: &[u8],
    packetization: H264Packetization,
    parameter_set_injection: H264ParameterSetInjection<'_>,
    output: &mut Vec<u8>,
) -> Result<(), H264ByteStreamError> {
    output.clear();

    let write_result = {
        if let H264ParameterSetInjection::BeforeAccessUnit {
            sequence_parameter_sets,
            picture_parameter_sets,
        } = parameter_set_injection
        {
            append_parameter_sets(output, sequence_parameter_sets);
            append_parameter_sets(output, picture_parameter_sets);
        }

        append_access_unit_nals_to_annex_b(packet_bytes, packetization, output)
    };

    if write_result.is_err() {
        // Частично записанный Annex B packet нельзя передавать decoder-у как валидный output.
        output.clear();
    }

    write_result
}

/// Возвращает `true` только для access unit-а с IDR slice.
pub fn probe_h264_packet_keyframe(
    packet_bytes: &[u8],
    packetization: H264Packetization,
) -> Result<bool, H264ByteStreamError> {
    let nal_units = h264_nal_units(packet_bytes, packetization)?;

    for nal_unit in nal_units {
        if nal_unit.nal_unit_type() == H264_NAL_TYPE_IDR_SLICE {
            return Ok(true);
        }
    }

    Ok(false)
}

fn annex_b_nal_units(packet_bytes: &[u8]) -> Result<Vec<H264NalUnit<'_>>, H264ByteStreamError> {
    let Some((first_start_code_offset, first_start_code_size)) =
        find_annex_b_start_code(packet_bytes, 0)
    else {
        return Err(H264ByteStreamError::MissingStartCode);
    };

    let mut nal_units = Vec::new();
    let mut nal_start = first_start_code_offset + first_start_code_size;

    while nal_start < packet_bytes.len() {
        let next_start_code = find_annex_b_start_code(packet_bytes, nal_start);
        let nal_end = next_start_code
            .map(|(start_code_offset, _)| start_code_offset)
            .unwrap_or(packet_bytes.len());

        if nal_start < nal_end {
            nal_units.push(parse_nal_unit(&packet_bytes[nal_start..nal_end])?);
        }

        let Some((next_start_code_offset, next_start_code_size)) = next_start_code else {
            break;
        };
        nal_start = next_start_code_offset + next_start_code_size;
    }

    if nal_units.is_empty() {
        return Err(H264ByteStreamError::NoNalUnits);
    }

    Ok(nal_units)
}

fn avcc_nal_units(
    packet_bytes: &[u8],
    nal_length_size: H264NalLengthSize,
) -> Result<Vec<H264NalUnit<'_>>, H264ByteStreamError> {
    let mut cursor = 0;
    let mut nal_units = Vec::new();

    while cursor < packet_bytes.len() {
        let remaining_bytes = packet_bytes.len() - cursor;
        if remaining_bytes < nal_length_size.bytes() {
            return Err(H264ByteStreamError::TruncatedAvccNalLength {
                nal_length_size,
                remaining_bytes,
            });
        }

        let declared_size = read_avcc_nal_size(packet_bytes, cursor, nal_length_size);
        cursor += nal_length_size.bytes();
        if declared_size == 0 {
            return Err(H264ByteStreamError::ZeroLengthAvccNalUnit);
        }

        let remaining_bytes = packet_bytes.len() - cursor;
        if remaining_bytes < declared_size {
            return Err(H264ByteStreamError::TruncatedAvccNalUnit {
                declared_size,
                remaining_bytes,
            });
        }

        nal_units.push(parse_nal_unit(
            &packet_bytes[cursor..cursor + declared_size],
        )?);
        cursor += declared_size;
    }

    if nal_units.is_empty() {
        return Err(H264ByteStreamError::NoNalUnits);
    }

    Ok(nal_units)
}

fn append_access_unit_nals_to_annex_b(
    packet_bytes: &[u8],
    packetization: H264Packetization,
    output: &mut Vec<u8>,
) -> Result<(), H264ByteStreamError> {
    match packetization {
        H264Packetization::AnnexB => append_annex_b_nal_units_to_annex_b(packet_bytes, output),
        H264Packetization::AvccLengthPrefixed { nal_length_size }
        | H264Packetization::AvccLengthPrefixedWithInBandParameterSets { nal_length_size } => {
            append_avcc_nal_units_to_annex_b(packet_bytes, nal_length_size, output)
        }
    }
}

fn append_annex_b_nal_units_to_annex_b(
    packet_bytes: &[u8],
    output: &mut Vec<u8>,
) -> Result<(), H264ByteStreamError> {
    let Some((first_start_code_offset, first_start_code_size)) =
        find_annex_b_start_code(packet_bytes, 0)
    else {
        return Err(H264ByteStreamError::MissingStartCode);
    };

    let mut nal_units_written = 0usize;
    let mut nal_start = first_start_code_offset + first_start_code_size;

    while nal_start < packet_bytes.len() {
        let next_start_code = find_annex_b_start_code(packet_bytes, nal_start);
        let nal_end = next_start_code
            .map(|(start_code_offset, _)| start_code_offset)
            .unwrap_or(packet_bytes.len());

        if nal_start < nal_end {
            append_nal_unit_to_annex_b(&packet_bytes[nal_start..nal_end], output)?;
            nal_units_written += 1;
        }

        let Some((next_start_code_offset, next_start_code_size)) = next_start_code else {
            break;
        };
        nal_start = next_start_code_offset + next_start_code_size;
    }

    if nal_units_written == 0 {
        return Err(H264ByteStreamError::NoNalUnits);
    }

    Ok(())
}

fn append_avcc_nal_units_to_annex_b(
    packet_bytes: &[u8],
    nal_length_size: H264NalLengthSize,
    output: &mut Vec<u8>,
) -> Result<(), H264ByteStreamError> {
    let mut cursor = 0;
    let mut nal_units_written = 0usize;

    while cursor < packet_bytes.len() {
        let remaining_bytes = packet_bytes.len() - cursor;
        if remaining_bytes < nal_length_size.bytes() {
            return Err(H264ByteStreamError::TruncatedAvccNalLength {
                nal_length_size,
                remaining_bytes,
            });
        }

        let declared_size = read_avcc_nal_size(packet_bytes, cursor, nal_length_size);
        cursor += nal_length_size.bytes();
        if declared_size == 0 {
            return Err(H264ByteStreamError::ZeroLengthAvccNalUnit);
        }

        let remaining_bytes = packet_bytes.len() - cursor;
        if remaining_bytes < declared_size {
            return Err(H264ByteStreamError::TruncatedAvccNalUnit {
                declared_size,
                remaining_bytes,
            });
        }

        append_nal_unit_to_annex_b(&packet_bytes[cursor..cursor + declared_size], output)?;
        cursor += declared_size;
        nal_units_written += 1;
    }

    if nal_units_written == 0 {
        return Err(H264ByteStreamError::NoNalUnits);
    }

    Ok(())
}

fn append_nal_unit_to_annex_b(
    nal_unit_bytes: &[u8],
    output: &mut Vec<u8>,
) -> Result<(), H264ByteStreamError> {
    let nal_unit = parse_nal_unit(nal_unit_bytes)?;
    output.extend_from_slice(H264_START_CODE_4);
    output.extend_from_slice(nal_unit.bytes());
    Ok(())
}

fn read_avcc_nal_size(
    packet_bytes: &[u8],
    cursor: usize,
    nal_length_size: H264NalLengthSize,
) -> usize {
    match nal_length_size.get() {
        1 => usize::from(packet_bytes[cursor]),
        2 => usize::from(u16::from_be_bytes([
            packet_bytes[cursor],
            packet_bytes[cursor + 1],
        ])),
        4 => u32::from_be_bytes([
            packet_bytes[cursor],
            packet_bytes[cursor + 1],
            packet_bytes[cursor + 2],
            packet_bytes[cursor + 3],
        ]) as usize,
        _ => unreachable!("H264NalLengthSize содержит только 1/2/4"),
    }
}

fn parse_nal_unit(nal_unit_bytes: &[u8]) -> Result<H264NalUnit<'_>, H264ByteStreamError> {
    Ok(H264NalUnit {
        header: parse_nal_header(nal_unit_bytes)?,
        bytes: nal_unit_bytes,
    })
}

pub(super) fn parse_nal_header(
    nal_unit_bytes: &[u8],
) -> Result<H264NalHeader, H264ByteStreamError> {
    let Some(&header) = nal_unit_bytes.first() else {
        return Err(H264ByteStreamError::EmptyNalUnit);
    };

    if header & H264_NAL_FORBIDDEN_ZERO_MASK != 0 {
        return Err(H264ByteStreamError::InvalidNalHeader { header });
    }

    Ok(H264NalHeader {
        nal_unit_type: header & H264_NAL_TYPE_MASK,
        nal_ref_idc: (header & H264_NAL_REF_IDC_MASK) >> 5,
    })
}

fn find_annex_b_start_code(packet_bytes: &[u8], from_offset: usize) -> Option<(usize, usize)> {
    let mut offset = from_offset;
    while offset + H264_START_CODE_3.len() <= packet_bytes.len() {
        if packet_bytes[offset..].starts_with(H264_START_CODE_4) {
            return Some((offset, H264_START_CODE_4.len()));
        }
        if packet_bytes[offset..].starts_with(H264_START_CODE_3) {
            return Some((offset, H264_START_CODE_3.len()));
        }
        offset += 1;
    }

    None
}

fn contains_annex_b_start_code(packet_bytes: &[u8]) -> bool {
    find_annex_b_start_code(packet_bytes, 0).is_some()
}

fn append_parameter_sets(output: &mut Vec<u8>, parameter_sets: &[Vec<u8>]) {
    for parameter_set in parameter_sets {
        output.extend_from_slice(H264_START_CODE_4);
        output.extend_from_slice(parameter_set);
    }
}
