//! AV1 codec-private metadata и OBU packet probing.
//!
//! Fixed header `av1C` даёт container-level profile/bit-depth/chroma до decode.
//! AV1 в ISOBMFF/low-overhead bitstream format: temporal unit состоит из OBU-ов,
//! у которых `obu_has_size_field = 1`. Config OBUs (sequence header) лежат в
//! `av1C` codec-private, а sync-сэмплы (keyframes) дополнительно несут sequence
//! header in-band. Software decoder (libdav1d) НЕ получает sequence header из
//! `extradata` для самого декодирования — он нужен in-band, поэтому decode
//! обязан стартовать с KEY_FRAME. Без packet-level keyframe probe AV1 packets
//! остаются `Unknown`, и player после flush/backend-swap ошибочно принимает
//! inter-frame как decode start, отдавая декодеру кадр без sequence header
//! (`libdav1d: Error parsing OBU data`).

use core::fmt;

mod configuration;

pub use configuration::{
    Av1DecoderConfigurationRecordError, av1_decode_requirement_from_decoder_configuration_record,
};

/// OBU type codes (AV1 spec §6.2.2).
const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_FRAME_HEADER: u8 = 3;
const OBU_FRAME: u8 = 6;

/// `frame_type` code KEY_FRAME (AV1 spec §6.8.2): сбрасывает decode state и
/// служит random-access точкой.
const FRAME_TYPE_KEY_FRAME: u32 = 0;

/// Ошибки разбора AV1 OBU-потока при keyframe probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Av1ObuError {
    /// Буфер закончился посреди OBU header/payload.
    Truncated,
    /// `leb128` size превышает разумную длину или не завершён.
    InvalidLeb128,
    /// Установлен `obu_forbidden_bit` — поток не является валидным OBU stream.
    ForbiddenBit,
    /// В temporal unit нет frame/frame-header OBU, по которому судят keyframe.
    NoFrameObu,
}

impl fmt::Display for Av1ObuError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(formatter, "AV1 OBU stream truncated"),
            Self::InvalidLeb128 => write!(formatter, "AV1 OBU has invalid leb128 size"),
            Self::ForbiddenBit => write!(formatter, "AV1 OBU forbidden bit is set"),
            Self::NoFrameObu => {
                write!(formatter, "AV1 temporal unit has no frame/frame-header OBU")
            }
        }
    }
}

impl std::error::Error for Av1ObuError {}

/// MSB-first bit reader без emulation prevention (AV1 его не использует).
struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    fn read_bit(&mut self) -> Result<u32, Av1ObuError> {
        let byte_index = self.bit_pos / 8;
        if byte_index >= self.data.len() {
            return Err(Av1ObuError::Truncated);
        }
        let bit_offset = 7 - (self.bit_pos % 8);
        let bit = (self.data[byte_index] >> bit_offset) & 1;
        self.bit_pos += 1;
        Ok(u32::from(bit))
    }

    fn read_bits(&mut self, count: u32) -> Result<u32, Av1ObuError> {
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | self.read_bit()?;
        }
        Ok(value)
    }
}

/// Один OBU из low-overhead bitstream-а: тип и payload (без header/size).
struct Obu<'a> {
    obu_type: u8,
    payload: &'a [u8],
}

/// Итератор по OBU-ам в low-overhead bitstream format (`obu_has_size_field = 1`).
struct ObuIter<'a> {
    data: &'a [u8],
}

impl<'a> ObuIter<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn next_obu(&mut self) -> Result<Option<Obu<'a>>, Av1ObuError> {
        if self.data.is_empty() {
            return Ok(None);
        }

        let header = self.data[0];
        if (header & 0x80) != 0 {
            return Err(Av1ObuError::ForbiddenBit);
        }
        let obu_type = (header >> 3) & 0x0f;
        let extension_flag = (header >> 2) & 1;
        let has_size_field = (header >> 1) & 1;

        let mut offset = 1usize;
        if extension_flag == 1 {
            // extension header — ещё один байт (temporal_id/spatial_id/reserved).
            offset = offset.checked_add(1).ok_or(Av1ObuError::Truncated)?;
            if offset > self.data.len() {
                return Err(Av1ObuError::Truncated);
            }
        }

        let obu_size = if has_size_field == 1 {
            let (size, consumed) = read_leb128(&self.data[offset..])?;
            offset = offset.checked_add(consumed).ok_or(Av1ObuError::Truncated)?;
            size
        } else {
            // Без size field OBU занимает остаток буфера (для MP4 не встречается,
            // но соблюдаем спецификацию).
            self.data.len().saturating_sub(offset)
        };

