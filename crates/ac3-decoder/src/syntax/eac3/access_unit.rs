//! Access unit E-AC-3: независимый substream 0 (или ядро AC-3) и его
//! зависимые substream-ы (A/52 Annex E §2.3.1.1–2.3.1.2, §3.8;
//! ETSI TS 102 366 v1.4.1 §E.1.3.1.1–E.1.3.1.2, §E.2.8).
//!
//! Модуль режет access unit на syncframe-ы по их собственным заголовкам,
//! проверяет состав, разбирает кадры программы 0 до коэффициентов и
//! сопоставляет каждому кодированному каналу его позицию (Таблица 5.8 или
//! `chanmap`, Таблица E2.5). Сведение каналов программы в один набор
//! (замена совпадающих позиций, добавление новых, §3.8.2) и PCM — забота
//! синтеза; другие независимые программы (substreamid 1–7) пропускаются.

use super::audblk::mantissas::AhtFrameStore;
use super::bsi::{StreamType, peek_frame_header};
use super::channel_map::substream_positions;
use super::syncframe::{
    EnhancedFrameResources, EnhancedSyncframeInfo, decode_eac3_syncframe, verify_enhanced_crc,
};
use super::tool_usage::EnhancedToolUsage;
use crate::bitstream::crc::verify_syncframe_crcs;
use crate::config::CrcPolicy;
use crate::error::{Ac3DecodeError, AccessUnitViolation, BitstreamViolation, FrameSection};
use crate::output::{ChannelPosition, ac3_channel_positions};
use crate::spectral::dither::DitherGenerator;
use crate::spectral::spx::SpxNoiseGenerator;
use crate::syntax::audblk::output::DecodedAudioBlock;
use crate::syntax::syncframe::{
    AUDIO_BLOCKS_PER_SYNCFRAME, SyncframeInfo, decode_ac3_syncframe, frame_words,
};
use crate::syntax::syncinfo::SYNC_WORD;

/// Сколько байт нужно, чтобы узнать синтаксис кадра (`bsid` в байте 5).
const FAMILY_PROBE_BYTES: usize = 6;
/// Байт и сдвиг `bsid` (одинаковы в AC-3 и E-AC-3, Annex E §2.1).
const BSID_BYTE: usize = 5;
const BSID_SHIFT: u8 = 3;
/// Последний `bsid` синтаксиса AC-3 (9/10 — пониженные частоты, см. D1).
const LAST_AC3_BSID: u8 = 10;
/// `bsid` E-AC-3 (Annex E §2.3.1.6).
const EAC3_BSIDS: std::ops::RangeInclusive<u8> = 11..=16;
/// Наибольшее число зависимых substream-ов у независимого (§2.3.1.2).
const MAX_DEPENDENT_SUBSTREAMS: usize = 8;

/// Заголовок разобранного substream-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubstreamHeader {
    /// Ядро AC-3 (`bsid` ≤ 10) — независимый substream 0.
    Ac3(SyncframeInfo),
    /// Кадр E-AC-3.
    Enhanced(EnhancedSyncframeInfo),
}

impl SubstreamHeader {
    /// Частота дискретизации кадра (с учётом bsid 9/10 и `fscod2`).
    pub(crate) fn sample_rate_hz(&self) -> u32 {
        match self {
            Self::Ac3(info) => info.sample_rate_hz,
            Self::Enhanced(info) => info.sample_rate_hz,
        }
    }

    fn audio_blocks(&self) -> usize {
        match self {
            Self::Ac3(_) => AUDIO_BLOCKS_PER_SYNCFRAME,
            Self::Enhanced(info) => info.audio_blocks,
        }
    }

    fn is_converted_from_ac3(&self) -> bool {
        matches!(self, Self::Enhanced(info) if info.stream_type == StreamType::ConvertedFromAc3)
    }
}

/// Переиспользуемые буферы одного substream-а (позиции и коэффициенты).
#[derive(Debug, Clone)]
struct SubstreamBuffers {
    /// Позиции кодированных каналов: fbw в порядке потока, затем LFE.
    positions: Vec<ChannelPosition>,
    /// Коэффициенты блоков (действительны первые `audio_blocks()` заголовка).
    blocks: Box<[DecodedAudioBlock; AUDIO_BLOCKS_PER_SYNCFRAME]>,
}

