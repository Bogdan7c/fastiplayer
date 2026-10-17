//! Метаданные BSI E-AC-3: сведение (`mixmdate`) и информационные
//! (`infomdate`) поля (A/52 Annex E §2.2.2 Таблица E1.2, §2.3.1.9–2.3.1.63;
//! ETSI TS 102 366 v1.4.1 §E.1.2.2, §E.1.3.1).
//!
//! Декодеру для PCM эти поля не нужны (сведение и downmix делает
//! приложение), но их нужно прочитать бит в бит, иначе съедет всё, что
//! идёт после BSI. Смысловые значения отдаются наружу как метаданные.

use crate::bitstream::{BitReader, ReadPastLimit};
use crate::syntax::annex_d::{
    AlternateCenterMixLevel, AlternateSurroundMixLevel, AnalogDigitalConverterType,
    DolbyHeadphoneMode, DolbySurroundExMode, PreferredStereoDownmix,
};
use crate::syntax::bsi_values::{AudioCodingMode, DolbySurroundMode, RoomType, ServiceType};

/// Базовый пиковый уровень сведения: `mixlevel` + 80 дБ SPL (§5.4.2.14).
const MIXING_LEVEL_BASE_DB_SPL: u8 = 80;
/// `mixdeflen` + 2 = длина поля `mixdata` в байтах (§2.3.1.22).
const MIX_DATA_LENGTH_BIAS_BYTES: usize = 2;

/// Режим дополнительных данных сведения `mixdef` (Таблица E2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MixingOption {
    /// `'00'` — вариант 1, дополнительных бит нет.
    Basic,
    /// `'01'` — вариант 2: `premixcmpsel`, `drcsrc`, `premixcmpscl`.
    PremixCompression {
        /// `premixcmpsel`: `true` — для premix используются поля `compr`.
        compression_words_selected: bool,
        /// `drcsrc`: `true` — DRC сведённого выхода из этого substream-а.
        drc_from_this_substream: bool,
        /// `premixcmpscl` (Таблица E2.7), 0–7.
        premix_compression_scale: u8,
    },
    /// `'10'` — вариант 3: 12 бит `mixdata`.
    Reserved12Bits {
        /// Сырые 12 бит.
        mix_data: u16,
    },
    /// `'11'` — вариант 4: поле переменной длины `(mixdeflen + 2)` байт.
    Extended {
        /// Длина поля в байтах.
        length_bytes: usize,
    },
}

/// Метаданные сведения (`mixmdate = 1`, §2.3.1.9–2.3.1.61).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct MixingMetadata {
    /// `dmixmod`, если каналов больше двух.
    pub preferred_downmix: Option<PreferredStereoDownmix>,
    /// `ltrtcmixlev`, если три фронтальных канала.
    pub lt_rt_center_mix_level: Option<AlternateCenterMixLevel>,
    /// `lorocmixlev`, если три фронтальных канала.
    pub lo_ro_center_mix_level: Option<AlternateCenterMixLevel>,
    /// `ltrtsurmixlev`, если есть surround.
    pub lt_rt_surround_mix_level: Option<AlternateSurroundMixLevel>,
    /// `lorosurmixlev`, если есть surround.
    pub lo_ro_surround_mix_level: Option<AlternateSurroundMixLevel>,
    /// `lfemixlevcod`: уровень LFE при downmix = `10 − код` дБ (§2.3.1.11).
    pub lfe_mix_level_code: Option<u8>,
    /// `pgmscl` (только независимый substream): 0 — mute, 1–63 — −50…+12 дБ.
    pub program_scale_factor: Option<u8>,
    /// `pgmscl2` для Ch2 режима 1+1.
    pub second_program_scale_factor: Option<u8>,
    /// `extpgmscl` — масштаб внешней программы.
    pub external_program_scale_factor: Option<u8>,
    /// `mixdef` и его поля (только независимый substream).
    pub mixing_option: Option<MixingOption>,
    /// `panmean` (моно/1+1 источник): 0–239 = 0…358,5° с шагом 1,5°.
    pub pan_mean_direction: Option<u8>,
    /// `panmean2` для Ch2 режима 1+1.
    pub second_pan_mean_direction: Option<u8>,
    /// Передана ли конфигурация сведения по блокам (`frmmixcfginfoe`).
    pub block_mixing_configuration: bool,
}