        let payload_end = offset.checked_add(obu_size).ok_or(Av1ObuError::Truncated)?;
        if payload_end > self.data.len() {
            return Err(Av1ObuError::Truncated);
        }

        let payload = &self.data[offset..payload_end];
        self.data = &self.data[payload_end..];

        Ok(Some(Obu { obu_type, payload }))
    }
}

/// Читает `leb128` (AV1 spec §4.10.5), возвращает значение и число байт.
fn read_leb128(data: &[u8]) -> Result<(usize, usize), Av1ObuError> {
    let mut value: u64 = 0;
    for index in 0..8 {
        let byte = *data.get(index).ok_or(Av1ObuError::Truncated)?;
        value |= u64::from(byte & 0x7f) << (index * 7);
        if (byte & 0x80) == 0 {
            let value = usize::try_from(value).map_err(|_| Av1ObuError::InvalidLeb128)?;
            return Ok((value, index + 1));
        }
    }
    Err(Av1ObuError::InvalidLeb128)
}

/// Читает `reduced_still_picture_header` из payload sequence header OBU.
///
/// Layout (AV1 spec §5.5.1): `seq_profile f(3)`, `still_picture f(1)`,
/// `reduced_still_picture_header f(1)`.
fn reduced_still_picture_header(seq_header_payload: &[u8]) -> Result<bool, Av1ObuError> {
    let mut reader = BitReader::new(seq_header_payload);
    let _seq_profile = reader.read_bits(3)?;
    let _still_picture = reader.read_bit()?;
    Ok(reader.read_bit()? == 1)
}

/// Baseline `reduced_still_picture_header` из `av1C` codec-private (config OBUs
/// начинаются после 4-байтового AV1CodecConfigurationRecord header-а).
fn reduced_from_config_record(codec_private: &[u8]) -> Option<bool> {
    if codec_private.len() < 4 || (codec_private[0] & 0x80) == 0 {
        return None;
    }
    let mut iter = ObuIter::new(&codec_private[4..]);
    while let Ok(Some(obu)) = iter.next_obu() {
        if obu.obu_type == OBU_SEQUENCE_HEADER {
            return reduced_still_picture_header(obu.payload).ok();
        }
    }
    None
}

/// Определяет, является ли frame header OBU началом KEY_FRAME.
///
/// `uncompressed_header()` (AV1 spec §5.9.2) при не-`reduced_still_picture_header`
/// начинается с `show_existing_frame f(1)`; при `show_existing_frame == 1` это
/// показ ранее декодированного кадра (не decode start), иначе `frame_type f(2)`.
fn frame_header_is_keyframe(payload: &[u8], reduced: bool) -> Result<bool, Av1ObuError> {
    if reduced {
        // reduced_still_picture_header ⇒ frame_type = KEY_FRAME безусловно.
        return Ok(true);
    }
    let mut reader = BitReader::new(payload);
    let show_existing_frame = reader.read_bit()?;
    if show_existing_frame == 1 {
        return Ok(false);
    }
    let frame_type = reader.read_bits(2)?;
    Ok(frame_type == FRAME_TYPE_KEY_FRAME)
}

