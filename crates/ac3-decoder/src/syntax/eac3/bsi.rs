//! `syncinfo` + `bsi` E-AC-3 (A/52 Annex E §2.2.1–2.2.2, Таблицы E1.1–E1.2,
//! семантика §2.3.1; ETSI TS 102 366 v1.4.1 §E.1.2.1–E.1.2.2, §E.1.3.1).
//!
//! В отличие от AC-3 длина кадра записана прямо в BSI (`frmsiz`), а частота —
//! `fscod` с «пониженными» частотами через `fscod2`. Поля сведения и
//! информационные поля разбираются в `bsi_metadata`.

use super::bsi_metadata::{
    InformationalMetadata, MetadataContext, MixingMetadata, read_informational_metadata,
    read_mixing_metadata,
};
use super::channel_map::CustomChannelMap;
use crate::bitstream::{BitReader, ReadPastLimit};
use crate::error::{Ac3DecodeError, BitstreamViolation, FrameSection};
use crate::syntax::bsi_values::{AudioCodingMode, DialogueNormalization};
use crate::tables::frame_size::SampleRateCode;

/// Байтов, в которых лежат все поля, нужные для длины и типа кадра:
/// syncword (2) + strmtyp/substreamid/frmsiz (2) + fscod… (1) + bsid (1).
pub(crate) const MINIMUM_HEADER_BYTES: usize = 6;
/// `fscod = 3` — «пониженная частота» по `fscod2` (Таблица E2.2).
const REDUCED_RATE_FSCOD: u8 = 3;
/// `numblkscod = 3` — шесть блоков (Таблица E2.4).
const SIX_BLOCKS_CODE: u8 = 3;
/// Блоков на кадр по `numblkscod` (Таблица E2.4).
const AUDIO_BLOCKS_BY_CODE: [usize; 4] = [1, 2, 3, 6];
/// Новых сэмплов на канал в аудиоблоке (§2.2: «as in AC-3»).
const SAMPLES_PER_AUDIO_BLOCK: usize = 256;

/// Тип substream-а `strmtyp` (Таблица E2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamType {
    /// Тип 0: независимый поток или substream.
    Independent,
    /// Тип 1: зависимый substream (декодируется вместе со своим независимым).
    Dependent,
    /// Тип 2: независимый substream, перекодированный из AC-3.
    ConvertedFromAc3,
}

impl StreamType {
    fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Independent),
            1 => Some(Self::Dependent),
            2 => Some(Self::ConvertedFromAc3),
            _ => None,
        }
    }

    /// Независимый ли это substream (тип 0 или 2).
    #[must_use]
    pub const fn is_independent(self) -> bool {
        !matches!(self, Self::Dependent)
    }
}

/// Метаданные одной программы (Ch1, а в режиме 1+1 ещё и Ch2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnhancedProgramMetadata {
    /// `dialnorm` / `dialnorm2`.
    pub dialogue_normalization: DialogueNormalization,
    /// `compr` / `compr2`, если передан (`compre`).
    pub compression_gain_word: Option<u8>,
}

/// Сведения о кадре, перекодированном из AC-3 (`strmtyp = 2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConvertedFrameInfo {
    /// `blkid`: первый блок кадра был первым блоком исходного кадра AC-3.
    pub first_block_of_ac3_frame: bool,
    /// `frmsizecod` исходного кадра AC-3 (передаётся при `blkid = 1`).
    pub ac3_frame_size_code: Option<u8>,
}

/// Разобранная информация о потоке одного syncframe-а E-AC-3.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EnhancedBitStreamInfo {
    /// `bsid` (11–16).
    pub bitstream_id: u8,
    /// `acmod`.
    pub audio_coding_mode: AudioCodingMode,
    /// `lfeon`.
    pub lfe_on: bool,
    /// Метаданные Ch1.
    pub program: EnhancedProgramMetadata,
    /// Метаданные Ch2 в режиме 1+1.
    pub dual_mono_second_program: Option<EnhancedProgramMetadata>,
    /// `chanmap` зависимого substream-а (`chanmape = 1`).
    pub channel_map: Option<CustomChannelMap>,
    /// Метаданные сведения (`mixmdate = 1`).
    pub mixing: Option<MixingMetadata>,
    /// Информационные метаданные (`infomdate = 1`).
    pub informational: Option<InformationalMetadata>,
    /// `convsync` (независимый substream с числом блоков меньше 6).
    pub converter_synchronization: Option<bool>,
    /// `blkid`/`frmsizecod` у `strmtyp = 2`.
    pub converted_frame: Option<ConvertedFrameInfo>,
    /// Содержимое `addbsi` (1–64 байта) или пусто.
    pub additional_bsi: Vec<u8>,
}