/// Информация о производстве программы (`audprodie`, §5.4.2.13–5.4.2.15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnhancedProductionInfo {
    /// Пиковый уровень сведения, дБ SPL (`mixlevel` + 80).
    pub peak_mixing_level_db_spl: u8,
    /// `roomtyp`.
    pub room_type: RoomType,
    /// `adconvtyp`.
    pub converter_type: AnalogDigitalConverterType,
}

/// Информационные метаданные (`infomdate = 1`, §2.3.1.62–2.3.1.63).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct InformationalMetadata {
    /// Исходный `bsmod`.
    pub bitstream_mode: u8,
    /// Тип сервиса по `bsmod`/`acmod` (Таблица 5.7).
    pub service: ServiceType,
    /// `copyrightb`.
    pub copyright: bool,
    /// `origbs`.
    pub original_bitstream: bool,
    /// `dsurmod` (только 2/0).
    pub dolby_surround_mode: Option<DolbySurroundMode>,
    /// `dheadphonmod` (только 2/0).
    pub dolby_headphone_mode: Option<DolbyHeadphoneMode>,
    /// `dsurexmod` (только при двух surround-каналах).
    pub dolby_surround_ex_mode: Option<DolbySurroundExMode>,
    /// `audprodie` → `mixlevel`/`roomtyp`/`adconvtyp`.
    pub production: Option<EnhancedProductionInfo>,
    /// То же для Ch2 режима 1+1.
    pub second_production: Option<EnhancedProductionInfo>,
    /// `sourcefscod`: источник дискретизирован с вдвое большей частотой.
    pub source_sample_rate_doubled: Option<bool>,
}

/// Неизменные в пределах BSI параметры, от которых зависит синтаксис метаданных.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MetadataContext {
    pub(crate) mode: AudioCodingMode,
    pub(crate) lfe_on: bool,
    pub(crate) independent: bool,
    pub(crate) audio_blocks: usize,
    pub(crate) reduced_sample_rate: bool,
}

/// Читает `mixmdate` и, если он установлен, поля сведения.
pub(crate) fn read_mixing_metadata(
    reader: &mut BitReader<'_>,
    context: &MetadataContext,
) -> Result<Option<MixingMetadata>, ReadPastLimit> {
    if !reader.read_flag()? {
        return Ok(None);
    }
    let acmod = context.mode.code();
    let preferred_downmix = if acmod > 2 {
        Some(PreferredStereoDownmix::from_code(reader.read_u8(2)?))
    } else {
        None
    };
    let (lt_rt_center_mix_level, lo_ro_center_mix_level) = if acmod & 1 == 1 && acmod > 2 {
        (
            Some(AlternateCenterMixLevel::from_code(reader.read_u8(3)?)),
            Some(AlternateCenterMixLevel::from_code(reader.read_u8(3)?)),
        )
    } else {
        (None, None)
    };
    let (lt_rt_surround_mix_level, lo_ro_surround_mix_level) = if acmod & 4 == 4 {
        (
            Some(AlternateSurroundMixLevel::from_code(reader.read_u8(3)?)),
            Some(AlternateSurroundMixLevel::from_code(reader.read_u8(3)?)),
        )
    } else {
        (None, None)
    };
    let lfe_mix_level_code = if context.lfe_on && reader.read_flag()? {
        Some(reader.read_u8(5)?)
    } else {
        None
    };
    let mut metadata = MixingMetadata {
        preferred_downmix,
        lt_rt_center_mix_level,
        lo_ro_center_mix_level,
        lt_rt_surround_mix_level,
        lo_ro_surround_mix_level,
        lfe_mix_level_code,
        program_scale_factor: None,
        second_program_scale_factor: None,
        external_program_scale_factor: None,
        mixing_option: None,
        pan_mean_direction: None,
        second_pan_mean_direction: None,
        block_mixing_configuration: false,
    };
    if context.independent {
        read_independent_mixing(reader, context, &mut metadata)?;
    }
    Ok(Some(metadata))
}

