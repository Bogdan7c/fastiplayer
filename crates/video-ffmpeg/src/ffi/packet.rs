//! Safe packet input helpers for future `AVPacket` ownership.

/// FFmpeg требует zero padding после compressed input buffer-а.
///
/// Значение соответствует публичному `AV_INPUT_BUFFER_PADDING_SIZE` FFmpeg.
/// Реальный `AVPacket` wrapper в следующих сессиях будет брать ownership уже
/// здесь, внутри `ffi`, а не в decoder thread или `player-core`.
pub const INPUT_BUFFER_PADDING_BYTES: usize = 64;

/// Encoded payload вместе с padding, который безопасен для FFmpeg bitstream readers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaddedPacketBytes {
    /// Payload плюс trailing zero padding.
    padded_bytes: Vec<u8>,

    /// Длина настоящего compressed payload-а без padding.
    payload_len: usize,
}

impl PaddedPacketBytes {
    /// Копирует compressed payload и добавляет FFmpeg-required zero padding.
    #[must_use]
    pub fn new(encoded_payload: impl AsRef<[u8]>) -> Self {
        let encoded_payload = encoded_payload.as_ref();
        let payload_len = encoded_payload.len();
        let mut padded_bytes = Vec::with_capacity(payload_len + INPUT_BUFFER_PADDING_BYTES);

        padded_bytes.extend_from_slice(encoded_payload);
        padded_bytes.resize(payload_len + INPUT_BUFFER_PADDING_BYTES, 0);

        Self {
            padded_bytes,
            payload_len,
        }
    }

    /// Возвращает compressed payload без trailing padding.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.padded_bytes[..self.payload_len]
    }

    /// Возвращает payload плюс padding для будущего `av_new_packet`/`av_packet_from_data`.
    #[must_use]
    pub fn padded_bytes(&self) -> &[u8] {
        &self.padded_bytes
    }

    /// Возвращает длину compressed payload-а без padding.
    #[must_use]
    pub const fn payload_len(&self) -> usize {
        self.payload_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_packet_keeps_payload_and_zero_padding_separate() {
        let packet_bytes = PaddedPacketBytes::new([1_u8, 2, 3]);

        assert_eq!(packet_bytes.payload(), &[1, 2, 3]);
        assert_eq!(packet_bytes.payload_len(), 3);
        assert_eq!(
            packet_bytes.padded_bytes().len(),
            3 + INPUT_BUFFER_PADDING_BYTES
        );
        assert!(
            packet_bytes.padded_bytes()[3..]
                .iter()
                .all(|padding_byte| *padding_byte == 0)
        );
    }
}
