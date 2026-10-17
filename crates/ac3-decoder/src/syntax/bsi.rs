//! `bsi` — информация о потоке (A/52 §5.3.2, Таблица 5.2; Annex D Таблица D2.1).

use super::annex_d::{BsiExtension, read_bsi_extension};
use super::bsi_values::{
    AudioCodingMode, AudioProductionInfo, CenterMixLevel, DialogueNormalization, DolbySurroundMode,
    ProgramMetadata, RoomType, ServiceType, SurroundMixLevel,
};
use crate::bitstream::{BitReader, ReadPastLimit};

/// Базовый пиковый уровень сведения: `mixlevel` + 80 дБ SPL (§5.4.2.14).
const MIXING_LEVEL_BASE_DB_SPL: u8 = 80;

/// Разобранная информация о потоке одного syncframe-а AC-3.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BitStreamInfo {
    /// `bsid` (0–8 AC-3, 6 — Annex D, 9/10 — пониженная частота).
    pub bitstream_id: u8,
    /// Исходный `bsmod`.
    pub bitstream_mode: u8,
    /// Тип сервиса по `bsmod` и `acmod` (Таблица 5.7).
    pub service: ServiceType,
    /// `acmod`.
    pub audio_coding_mode: AudioCodingMode,
    /// `cmixlev`, если три фронтальных канала.
    pub center_mix_level: Option<CenterMixLevel>,
    /// `surmixlev`, если есть surround.
    pub surround_mix_level: Option<SurroundMixLevel>,
    /// `dsurmod`, только в режиме 2/0.
    pub dolby_surround_mode: Option<DolbySurroundMode>,
    /// `lfeon`.
    pub lfe_on: bool,
    /// Метаданные основной программы (Ch1 в режиме 1+1).
    pub program: ProgramMetadata,
    /// Метаданные Ch2, только в режиме 1+1.
    pub dual_mono_second_program: Option<ProgramMetadata>,
    /// `copyrightb`.
    pub copyright: bool,
    /// `origbs`.
    pub original_bitstream: bool,
    /// Тайм-код или Annex D `xbsi1`/`xbsi2`.
    pub extension: BsiExtension,
    /// Содержимое `addbsi` (1–64 байта) или пусто, если `addbsie == 0`.
    pub additional_bsi: Vec<u8>,
}

impl BitStreamInfo {
    /// Число полнополосных каналов `nfchans`.
    #[must_use]
    pub const fn full_bandwidth_channels(&self) -> usize {
        self.audio_coding_mode.full_bandwidth_channels()
    }
}

/// Читает `bsi` начиная с поля `bsid` (сразу после `syncinfo`).
pub(crate) fn read_bsi(reader: &mut BitReader<'_>) -> Result<BitStreamInfo, ReadPastLimit> {
    let bitstream_id = reader.read_u8(5)?;
    let bitstream_mode = reader.read_u8(3)?;
    let audio_coding_mode = AudioCodingMode::from_code(reader.read_u8(3)?);
    let center_mix_level = if audio_coding_mode.has_three_front_channels() {
        Some(CenterMixLevel::from_code(reader.read_u8(2)?))
    } else {
        None
    };
    let surround_mix_level = if audio_coding_mode.has_surround() {
        Some(SurroundMixLevel::from_code(reader.read_u8(2)?))
    } else {
        None
    };
    let dolby_surround_mode = if audio_coding_mode == AudioCodingMode::Stereo {
        Some(DolbySurroundMode::from_code(reader.read_u8(2)?))
    } else {
        None
    };
    let lfe_on = reader.read_flag()?;
    let program = read_program_metadata(reader)?;
    let dual_mono_second_program = if audio_coding_mode == AudioCodingMode::DualMono {
        Some(read_program_metadata(reader)?)
    } else {
        None
    };
    let copyright = reader.read_flag()?;
    let original_bitstream = reader.read_flag()?;
    let extension = read_bsi_extension(reader, bitstream_id)?;
    let additional_bsi = read_additional_bsi(reader)?;
    Ok(BitStreamInfo {
        bitstream_id,
        bitstream_mode,
        service: ServiceType::from_codes(bitstream_mode, audio_coding_mode),
        audio_coding_mode,
        center_mix_level,
        surround_mix_level,
        dolby_surround_mode,
        lfe_on,
        program,
        dual_mono_second_program,
        copyright,
        original_bitstream,
        extension,
        additional_bsi,
    })
}

/// `dialnorm`, `compre/compr`, `langcode/langcod`, `audprodie/mixlevel/roomtyp`
/// — одна и та же последовательность для Ch1 и (в режиме 1+1) для Ch2.
fn read_program_metadata(reader: &mut BitReader<'_>) -> Result<ProgramMetadata, ReadPastLimit> {
    let dialogue_normalization = DialogueNormalization::from_code(reader.read_u8(5)?);
    let compression_gain_word = if reader.read_flag()? {
        Some(reader.read_u8(8)?)
    } else {
        None
    };
    let language_code = if reader.read_flag()? {
        Some(reader.read_u8(8)?)
    } else {
        None
    };
    let production = if reader.read_flag()? {
        let mixing_level = reader.read_u8(5)?;
        let room_type = RoomType::from_code(reader.read_u8(2)?);
        Some(AudioProductionInfo {
            peak_mixing_level_db_spl: MIXING_LEVEL_BASE_DB_SPL + mixing_level,
            room_type,
        })
    } else {
        None
    };
    Ok(ProgramMetadata {
        dialogue_normalization,
        compression_gain_word,
        language_code,
        production,
    })
}

/// `addbsie`, `addbsil`, `addbsi[(addbsil + 1) × 8]` (§5.4.2.29–5.4.2.31).
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