impl SubstreamBuffers {
    fn new() -> Self {
        Self {
            positions: Vec::new(),
            blocks: Box::new(std::array::from_fn(|_| DecodedAudioBlock::silent())),
        }
    }
}

/// Заголовок и счётчики разобранного substream-а текущего access unit-а.
#[derive(Debug, Clone)]
struct SubstreamRecord {
    header: SubstreamHeader,
    tools: EnhancedToolUsage,
}

/// Один разобранный substream программы 0 (вид на буферы парсера).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ParsedSubstream<'a> {
    pub(crate) header: &'a SubstreamHeader,
    /// Позиции кодированных каналов: fbw в порядке потока, затем LFE.
    pub(crate) positions: &'a [ChannelPosition],
    /// Коэффициенты блоков (действительны первые `audio_blocks` заголовка).
    pub(crate) blocks: &'a [DecodedAudioBlock; AUDIO_BLOCKS_PER_SYNCFRAME],
    pub(crate) tools: EnhancedToolUsage,
}

/// Разобранный access unit: substream-ы программы 0 по порядку потока.
#[derive(Debug)]
pub(crate) struct ParsedAccessUnit<'a> {
    records: &'a [SubstreamRecord],
    buffers: &'a [SubstreamBuffers],
    /// Сколько syncframe-ов других программ (substreamid 1–7) пропущено.
    pub(crate) skipped_substreams: usize,
}

impl<'a> ParsedAccessUnit<'a> {
    /// Частота программы: частота независимого substream-а (зависимые
    /// обязаны совпадать с ней — проверено разбором, §2.3.1.2).
    pub(crate) fn sample_rate_hz(&self) -> u32 {
        self.records
            .first()
            .map_or(0, |record| record.header.sample_rate_hz())
    }

    /// Substream-ы программы 0: независимый (или ядро AC-3), затем зависимые.
    pub(crate) fn substreams(&self) -> impl Iterator<Item = ParsedSubstream<'a>> + 'a {
        self.records
            .iter()
            .zip(self.buffers)
            .map(|(record, buffers)| ParsedSubstream {
                header: &record.header,
                positions: &buffers.positions,
                blocks: &buffers.blocks,
                tools: record.tools,
            })
    }
}

/// Владелец буферов и межкадрового состояния разбора access unit-ов.
///
/// Буферы (substream-ы с коэффициентами, данные AHT) переиспользуются между
/// access unit-ами; между кадрами живёт только генератор шума SPX (и dither
/// декодера, который передаётся снаружи). Разбирает и одиночные кадры AC-3
/// (программа из ядра без зависимых).
#[derive(Debug, Clone)]
pub(crate) struct EnhancedAccessUnitParser {
    crc: CrcPolicy,
    aht: Box<AhtFrameStore>,
    spx_noise: SpxNoiseGenerator,
    /// Буферы substream-ов; растут по требованию и не освобождаются.
    buffers: Vec<SubstreamBuffers>,
    /// Заголовки substream-ов текущего access unit-а (очищаются на каждый).
    records: Vec<SubstreamRecord>,
}

/// Синтаксис очередного кадра access unit-а.
enum FrameFamily {
    Ac3,
    Enhanced,
}

impl EnhancedAccessUnitParser {
    pub(crate) fn new(crc: CrcPolicy) -> Self {
        Self {
            crc,
            aht: AhtFrameStore::new(),
            spx_noise: SpxNoiseGenerator::new(),
            buffers: Vec::new(),
            records: Vec::new(),
        }
    }

    /// Возвращает генератор шума SPX к зерну (`Ac3Decoder::reset`): тот же
    /// вход после сброса даёт тот же шум. Буферы состоянием не являются —
    /// каждый кадр перезаписывает их целиком.
    pub(crate) fn reset(&mut self) {
        self.spx_noise = SpxNoiseGenerator::new();
    }

    /// Генератор шума SPX стоит на зерне (как после создания или `reset`).
    #[cfg(test)]
    pub(crate) fn spx_noise_is_at_seed(&self) -> bool {
        self.spx_noise == SpxNoiseGenerator::new()
    }

