use std::collections::BTreeMap;

use media_core::{TrackLabels, TrackLanguage};

use crate::MpegTsDemuxError;
use crate::psi_descriptors::{
    Iso639LanguageCode, recognize_elementary_stream, stream_type_registry,
};

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
    /// AC-3 / E-AC-3 syncframes (ATSC 0x81/0x87 или DVB private PES с
    /// дескриптором); точный кодек доказывает `bsid` syncframe-а.
    Ac3Syncframes,
}

impl StreamKind {
    /// Видео access units (Annex-B), собираемые video assembler-ом.
    pub(crate) const fn is_video(self) -> bool {
        matches!(self, Self::H264 | Self::H265)
    }

    /// Аудио кадры, чей кодек/частоту/каналы доказывает elementary header.
    pub(crate) const fn is_audio(self) -> bool {
        matches!(self, Self::AacAdts | Self::MpegAudio | Self::Ac3Syncframes)
    }
}

/// Один поддерживаемый PMT elementary stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ElementaryStream {
    /// Elementary PID.
    pub(crate) pid: u16,
    /// Codec family, доказанная stream_type (и дескрипторами для private PES).
    pub(crate) kind: StreamKind,
    /// Язык из `ISO_639_language_descriptor`, если PMT его указала.
    pub(crate) language: Option<Iso639LanguageCode>,
}

impl ElementaryStream {
    /// Метки дорожки, которые сообщила PMT (сейчас — только язык).
    pub(crate) fn track_labels(&self) -> TrackLabels {
        TrackLabels {
            language: self.language.map(|code| TrackLanguage::new(code.as_str())),
            ..TrackLabels::default()
        }
    }
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
                return Err(malformed("PSI payload-unit start without pointer_field"));
            };
            let pointer = usize::from(pointer);
            if pointer > remainder.len() {
                return Err(malformed("PSI pointer_field is coming to payload"));
            }
            if !self.bytes.is_empty() {
                self.append(&remainder[..pointer], &mut sections)?;
                if !self.bytes.is_empty() {
                    return Err(malformed(
                        "The new PSI section begins before the previous one is completed.",
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
            let expected = if let Some(expected) = self.expected_length {
                expected
            } else {
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
                    return Err(malformed("PSI section_length outside MPEG-TS bounds"));
                }
                let expected = 3 + section_length;
                self.expected_length = Some(expected);
                expected
            };
            let needed = expected.saturating_sub(self.bytes.len());
            let copied = needed.min(unread.len());
            self.bytes.extend_from_slice(&unread[..copied]);
            unread = &unread[copied..];
            if self.bytes.len() == expected {
                if mpeg_crc32(&self.bytes) != 0 {
                    self.reset();
                    return Err(malformed("PSI CRC32 doesn't match"));
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
    for entry in section[8..section.len() - 4].as_chunks::<4>().0 {
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
        return Err(malformed("PAT does not contain any mapping program."));
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
        return Err(malformed("PMT program descriptors go beyond the section"));
    }
    let registry = stream_type_registry(&section[12..cursor], program_number);
    let mut streams = Vec::new();
    while cursor < payload_end {
        if payload_end - cursor < 5 {
            return Err(malformed("PMT elementary stream entry"));
        }
        let stream_type = section[cursor];
        let pid = (u16::from(section[cursor + 1] & 0x1f) << 8) | u16::from(section[cursor + 2]);
        let descriptor_length =
            (usize::from(section[cursor + 3] & 0x0f) << 8) | usize::from(section[cursor + 4]);
        cursor += 5;
        if cursor + descriptor_length > payload_end {
            return Err(malformed("PMT ES descriptors go beyond section"));
        }
        let descriptor_loop = &section[cursor..cursor + descriptor_length];
        if let Some(recognized) =
            recognize_elementary_stream(registry, stream_type, pid, descriptor_loop)
        {
            streams.push(ElementaryStream {
                pid,
                kind: recognized.kind,
                language: recognized.language,
            });
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
        return Err(malformed("PSI table_id or short section"));
    }
    if section[1] & 0x80 == 0 || section[5] & 0x01 == 0 {
        return Err(malformed(
            "PSI section_syntax/current_next does not allow use",
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
