//! Минимальный VP8 uncompressed-header probe для container keyframe recovery.
//!
//! Модуль не декодирует bool-coded partitions: он проверяет только поля,
//! которые физически находятся в uncompressed frame header и достаточны для
//! безопасного определения random-access frame.

use thiserror::Error;

/// Трёхбайтовый frame tag присутствует у каждого VP8 frame.
const VP8_FRAME_TAG_SIZE: usize = 3;
/// Keyframe добавляет sync code и два 16-bit dimension fields.
const VP8_KEYFRAME_HEADER_SIZE: usize = 10;
/// Нормативный VP8 keyframe sync code.
const VP8_KEYFRAME_SYNC_CODE: [u8; 3] = [0x9d, 0x01, 0x2a];

/// Ошибки bounded VP8 uncompressed-header probe.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Vp8PacketHeaderError {
    /// Packet короче обязательного frame tag-а.
    #[error("VP8 frame tag truncated: expected at least {required} bytes, got {actual}")]
    TruncatedFrameTag {
        /// Минимальная длина frame tag-а.
        required: usize,
        /// Фактическая длина packet-а.
        actual: usize,
    },
    /// Версия bitstream зарезервирована VP8 specification.
    #[error("VP8 frame version {version} is reserved")]
    ReservedVersion {
        /// Трёхбитное значение version из frame tag-а.
        version: u8,
    },
    /// First partition выходит за границы packet-а.
    #[error(
        "VP8 first partition truncated: frame tag declares {declared} bytes, only {available} remain"
    )]
    TruncatedFirstPartition {
        /// Длина first partition из frame tag-а.
        declared: usize,
        /// Доступные после frame tag-а bytes.
        available: usize,
    },
    /// Keyframe короче обязательного uncompressed header-а.
    #[error("VP8 keyframe header truncated: expected at least {required} bytes, got {actual}")]
    TruncatedKeyframeHeader {
        /// Минимальная длина keyframe header-а.
        required: usize,
        /// Фактическая длина packet-а.
        actual: usize,
    },
    /// Keyframe не содержит нормативный sync code.
    #[error("VP8 keyframe sync code is invalid")]
    InvalidKeyframeSyncCode,
    /// Keyframe объявляет нулевую coded dimension.
    #[error("VP8 keyframe dimensions must be non-zero, got {width}x{height}")]
    ZeroKeyframeDimensions {
        /// Coded width без scale bits.
        width: u16,
        /// Coded height без scale bits.
        height: u16,
    },
}

/// Возвращает `true` для VP8 keyframe после структурной проверки header-а.
pub fn probe_vp8_packet_keyframe(packet_bytes: &[u8]) -> Result<bool, Vp8PacketHeaderError> {
    if packet_bytes.len() < VP8_FRAME_TAG_SIZE {
        return Err(Vp8PacketHeaderError::TruncatedFrameTag {
            required: VP8_FRAME_TAG_SIZE,
            actual: packet_bytes.len(),
        });
    }

    let frame_tag = u32::from(packet_bytes[0])
        | (u32::from(packet_bytes[1]) << 8)
        | (u32::from(packet_bytes[2]) << 16);
    let keyframe = frame_tag & 1 == 0;
    let version = ((frame_tag >> 1) & 0x07) as u8;
    if version > 3 {
        return Err(Vp8PacketHeaderError::ReservedVersion { version });
    }

    let first_partition_size = (frame_tag >> 5) as usize;
    let available_partition_bytes = packet_bytes.len() - VP8_FRAME_TAG_SIZE;
    if first_partition_size > available_partition_bytes {
        return Err(Vp8PacketHeaderError::TruncatedFirstPartition {
            declared: first_partition_size,
            available: available_partition_bytes,
        });
    }

    if !keyframe {
        return Ok(false);
    }
    if packet_bytes.len() < VP8_KEYFRAME_HEADER_SIZE {
        return Err(Vp8PacketHeaderError::TruncatedKeyframeHeader {
            required: VP8_KEYFRAME_HEADER_SIZE,
            actual: packet_bytes.len(),
        });
    }
    if packet_bytes[3..6] != VP8_KEYFRAME_SYNC_CODE {
        return Err(Vp8PacketHeaderError::InvalidKeyframeSyncCode);
    }

    let width_field = u16::from_le_bytes([packet_bytes[6], packet_bytes[7]]);
    let height_field = u16::from_le_bytes([packet_bytes[8], packet_bytes[9]]);
    let width = width_field & 0x3fff;
    let height = height_field & 0x3fff;
    if width == 0 || height == 0 {
        return Err(Vp8PacketHeaderError::ZeroKeyframeDimensions { width, height });
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{VP8_KEYFRAME_SYNC_CODE, Vp8PacketHeaderError, probe_vp8_packet_keyframe};

    /// Собирает structurally valid VP8 packet с заданным frame type.
    fn vp8_packet(keyframe: bool) -> Vec<u8> {
        let first_partition_size = if keyframe { 7_u32 } else { 1_u32 };
        let frame_tag = u32::from(!keyframe) | (first_partition_size << 5);
        let mut packet = vec![
            frame_tag as u8,
            (frame_tag >> 8) as u8,
            (frame_tag >> 16) as u8,
        ];
        if keyframe {
            packet.extend_from_slice(&VP8_KEYFRAME_SYNC_CODE);
            packet.extend_from_slice(&320_u16.to_le_bytes());
            packet.extend_from_slice(&180_u16.to_le_bytes());
        } else {
            packet.push(0);
        }
        packet
    }

    /// Keyframe определяется только после sync-code/dimension validation.
    #[test]
    fn valid_keyframe_is_reported() {
        assert_eq!(probe_vp8_packet_keyframe(&vp8_packet(true)), Ok(true));
    }

    /// Interframe не требует keyframe-only sync code.
    #[test]
    fn valid_interframe_is_reported() {
        assert_eq!(probe_vp8_packet_keyframe(&vp8_packet(false)), Ok(false));
    }

    /// Объявленная first partition не может выходить за packet boundary.
    #[test]
    fn truncated_partition_is_typed() {
        let packet = [0x40, 0x01, 0x00];
        assert_eq!(
            probe_vp8_packet_keyframe(&packet),
            Err(Vp8PacketHeaderError::TruncatedFirstPartition {
                declared: 10,
                available: 0,
            })
        );
    }

    /// Повреждённый keyframe sync code не превращается в false interframe.
    #[test]
    fn invalid_keyframe_sync_code_is_typed() {
        let mut packet = vp8_packet(true);
        packet[3] = 0;
        assert_eq!(
            probe_vp8_packet_keyframe(&packet),
            Err(Vp8PacketHeaderError::InvalidKeyframeSyncCode)
        );
    }
}
