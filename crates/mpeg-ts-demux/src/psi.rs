use std::collections::BTreeMap;

use crate::MpegTsDemuxError;

/// Один PAT program mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProgramMapEntry {
    /// MPEG program number.
    pub(crate) program_number: u16,
    /// PID соответствующего PMT.
    pub(crate) pmt_pid: u16,
}

/// Поддерживаемая elementary stream identity из PMT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamKind {
    /// ITU-T H.264 / ISO AVC.
    H264,
    /// ITU-T H.265 / ISO HEVC.
    H265,
    /// MPEG-2/4 AAC в ADTS transport framing.
    AacAdts,
    /// MPEG-1 audio stream type; layer уточняется frame header-ом.
    MpegAudio,
}

/// Один поддерживаемый PMT elementary stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ElementaryStream {
    /// Elementary PID.
    pub(crate) pid: u16,
    /// Codec family, доказанная stream_type.
    pub(crate) kind: StreamKind,
}

/// Нормализованный PMT snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgramMap {
    /// Program number из table_id_extension.
    pub(crate) program_number: u16,
    /// PSI version для change detection.
    pub(crate) version: u8,
    /// PID, несущий PCR для программы.
    pub(crate) pcr_pid: u16,
    /// Только profile-supported elementary streams.
    pub(crate) streams: Vec<ElementaryStream>,
}

/// Section assembler поддерживает pointer field и section split между packets.
#[derive(Debug, Default, Clone)]
pub(crate) struct PsiSectionAssembler {
    bytes: Vec<u8>,
    expected_length: Option<usize>,
}

impl PsiSectionAssembler {
    /// Сбрасывает только PSI конкретного PID после continuity/discontinuity.
    pub(crate) fn reset(&mut self) {
        self.bytes.clear();
        self.expected_length = None;
    }

    /// Принимает transport payload и возвращает все завершённые sections.
    pub(crate) fn push(
        &mut self,
        payload_unit_start: bool,
        payload: &[u8],
    ) -> Result<Vec<Vec<u8>>, MpegTsDemuxError> {
        let mut unread = payload;
        let mut sections = Vec::new();
        if payload_unit_start {
            let Some((&pointer, remainder)) = unread.split_first() else {
                return Err(malformed("PSI payload-unit-start без pointer_field"));
            };
            let pointer = usize::from(pointer);
            if pointer > remainder.len() {
                return Err(malformed("PSI pointer_field выходит за payload"));
            }
            if !self.bytes.is_empty() {
                self.append(&remainder[..pointer], &mut sections)?;
                if !self.bytes.is_empty() {
                    return Err(malformed(
                        "новая PSI section началась до завершения предыдущей",
                    ));
                }
            }
            unread = &remainder[pointer..];
        }
        self.append(unread, &mut sections)?;
        Ok(sections)
    }

    fn append(
        &mut self,
        mut unread: &[u8],
        sections: &mut Vec<Vec<u8>>,
    ) -> Result<(), MpegTsDemuxError> {
        while !unread.is_empty() {
            if self.bytes.is_empty() && unread[0] == 0xff {
                break;
            }
            if self.expected_length.is_none() {
                let header_needed = 3_usize.saturating_sub(self.bytes.len());
                let copied = header_needed.min(unread.len());
                self.bytes.extend_from_slice(&unread[..copied]);
                unread = &unread[copied..];
                if self.bytes.len() < 3 {
                    break;
                }
                let section_length =
                    (usize::from(self.bytes[1] & 0x0f) << 8) | usize::from(self.bytes[2]);
                if !(4..=1_021).contains(&section_length) {
                    return Err(malformed("PSI section_length вне MPEG-TS bounds"));
                }
                self.expected_length = Some(3 + section_length);
            }
            let expected = self.expected_length.expect("set above");
            let needed = expected.saturating_sub(self.bytes.len());
            let copied = needed.min(unread.len());
            self.bytes.extend_from_slice(&unread[..copied]);
            unread = &unread[copied..];
            if self.bytes.len() == expected {
                if mpeg_crc32(&self.bytes) != 0 {
                    self.reset();
                    return Err(malformed("PSI CRC32 не совпадает"));
                }
                sections.push(std::mem::take(&mut self.bytes));
                self.expected_length = None;
            }
        }
        Ok(())
    }
}