/// Часть метаданных сведения, которая есть только у независимого substream-а.
fn read_independent_mixing(
    reader: &mut BitReader<'_>,
    context: &MetadataContext,
    metadata: &mut MixingMetadata,
) -> Result<(), ReadPastLimit> {
    let dual_mono = context.mode == AudioCodingMode::DualMono;
    metadata.program_scale_factor = read_optional(reader, 6)?;
    if dual_mono {
        metadata.second_program_scale_factor = read_optional(reader, 6)?;
    }
    metadata.external_program_scale_factor = read_optional(reader, 6)?;
    metadata.mixing_option = Some(read_mixing_option(reader)?);
    if context.mode.code() < 2 {
        metadata.pan_mean_direction = read_pan_information(reader)?;
        if dual_mono {
            metadata.second_pan_mean_direction = read_pan_information(reader)?;
        }
    }
    if reader.read_flag()? {
        metadata.block_mixing_configuration = true;
        read_block_mixing_configuration(reader, context.audio_blocks)?;
    }
    Ok(())
}

/// Поле вида «флаг наличия + значение `bits` бит».
fn read_optional(reader: &mut BitReader<'_>, bits: u32) -> Result<Option<u8>, ReadPastLimit> {
    if reader.read_flag()? {
        Ok(Some(reader.read_u8(bits)?))
    } else {
        Ok(None)
    }
}

/// `mixdef` и его поля (Таблица E2.6).
fn read_mixing_option(reader: &mut BitReader<'_>) -> Result<MixingOption, ReadPastLimit> {
    Ok(match reader.read_u8(2)? {
        0 => MixingOption::Basic,
        1 => MixingOption::PremixCompression {
            compression_words_selected: reader.read_flag()?,
            drc_from_this_substream: reader.read_flag()?,
            premix_compression_scale: reader.read_u8(3)?,
        },
        2 => MixingOption::Reserved12Bits {
            mix_data: reader.read_bits(12)?,
        },
        _ => read_extended_mix_data(reader)?,
    })
}

/// `mixdef = 3`: `mixdeflen`, затем поле `(mixdeflen + 2)` байт, внутри
/// которого стоят `mixdata2e`/`mixdata3e` с их полями, а остаток — `mixdata`.
///
/// Толкование длины (неоднозначность §2.3.1.22–2.3.1.52): поле длиной
/// `8·(mixdeflen+2)` бит начинается сразу после `mixdeflen` и включает все
/// разобранные подполя; `mixdatafill` при этом пуст, потому что длина поля
/// уже кратна байту. Подполя длиннее поля — ошибка (`ReadPastLimit`).
fn read_extended_mix_data(reader: &mut BitReader<'_>) -> Result<MixingOption, ReadPastLimit> {
    let length_bytes = usize::from(reader.read_u8(5)?) + MIX_DATA_LENGTH_BIAS_BYTES;
    let field_bits = length_bytes * 8;
    let field_start = reader.position_bits();
    if reader.read_flag()? {
        read_external_channel_scales(reader)?;
    }
    if reader.read_flag()? {
        read_speech_enhancement(reader)?;
    }
    let consumed = reader.position_bits() - field_start;
    let remaining = field_bits.checked_sub(consumed).ok_or(ReadPastLimit)?;
    reader.skip_bits(remaining)?;
    Ok(MixingOption::Extended { length_bytes })
}

/// `mixdata2e = 1`: premix и масштабы каналов внешней программы (§2.3.1.24–2.3.1.43).
fn read_external_channel_scales(reader: &mut BitReader<'_>) -> Result<(), ReadPastLimit> {
    // premixcmpsel, drcsrc, premixcmpscl.
    reader.skip_bits(1 + 1 + 3)?;
    // extpgm{l,c,r,ls,rs,lfe}scl и dmixscl: флаг + 4 бита каждый.
    for _ in 0..7 {
        read_optional(reader, 4)?;
    }
    if reader.read_flag()? {
        // addche: extpgmaux1scl, extpgmaux2scl.
        read_optional(reader, 4)?;
        read_optional(reader, 4)?;
    }
    Ok(())
}

