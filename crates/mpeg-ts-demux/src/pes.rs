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

/// PES, прерванный разрывом `continuity_counter` перед новым payload_unit_start.
#[derive(Debug)]
pub(crate) enum InterruptedPes {
    /// Закрывать нечего.
    Absent,
    /// Объявленный `PES_packet_length` набран: PES полный.
    Complete(PesPacket),
    /// Длина не объявлена (так пишут видео): потерянные пакеты могли быть его
    /// хвостом, полноту по самому PES не проверить.
    LengthNotDeclared(PesPacket),
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
        } else if self.bytes.is_empty() {
            // Продолжение PES, начало которого не было прочитано: старт потока
            // посреди PES, seek на PCR/keyframe anchor внутри чужого PES или
            // сброс PID-а после continuity gap. Без PES header-а эти байты
            // нельзя ни разобрать, ни привязать к PTS; раньше они копились и
            // следующий payload_unit_start ронял поток ошибкой «start code absent».
            // Сборка начинается со следующего payload_unit_start.
            return Ok(None);
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

    /// Закрывает PES, после которого continuity counter скакнул на новом
    /// payload_unit_start. PES с объявленной длиной, которой не хватило,
    /// возвращается typed `Malformed`, как и при обычном закрытии.
    pub(crate) fn close_after_continuity_gap(
        &mut self,
    ) -> Result<InterruptedPes, MpegTsDemuxError> {
        let length_declared = self.bytes.get(4..6).is_some_and(|length| length != [0, 0]);
        Ok(match self.finish()? {
            None => InterruptedPes::Absent,
            Some(pes) if length_declared => InterruptedPes::Complete(pes),
            Some(pes) => InterruptedPes::LengthNotDeclared(pes),
        })
    }
}

/// PTS из заголовка PES, начинающегося в `payload` (пакет с payload_unit_start),
/// без разбора остального; `None`, если заголовок неполон или PTS нет.
pub(crate) fn peek_pes_pts(payload: &[u8]) -> Option<u64> {
    if payload.len() < 14 || payload[..3] != [0x00, 0x00, 0x01] {
        return None;
    }
    let timestamp_flags = (payload[7] >> 6) & 0x03;
    let expected_prefix = match timestamp_flags {
        0b10 => 0b0010,
        0b11 => 0b0011,
        _ => return None,
    };
    parse_timestamp(&payload[9..], expected_prefix).ok()
}

fn parse_pes(pid: u16, byte_offset: u64, bytes: Vec<u8>) -> Result<PesPacket, MpegTsDemuxError> {
    if bytes.len() < 9 || bytes[..3] != [0x00, 0x00, 0x01] {
        return Err(malformed(pid, "PES start code/header absent"));
    }
    let declared_packet_length = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    if declared_packet_length != 0 && declared_packet_length + 6 > bytes.len() {
        return Err(malformed(pid, "PES packet_length more collected bytes"));
    }
    if bytes[6] & 0xc0 != 0x80 {
        return Err(malformed(pid, "PES marker bits are not equal to '10'"));
    }
    let timestamp_flags = (bytes[7] >> 6) & 0x03;
    let optional_header_length = usize::from(bytes[8]);
    let payload_start = 9 + optional_header_length;
    if payload_start > bytes.len() {
        return Err(malformed(
            pid,
            "PES optional header extends beyond the packet",
        ));
    }
    let pts = match timestamp_flags {
        0b10 => Some(parse_timestamp(&bytes[9..], 0b0010)?),
        0b11 => Some(parse_timestamp(&bytes[9..], 0b0011)?),
        0b00 => None,
        _ => return Err(malformed(pid, "PES bans DTS without PTS")),
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
            reason: "PES timestamp or no marker bits".to_owned(),
        });
    }
    if bytes[0] >> 4 != expected_prefix {
        return Err(MpegTsDemuxError::Malformed {
            reason: "PES timestamp prefix".to_owned(),
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

#[cfg(test)]
mod tests {
    use super::*;

    const PID: u16 = 0x0101;

    fn pes_start(pts_marker_payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0x00, 0x00, 0x01, 0xc0, 0x00, 0x00, 0x80, 0x80, 0x05];
        bytes.extend_from_slice(&[0x21, 0x00, 0x01, 0x00, 0x01]);
        bytes.extend_from_slice(pts_marker_payload);
        bytes
    }

    #[test]
    fn continuation_without_seen_start_is_skipped_until_next_payload_unit_start() {
        let mut assembler = PesAssembler::new(PID, 1_024);
        // Хвост чужого PES (как после seek посреди PES) не накапливается.
        assert!(matches!(
            assembler.push(false, &[0xaa, 0xbb, 0xcc], 0),
            Ok(None)
        ));
        assert!(matches!(
            assembler.push(true, &pes_start(&[1, 2]), 188),
            Ok(None)
        ));
        assert!(matches!(assembler.push(false, &[3, 4], 376), Ok(None)));
        let completed = assembler.finish().expect("valid PES").expect("one PES");
        assert_eq!(completed.payload, vec![1, 2, 3, 4]);
        assert_eq!(completed.byte_offset, 188);
        assert_eq!(completed.pts, Some(0));
    }

    #[test]
    fn gap_close_distinguishes_declared_and_undeclared_length() {
        let mut declared = pes_start(&[1, 2]);
        // PES_packet_length = 3 (флаги) + 5 (PTS) + 2 байта payload-а.
        declared[5] = 10;
        let mut assembler = PesAssembler::new(PID, 1_024);
        assert!(matches!(assembler.push(true, &declared, 0), Ok(None)));
        assert!(matches!(
            assembler.close_after_continuity_gap(),
            Ok(InterruptedPes::Complete(pes)) if pes.payload == [1, 2]
        ));
        assert!(matches!(
            assembler.close_after_continuity_gap(),
            Ok(InterruptedPes::Absent)
        ));

        let mut truncated = declared.clone();
        truncated[5] = 20;
        assert!(matches!(assembler.push(true, &truncated, 0), Ok(None)));
        assert!(matches!(
            assembler.close_after_continuity_gap(),
            Err(MpegTsDemuxError::Malformed { .. })
        ));

        assert!(matches!(
            assembler.push(true, &pes_start(&[7]), 0),
            Ok(None)
        ));
        assert!(matches!(
            assembler.close_after_continuity_gap(),
            Ok(InterruptedPes::LengthNotDeclared(pes)) if pes.payload == [7]
        ));
    }

    #[test]
    fn peek_reads_pts_only_from_a_complete_header() {
        assert_eq!(peek_pes_pts(&pes_start(&[])), Some(0));
        assert_eq!(peek_pes_pts(&pes_start(&[])[..13]), None);
        let mut without_pts = pes_start(&[]);
        without_pts[7] = 0x00;
        assert_eq!(peek_pes_pts(&without_pts), None);
        assert_eq!(peek_pes_pts(&[0xff; 20]), None);
    }

    #[test]
    fn reset_mid_pes_drops_following_continuation() {
        let mut assembler = PesAssembler::new(PID, 1_024);
        assert!(matches!(
            assembler.push(true, &pes_start(&[1]), 0),
            Ok(None)
        ));
        assembler.reset();
        assert!(matches!(assembler.push(false, &[9, 9], 188), Ok(None)));
        assert!(matches!(assembler.finish(), Ok(None)));
    }
}