/// Разбирает complete PAT section и сохраняет все non-network programs.
pub(crate) fn parse_pat(section: &[u8]) -> Result<(u8, Vec<ProgramMapEntry>), MpegTsDemuxError> {
    validate_common_section(section, 0x00, 12)?;
    let version = (section[5] >> 1) & 0x1f;
    let mut programs = Vec::new();
    for entry in section[8..section.len() - 4].chunks_exact(4) {
        let program_number = u16::from_be_bytes([entry[0], entry[1]]);
        if program_number == 0 {
            continue;
        }
        let pmt_pid = (u16::from(entry[2] & 0x1f) << 8) | u16::from(entry[3]);
        programs.push(ProgramMapEntry {
            program_number,
            pmt_pid,
        });
    }
    if programs.is_empty() {
        return Err(malformed("PAT не содержит ни одной program mapping"));
    }
    Ok((version, programs))
}

/// Разбирает complete PMT section и отбрасывает unsupported stream types явно.
pub(crate) fn parse_pmt(section: &[u8]) -> Result<ProgramMap, MpegTsDemuxError> {
    validate_common_section(section, 0x02, 16)?;
    let program_number = u16::from_be_bytes([section[3], section[4]]);
    let version = (section[5] >> 1) & 0x1f;
    let pcr_pid = (u16::from(section[8] & 0x1f) << 8) | u16::from(section[9]);
    let program_info_length = (usize::from(section[10] & 0x0f) << 8) | usize::from(section[11]);
    let mut cursor = 12 + program_info_length;
    let payload_end = section.len() - 4;
    if cursor > payload_end {
        return Err(malformed("PMT program descriptors выходят за section"));
    }
    let mut streams = Vec::new();
    while cursor < payload_end {
        if payload_end - cursor < 5 {
            return Err(malformed("оборван PMT elementary stream entry"));
        }
        let stream_type = section[cursor];
        let pid = (u16::from(section[cursor + 1] & 0x1f) << 8) | u16::from(section[cursor + 2]);
        let descriptor_length =
            (usize::from(section[cursor + 3] & 0x0f) << 8) | usize::from(section[cursor + 4]);
        cursor += 5;
        if cursor + descriptor_length > payload_end {
            return Err(malformed("PMT ES descriptors выходят за section"));
        }
        let kind = match stream_type {
            0x1b => Some(StreamKind::H264),
            0x24 => Some(StreamKind::H265),
            0x0f => Some(StreamKind::AacAdts),
            0x03 | 0x04 => Some(StreamKind::MpegAudio),
            _ => None,
        };
        if let Some(kind) = kind {
            streams.push(ElementaryStream { pid, kind });
        }
        cursor += descriptor_length;
    }
    Ok(ProgramMap {
        program_number,
        version,
        pcr_pid,
        streams,
    })
}

/// Выбирает единственную playable program либо fail-closed сообщает ambiguity.
pub(crate) fn select_program(
    maps: &BTreeMap<u16, ProgramMap>,
) -> Result<Option<ProgramMap>, MpegTsDemuxError> {
    let playable: Vec<_> = maps
        .values()
        .filter(|program| !program.streams.is_empty())
        .cloned()
        .collect();
    match playable.as_slice() {
        [] => Ok(None),
        [program] => Ok(Some(program.clone())),
        programs => Err(MpegTsDemuxError::MultiplePlayablePrograms {
            programs: programs
                .iter()
                .map(|program| program.program_number)
                .collect(),
        }),
    }
}

fn validate_common_section(
    section: &[u8],
    expected_table_id: u8,
    minimum_length: usize,
) -> Result<(), MpegTsDemuxError> {
    if section.len() < minimum_length || section[0] != expected_table_id {
        return Err(malformed("неожиданный PSI table_id или короткая section"));
    }
    if section[1] & 0x80 == 0 || section[5] & 0x01 == 0 {
        return Err(malformed(
            "PSI section_syntax/current_next не разрешают применение",
        ));
    }
    Ok(())
}

/// MPEG-2 CRC32 с polynomial 0x04C11DB7 и initial all-ones.
pub(crate) fn mpeg_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn malformed(reason: &str) -> MpegTsDemuxError {
    MpegTsDemuxError::Malformed {
        reason: reason.to_owned(),
    }
}