/// Определяет, начинается ли AV1 temporal unit (packet) с KEY_FRAME.
///
/// Возвращает `Ok(true)` для decode-start keyframe, `Ok(false)` для inter/show-
/// existing кадров и `Err` при некорректном/неполном OBU-потоке (caller трактует
/// это как неопределённость, а не как keyframe).
pub fn probe_av1_packet_keyframe(
    packet: &[u8],
    codec_private: Option<&[u8]>,
) -> Result<bool, Av1ObuError> {
    let mut reduced = codec_private
        .and_then(reduced_from_config_record)
        .unwrap_or(false);

    let mut iter = ObuIter::new(packet);
    while let Some(obu) = iter.next_obu()? {
        match obu.obu_type {
            OBU_SEQUENCE_HEADER => {
                if let Ok(value) = reduced_still_picture_header(obu.payload) {
                    reduced = value;
                }
            }
            OBU_FRAME | OBU_FRAME_HEADER => {
                return frame_header_is_keyframe(obu.payload, reduced);
            }
            _ => {}
        }
    }

    Err(Av1ObuError::NoFrameObu)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Кодирует OBU в low-overhead формате (obu_has_size_field = 1, без extension).
    fn obu(obu_type: u8, payload: &[u8]) -> Vec<u8> {
        let header = (obu_type << 3) | 0b0000_0010;
        let mut bytes = vec![header];
        // leb128 size (payload короткий — один байт достаточно для тестов).
        assert!(payload.len() < 0x80);
        bytes.push(payload.len() as u8);
        bytes.extend_from_slice(payload);
        bytes
    }

    /// Sequence header payload с заданным reduced_still_picture_header.
    fn seq_header_payload(reduced: bool) -> Vec<u8> {
        // seq_profile(3)=0, still_picture(1)=0, reduced(1) на MSB-индексе 4, padding.
        let reduced_bit = if reduced { 0b0000_1000 } else { 0 };
        vec![reduced_bit, 0, 0]
    }

    /// Frame payload, где первые биты задают show_existing_frame и frame_type.
    fn frame_payload(show_existing: bool, frame_type: u8) -> Vec<u8> {
        // show_existing_frame(1) | frame_type(2) | остаток.
        let mut first = 0u8;
        if show_existing {
            first |= 0b1000_0000;
        } else {
            first |= (frame_type & 0b11) << 5;
        }
        vec![first, 0, 0]
    }

    #[test]
    fn seq_header_plus_key_frame_is_keyframe() {
        let mut packet = obu(OBU_SEQUENCE_HEADER, &seq_header_payload(false));
        packet.extend(obu(
            OBU_FRAME,
            &frame_payload(false, FRAME_TYPE_KEY_FRAME as u8),
        ));
        assert_eq!(probe_av1_packet_keyframe(&packet, None), Ok(true));
    }

    #[test]
    fn inter_frame_without_seq_header_is_not_keyframe() {
        let packet = obu(OBU_FRAME, &frame_payload(false, 1));
        assert_eq!(probe_av1_packet_keyframe(&packet, None), Ok(false));
    }

    #[test]
    fn show_existing_frame_is_not_keyframe() {
        let packet = obu(OBU_FRAME_HEADER, &frame_payload(true, 0));
        assert_eq!(probe_av1_packet_keyframe(&packet, None), Ok(false));
    }

    #[test]
    fn intra_only_frame_is_not_treated_as_keyframe() {
        // frame_type = INTRA_ONLY_FRAME (2) — не полный random-access KEY_FRAME.
        let packet = obu(OBU_FRAME, &frame_payload(false, 2));
        assert_eq!(probe_av1_packet_keyframe(&packet, None), Ok(false));
    }

    #[test]
    fn temporal_delimiter_is_skipped_before_frame() {
        let mut packet = obu(2 /* OBU_TEMPORAL_DELIMITER */, &[]);
        packet.extend(obu(OBU_SEQUENCE_HEADER, &seq_header_payload(false)));
        packet.extend(obu(
            OBU_FRAME,
            &frame_payload(false, FRAME_TYPE_KEY_FRAME as u8),
        ));
        assert_eq!(probe_av1_packet_keyframe(&packet, None), Ok(true));
    }

    #[test]
    fn reduced_still_picture_from_config_record_forces_keyframe() {
        // av1C: 4-байтовый header (0x81, ...), затем sequence header OBU c reduced=1.
        let mut codec_private = vec![0x81, 0x00, 0x00, 0x00];
        codec_private.extend(obu(OBU_SEQUENCE_HEADER, &seq_header_payload(true)));
        // Packet несёт только frame OBU без in-band seq header.
        let packet = obu(OBU_FRAME, &frame_payload(false, 1));
        assert_eq!(
            probe_av1_packet_keyframe(&packet, Some(&codec_private)),
            Ok(true)
        );
    }

    #[test]
    fn forbidden_bit_is_error() {
        let packet = vec![0x80, 0x01, 0x00];
        assert_eq!(
            probe_av1_packet_keyframe(&packet, None),
            Err(Av1ObuError::ForbiddenBit)
        );
    }

    #[test]
    fn packet_without_frame_obu_is_error() {
        let packet = obu(OBU_SEQUENCE_HEADER, &seq_header_payload(false));
        assert_eq!(
            probe_av1_packet_keyframe(&packet, None),
            Err(Av1ObuError::NoFrameObu)
        );
    }

    #[test]
    fn truncated_obu_is_error() {
        // header заявляет size=10, но payload отсутствует.
        let packet = vec![(OBU_FRAME << 3) | 0b10, 0x0a];
        assert_eq!(
            probe_av1_packet_keyframe(&packet, None),
            Err(Av1ObuError::Truncated)
        );
    }
}