/// `mixdata3e = 1`: данные улучшения речи (§2.3.1.44–2.3.1.51).
fn read_speech_enhancement(reader: &mut BitReader<'_>) -> Result<(), ReadPastLimit> {
    reader.skip_bits(5)?; // spchdat
    if reader.read_flag()? {
        reader.skip_bits(5 + 2)?; // spchdat1, spchan1att
        if reader.read_flag()? {
            reader.skip_bits(5 + 3)?; // spchdat2, spchan2att
        }
    }
    Ok(())
}

/// `paninfoe` → `panmean` (8) + `paninfo` (6, зарезервировано).
fn read_pan_information(reader: &mut BitReader<'_>) -> Result<Option<u8>, ReadPastLimit> {
    if !reader.read_flag()? {
        return Ok(None);
    }
    let mean = reader.read_u8(8)?;
    reader.skip_bits(6)?;
    Ok(Some(mean))
}

/// `blkmixcfginfo`: при одном блоке — сразу 5 бит, иначе флаг + 5 бит на блок.
fn read_block_mixing_configuration(
    reader: &mut BitReader<'_>,
    audio_blocks: usize,
) -> Result<(), ReadPastLimit> {
    const BLOCK_MIXING_INFO_BITS: usize = 5;
    if audio_blocks == 1 {
        return reader.skip_bits(BLOCK_MIXING_INFO_BITS);
    }
    for _ in 0..audio_blocks {
        if reader.read_flag()? {
            reader.skip_bits(BLOCK_MIXING_INFO_BITS)?;
        }
    }
    Ok(())
}

/// Читает `infomdate` и, если он установлен, информационные поля.
pub(crate) fn read_informational_metadata(
    reader: &mut BitReader<'_>,
    context: &MetadataContext,
) -> Result<Option<InformationalMetadata>, ReadPastLimit> {
    if !reader.read_flag()? {
        return Ok(None);
    }
    let bitstream_mode = reader.read_u8(3)?;
    let copyright = reader.read_flag()?;
    let original_bitstream = reader.read_flag()?;
    let (dolby_surround_mode, dolby_headphone_mode) = if context.mode == AudioCodingMode::Stereo {
        (
            Some(DolbySurroundMode::from_code(reader.read_u8(2)?)),
            Some(DolbyHeadphoneMode::from_code(reader.read_u8(2)?)),
        )
    } else {
        (None, None)
    };
    let dolby_surround_ex_mode = if context.mode.code() >= 6 {
        Some(DolbySurroundExMode::from_code(reader.read_u8(2)?))
    } else {
        None
    };
    let production = read_production(reader)?;
    let second_production = if context.mode == AudioCodingMode::DualMono {
        read_production(reader)?
    } else {
        None
    };
    let source_sample_rate_doubled = if context.reduced_sample_rate {
        None
    } else {
        Some(reader.read_flag()?)
    };
    Ok(Some(InformationalMetadata {
        bitstream_mode,
        service: ServiceType::from_codes(bitstream_mode, context.mode),
        copyright,
        original_bitstream,
        dolby_surround_mode,
        dolby_headphone_mode,
        dolby_surround_ex_mode,
        production,
        second_production,
        source_sample_rate_doubled,
    }))
}

/// `audprodie` / `audprodi2e`: `mixlevel` (5), `roomtyp` (2), `adconvtyp` (1).
fn read_production(
    reader: &mut BitReader<'_>,
) -> Result<Option<EnhancedProductionInfo>, ReadPastLimit> {
    if !reader.read_flag()? {
        return Ok(None);
    }
    Ok(Some(EnhancedProductionInfo {
        peak_mixing_level_db_spl: MIXING_LEVEL_BASE_DB_SPL + reader.read_u8(5)?,
        room_type: RoomType::from_code(reader.read_u8(2)?),
        converter_type: AnalogDigitalConverterType::from_flag(reader.read_flag()?),
    }))
}