impl EnhancedBitStreamInfo {
    /// Число полнополосных каналов `nfchans`.
    #[must_use]
    pub const fn full_bandwidth_channels(&self) -> usize {
        self.audio_coding_mode.full_bandwidth_channels()
    }
}

/// Поля заголовка, которые нужны раньше BSI: тип, длина, частота, блоки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EnhancedFrameHeader {
    pub(crate) stream_type: StreamType,
    pub(crate) substream_id: u8,
    /// Длина кадра в байтах: `(frmsiz + 1) · 2` (§2.3.1.3).
    pub(crate) frame_bytes: usize,
    /// Строка таблиц, зависящих от частоты (Таблица 7.15). Для пониженных
    /// частот — `fscod2` (24/22,05/16 кГц — половины 48/44,1/32 кГц).
    pub(crate) sample_rate_code: SampleRateCode,
    pub(crate) sample_rate_hz: u32,
    pub(crate) reduced_sample_rate: bool,
    pub(crate) audio_blocks: usize,
    /// `numblkscod` (0–3).
    pub(crate) block_count_code: u8,
}

impl EnhancedFrameHeader {
    /// Сэмплов на канал в кадре.
    pub(crate) const fn samples_per_channel(&self) -> usize {
        self.audio_blocks * SAMPLES_PER_AUDIO_BLOCK
    }
}

/// Читает тип, длину и частоту кадра по первым байтам (без проверки, что
/// кадр целиком в буфере). `bsid` вызывающий код уже классифицировал.
pub(crate) fn peek_frame_header(frame: &[u8]) -> Result<EnhancedFrameHeader, Ac3DecodeError> {
    let Some(header) = frame.first_chunk::<MINIMUM_HEADER_BYTES>() else {
        return Err(Ac3DecodeError::Truncated {
            available_bytes: frame.len(),
            required_bytes: MINIMUM_HEADER_BYTES,
        });
    };
    let stream_type =
        StreamType::from_code(header[2] >> 6).ok_or(Ac3DecodeError::InvalidBitstream {
            section: FrameSection::BitStreamInformation,
            violation: BitstreamViolation::ReservedStreamType,
        })?;
    let substream_id = (header[2] >> 3) & 0x7;
    let frame_size_code = (usize::from(header[2] & 0x7) << 8) | usize::from(header[3]);
    let frame_bytes = (frame_size_code + 1) * 2;
    let fscod = header[4] >> 6;
    let second_code = (header[4] >> 4) & 0x3;
    let (sample_rate_code, reduced_sample_rate, block_count_code) = if fscod == REDUCED_RATE_FSCOD {
        // fscod2 (Таблица E2.3), блоков всегда 6; fscod2 = 3 зарезервирован.
        let code =
            SampleRateCode::from_code(second_code).ok_or(Ac3DecodeError::ReservedSampleRateCode)?;
        (code, true, SIX_BLOCKS_CODE)
    } else {
        let code =
            SampleRateCode::from_code(fscod).ok_or(Ac3DecodeError::ReservedSampleRateCode)?;
        (code, false, second_code)
    };
    let full_rate_hz = sample_rate_code.hertz();
    Ok(EnhancedFrameHeader {
        stream_type,
        substream_id,
        frame_bytes,
        sample_rate_code,
        sample_rate_hz: if reduced_sample_rate {
            full_rate_hz / 2
        } else {
            full_rate_hz
        },
        reduced_sample_rate,
        audio_blocks: AUDIO_BLOCKS_BY_CODE[usize::from(block_count_code)],
        block_count_code,
    })
}