    /// Буфер блоков substream-а 0 (тесты разбора отдельных кадров AC-3).
    #[cfg(test)]
    pub(crate) fn primary_blocks_mut(
        &mut self,
    ) -> &mut [DecodedAudioBlock; AUDIO_BLOCKS_PER_SYNCFRAME] {
        if self.buffers.is_empty() {
            self.buffers.push(SubstreamBuffers::new());
        }
        &mut self.buffers[0].blocks
    }

    /// Последний успешно разобранный access unit (буферы живут до следующего
    /// разбора); `skipped_substreams` не сохраняется.
    #[cfg(test)]
    pub(crate) fn last_access_unit(&self) -> ParsedAccessUnit<'_> {
        ParsedAccessUnit {
            records: &self.records,
            buffers: &self.buffers,
            skipped_substreams: 0,
        }
    }

    /// Другое зерно шума SPX: dev-сверка отличает бины, зависящие от шума
    /// SPX, сравнением двух декодеров с разными зёрнами.
    #[cfg(test)]
    pub(crate) fn reseed_spx_noise(&mut self, seed: u32) {
        self.spx_noise = SpxNoiseGenerator::with_seed(seed);
    }

    /// Коэффициенты substream-а 0 последнего разбора.
    #[cfg(test)]
    pub(crate) fn primary_blocks(&self) -> &[DecodedAudioBlock; AUDIO_BLOCKS_PER_SYNCFRAME] {
        &self.buffers[0].blocks
    }

    /// Разбирает access unit. При ошибке содержимое буферов не определено.
    pub(crate) fn parse(
        &mut self,
        access_unit: &[u8],
        dither: &mut DitherGenerator,
    ) -> Result<ParsedAccessUnit<'_>, Ac3DecodeError> {
        if access_unit.is_empty() {
            return Err(Ac3DecodeError::Truncated {
                available_bytes: 0,
                required_bytes: FAMILY_PROBE_BYTES,
            });
        }
        self.records.clear();
        let mut offset = 0usize;
        let mut skipped = 0usize;
        // Кадры какой программы идут сейчас: 0 — наша, иначе пропускаем.
        let mut in_program_zero = false;
        while offset < access_unit.len() {
            let rest = access_unit.get(offset..).unwrap_or_default();
            let (family, length) = frame_family_and_length(rest)?;
            let frame = rest.get(..length).ok_or(Ac3DecodeError::Truncated {
                available_bytes: rest.len(),
                required_bytes: length,
            })?;
            match family {
                FrameFamily::Ac3 => {
                    if offset != 0 {
                        return Err(invalid(AccessUnitViolation::MisplacedAc3Syncframe));
                    }
                    in_program_zero = true;
                    self.parse_ac3_core(frame, dither)?;
                }
                FrameFamily::Enhanced => {
                    let header = peek_frame_header(frame)?;
                    if header.stream_type.is_independent() {
                        in_program_zero = header.substream_id == 0;
                        if offset == 0 && !in_program_zero {
                            return Err(invalid(
                                AccessUnitViolation::MissingIndependentSubstreamZero,
                            ));
                        }
                        if offset != 0 && in_program_zero {
                            return Err(invalid(AccessUnitViolation::RepeatedIndependentSubstream));
                        }
                    } else if offset == 0 {
                        return Err(invalid(
                            AccessUnitViolation::DependentSubstreamWithoutIndependent,
                        ));
                    }
                    if !in_program_zero {
                        skipped += 1;
                    } else {
                        let dependents = self.records.len().saturating_sub(1);
                        if !header.stream_type.is_independent()
                            && dependents >= MAX_DEPENDENT_SUBSTREAMS
                        {
                            return Err(invalid(AccessUnitViolation::TooManyDependentSubstreams));
                        }
                        self.parse_enhanced(frame, dither)?;
                    }
                }
            }
            offset += length;
        }
        Ok(ParsedAccessUnit {
            records: &self.records,
            buffers: &self.buffers,
            skipped_substreams: skipped,
        })
    }

    /// Буферы следующего substream-а (номер = число уже разобранных).
    fn next_buffers(&mut self) -> &mut SubstreamBuffers {
        let index = self.records.len();
        while self.buffers.len() <= index {
            self.buffers.push(SubstreamBuffers::new());
        }
        &mut self.buffers[index]
    }

    fn parse_ac3_core(
        &mut self,
        frame: &[u8],
        dither: &mut DitherGenerator,
    ) -> Result<(), Ac3DecodeError> {
        if self.crc == CrcPolicy::Verify {
            verify_syncframe_crcs(frame, frame.len() / 2)
                .map_err(|region| Ac3DecodeError::CrcMismatch { region })?;
        }
        let buffers = self.next_buffers();
        let decoded = decode_ac3_syncframe(frame, dither, &mut buffers.blocks)?;
        let bsi = &decoded.info.bsi;
        buffers.positions.clear();
        buffers
            .positions
            .extend(ac3_channel_positions(bsi.audio_coding_mode, bsi.lfe_on));
        self.records.push(SubstreamRecord {
            header: SubstreamHeader::Ac3(decoded.info),
            tools: EnhancedToolUsage::default(),
        });
        Ok(())
    }

    fn parse_enhanced(
        &mut self,
        frame: &[u8],
        dither: &mut DitherGenerator,
    ) -> Result<(), Ac3DecodeError> {
        if self.crc == CrcPolicy::Verify {
            verify_enhanced_crc(frame)?;
        }
        // Формат независимого substream-а программы (он всегда первый).
        let independent_format = self.records.first().map(|record| {
            (
                record.header.sample_rate_hz(),
                record.header.audio_blocks(),
                record.header.is_converted_from_ac3(),
            )
        });
        self.next_buffers();
        let mut tools = EnhancedToolUsage::default();
        let buffers = &mut self.buffers[self.records.len()];
        let decoded = decode_eac3_syncframe(
            frame,
            EnhancedFrameResources {
                aht: &mut self.aht,
                dither,
                spx_noise: &mut self.spx_noise,
            },
            &mut buffers.blocks,
            &mut tools,
        )?;
        let info = decoded.info;
        if let Some((sample_rate_hz, audio_blocks, converted)) = independent_format {
            if converted {
                return Err(invalid(
                    AccessUnitViolation::DependentSubstreamOfConvertedStream,
                ));
            }
            if sample_rate_hz != info.sample_rate_hz || audio_blocks != info.audio_blocks {
                return Err(invalid(
                    AccessUnitViolation::DependentSubstreamFormatMismatch,
                ));
            }
        }
        substream_positions(
            info.bsi.audio_coding_mode,
            info.bsi.lfe_on,
            info.bsi.channel_map,
            &mut buffers.positions,
        );
        let coded_channels = info.bsi.full_bandwidth_channels() + usize::from(info.bsi.lfe_on);
        if buffers.positions.len() != coded_channels {
            return Err(Ac3DecodeError::InvalidBitstream {
                section: FrameSection::BitStreamInformation,
                violation: BitstreamViolation::ChannelMapMismatch {
                    mapped_channels: buffers.positions.len() as u8,
                    coded_channels: coded_channels as u8,
                },
            });
        }
        self.records.push(SubstreamRecord {
            header: SubstreamHeader::Enhanced(info),
            tools,
        });
        Ok(())
    }
}

fn invalid(violation: AccessUnitViolation) -> Ac3DecodeError {
    Ac3DecodeError::InvalidAccessUnit(violation)
}

/// Синтаксис и длина очередного кадра по его первым байтам.
fn frame_family_and_length(rest: &[u8]) -> Result<(FrameFamily, usize), Ac3DecodeError> {
    let Some(probe) = rest.first_chunk::<FAMILY_PROBE_BYTES>() else {
        return Err(Ac3DecodeError::Truncated {
            available_bytes: rest.len(),
            required_bytes: FAMILY_PROBE_BYTES,
        });
    };
    if u16::from_be_bytes([probe[0], probe[1]]) != SYNC_WORD {
        return Err(Ac3DecodeError::MissingSyncWord);
    }
    let bitstream_id = probe[BSID_BYTE] >> BSID_SHIFT;
    if bitstream_id <= LAST_AC3_BSID {
        Ok((FrameFamily::Ac3, frame_words(rest)? * 2))
    } else if EAC3_BSIDS.contains(&bitstream_id) {
        Ok((FrameFamily::Enhanced, peek_frame_header(rest)?.frame_bytes))
    } else {
        Err(Ac3DecodeError::UnsupportedBitstreamId { bitstream_id })
    }
}
