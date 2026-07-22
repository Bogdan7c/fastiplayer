use crate::MpegTsDemuxError;

/// Завершённый PES payload с raw transport timestamps.
#[derive(Debug, Clone)]
pub(crate) struct PesPacket {
    /// Elementary PID owner.
    pub(crate) pid: u16,
    /// Optional 33-bit presentation timestamp.
    pub(crate) pts: Option<u64>,
    /// Optional 33-bit decode timestamp.
    pub(crate) dts: Option<u64>,
    /// Elementary-stream bytes после PES optional header.
    pub(crate) payload: Vec<u8>,
    /// Byte offset первого transport packet-а PES.
    pub(crate) byte_offset: u64,
}

/// Per-PID bounded PES accumulator.
#[derive(Debug, Clone)]
pub(crate) struct PesAssembler {
    pid: u16,
    bytes: Vec<u8>,
    byte_offset: u64,
    maximum_bytes: usize,
}

impl PesAssembler {
    /// Создаёт независимый accumulator для elementary PID.
    pub(crate) fn new(pid: u16, maximum_bytes: usize) -> Self {
        Self {
            pid,
            bytes: Vec::new(),
            byte_offset: 0,
            maximum_bytes,
        }
    }

    /// Сбрасывает только affected PES после continuity failure.
    pub(crate) fn reset(&mut self) {
        self.bytes.clear();
    }

    /// Начинает новый PES и возвращает предыдущий, если он был complete enough.
    pub(crate) fn push(
        &mut self,
        payload_unit_start: bool,
        payload: &[u8],
        byte_offset: u64,
    ) -> Result<Option<PesPacket>, MpegTsDemuxError> {
        let completed = if payload_unit_start {
            let completed = self.finish()?;
            self.byte_offset = byte_offset;
            completed
        } else {
            None
        };
        if self.bytes.len().saturating_add(payload.len()) > self.maximum_bytes {
            self.reset();
            return Err(MpegTsDemuxError::PesTooLarge {
                pid: self.pid,
                limit_bytes: self.maximum_bytes,
            });
        }
        self.bytes.extend_from_slice(payload);
        Ok(completed)
    }

    /// Завершает buffered PES на следующем start или EOF.
    pub(crate) fn finish(&mut self) -> Result<Option<PesPacket>, MpegTsDemuxError> {
        if self.bytes.is_empty() {
            return Ok(None);
        }
        let bytes = std::mem::take(&mut self.bytes);
        parse_pes(self.pid, self.byte_offset, bytes).map(Some)
    }
}

fn parse_pes(pid: u16, byte_offset: u64, bytes: Vec<u8>) -> Result<PesPacket, MpegTsDemuxError> {
    if bytes.len() < 9 || bytes[..3] != [0x00, 0x00, 0x01] {
        return Err(malformed(pid, "PES start code/header отсутствует"));
    }
    let declared_packet_length = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    if declared_packet_length != 0 && declared_packet_length + 6 > bytes.len() {
        return Err(malformed(pid, "PES packet_length больше собранных bytes"));
    }
    if bytes[6] & 0xc0 != 0x80 {
        return Err(malformed(pid, "PES marker bits не равны `10`"));
    }
    let timestamp_flags = (bytes[7] >> 6) & 0x03;
    let optional_header_length = usize::from(bytes[8]);
    let payload_start = 9 + optional_header_length;
    if payload_start > bytes.len() {
        return Err(malformed(pid, "PES optional header выходит за packet"));
    }
    let pts = match timestamp_flags {
        0b10 => Some(parse_timestamp(&bytes[9..], 0b0010)?),
        0b11 => Some(parse_timestamp(&bytes[9..], 0b0011)?),
        0b00 => None,
        _ => return Err(malformed(pid, "PES запрещает DTS без PTS")),
    };
    let dts = if timestamp_flags == 0b11 {
        Some(parse_timestamp(&bytes[14..], 0b0001)?)
    } else {
        None
    };
    let payload_end = if declared_packet_length == 0 {
        bytes.len()
    } else {
        (6 + declared_packet_length).min(bytes.len())
    };
    Ok(PesPacket {
        pid,
        pts,
        dts,
        payload: bytes[payload_start..payload_end].to_vec(),
        byte_offset,
    })
}

fn parse_timestamp(bytes: &[u8], expected_prefix: u8) -> Result<u64, MpegTsDemuxError> {
    if bytes.len() < 5 || bytes[0] & 1 == 0 || bytes[2] & 1 == 0 || bytes[4] & 1 == 0 {
        return Err(MpegTsDemuxError::Malformed {
            reason: "оборван PES timestamp или отсутствуют marker bits".to_owned(),
        });
    }
    if bytes[0] >> 4 != expected_prefix {
        return Err(MpegTsDemuxError::Malformed {
            reason: "неверный PES timestamp prefix".to_owned(),
        });
    }
    Ok((u64::from((bytes[0] >> 1) & 0x07) << 30)
        | (u64::from(bytes[1]) << 22)
        | (u64::from(bytes[2] >> 1) << 15)
        | (u64::from(bytes[3]) << 7)
        | u64::from(bytes[4] >> 1))
}

fn malformed(pid: u16, reason: &str) -> MpegTsDemuxError {
    MpegTsDemuxError::Malformed {
        reason: format!("PID {pid}: {reason}"),
    }
}