/// Читает `bsi` после полей, разобранных [`peek_frame_header`]: читатель стоит
/// на `acmod` (бит 36 кадра).
pub(crate) fn read_enhanced_bsi(
    reader: &mut BitReader<'_>,
    header: &EnhancedFrameHeader,
) -> Result<EnhancedBitStreamInfo, Ac3DecodeError> {
    let overrun = |_: ReadPastLimit| Ac3DecodeError::BitstreamOverrun {
        section: FrameSection::BitStreamInformation,
    };
    let audio_coding_mode = AudioCodingMode::from_code(reader.read_u8(3).map_err(overrun)?);
    let lfe_on = reader.read_flag().map_err(overrun)?;
    let bitstream_id = reader.read_u8(5).map_err(overrun)?;
    let program = read_program(reader).map_err(overrun)?;
    let dual_mono_second_program = if audio_coding_mode == AudioCodingMode::DualMono {
        Some(read_program(reader).map_err(overrun)?)
    } else {
        None
    };
    let channel_map =
        if header.stream_type == StreamType::Dependent && reader.read_flag().map_err(overrun)? {
            Some(CustomChannelMap::from_bits(
                reader.read_bits(16).map_err(overrun)?,
            ))
        } else {
            None
        };
    let context = MetadataContext {
        mode: audio_coding_mode,
        lfe_on,
        independent: header.stream_type == StreamType::Independent,
        audio_blocks: header.audio_blocks,
        reduced_sample_rate: header.reduced_sample_rate,
    };
    let mixing = read_mixing_metadata(reader, &context).map_err(overrun)?;
    let informational = read_informational_metadata(reader, &context).map_err(overrun)?;
    let converter_synchronization = if header.stream_type == StreamType::Independent
        && header.block_count_code != SIX_BLOCKS_CODE
    {
        Some(reader.read_flag().map_err(overrun)?)
    } else {
        None
    };
    let converted_frame = if header.stream_type == StreamType::ConvertedFromAc3 {
        Some(read_converted_frame(reader, header).map_err(overrun)?)
    } else {
        None
    };
    let additional_bsi = read_additional_bsi(reader).map_err(overrun)?;
    Ok(EnhancedBitStreamInfo {
        bitstream_id,
        audio_coding_mode,
        lfe_on,
        program,
        dual_mono_second_program,
        channel_map,
        mixing,
        informational,
        converter_synchronization,
        converted_frame,
        additional_bsi,
    })
}

/// `dialnorm` (5) + `compre` (1) [+ `compr` (8)].
fn read_program(reader: &mut BitReader<'_>) -> Result<EnhancedProgramMetadata, ReadPastLimit> {
    let dialogue_normalization = DialogueNormalization::from_code(reader.read_u8(5)?);
    let compression_gain_word = if reader.read_flag()? {
        Some(reader.read_u8(8)?)
    } else {
        None
    };
    Ok(EnhancedProgramMetadata {
        dialogue_normalization,
        compression_gain_word,
    })
}

/// `blkid` (неявно 1 при шести блоках) и `frmsizecod`.
fn read_converted_frame(
    reader: &mut BitReader<'_>,
    header: &EnhancedFrameHeader,
) -> Result<ConvertedFrameInfo, ReadPastLimit> {
    let first_block_of_ac3_frame =
        header.block_count_code == SIX_BLOCKS_CODE || reader.read_flag()?;
    let ac3_frame_size_code = if first_block_of_ac3_frame {
        Some(reader.read_u8(6)?)
    } else {
        None
    };
    Ok(ConvertedFrameInfo {
        first_block_of_ac3_frame,
        ac3_frame_size_code,
    })
}

/// `addbsie`, `addbsil`, `addbsi[(addbsil + 1) × 8]`.
fn read_additional_bsi(reader: &mut BitReader<'_>) -> Result<Vec<u8>, ReadPastLimit> {
    if !reader.read_flag()? {
        return Ok(Vec::new());
    }
    let length_bytes = usize::from(reader.read_u8(6)?) + 1;
    let mut additional = Vec::with_capacity(length_bytes);
    for _ in 0..length_bytes {
        additional.push(reader.read_u8(8)?);
    }
    Ok(additional)
}
