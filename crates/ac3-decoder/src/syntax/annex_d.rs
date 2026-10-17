//! Поля между `origbs` и `addbsie`: тайм-код (A/52 §5.4.2.26–5.4.2.28) или
//! альтернативный синтаксис BSI Annex D (`xbsi1`/`xbsi2`, при `bsid == 6`).
//!
//! Обе формы занимают одинаковое место (1 + 14 бит, дважды), поэтому legacy
//! декодеры читают Annex D как тайм-код (Annex D §3.2). Мы различаем их по
//! `bsid`, как требует Annex D §2.1.

use crate::bitstream::{BitReader, ReadPastLimit};

/// `bsid`, при котором действует альтернативный синтаксис (Annex D §2.1).
pub(crate) const ALTERNATE_SYNTAX_BSID: u8 = 6;

/// Расширенная часть BSI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BsiExtension {
    /// Исходный синтаксис: половины тайм-кода `timecod1`/`timecod2` (по 14 бит).
    TimeCode {
        /// `timecod1`: часы (5 бит), минуты (6), восьмисекундные шаги (3).
        first_half: Option<u16>,
        /// `timecod2`: секунды (3), кадры (5), доли кадра 1/64 (6).
        second_half: Option<u16>,
    },
    /// Альтернативный синтаксис Annex D.
    Alternate {
        /// `xbsi1`: предпочтительный downmix и его уровни.
        downmix: Option<ExtendedDownmixInfo>,
        /// `xbsi2`: информационные режимы.
        extra: Option<ExtendedProductionInfo>,
    },
}

/// Предпочтительный стереофонический downmix `dmixmod` (Annex D Таблица D2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreferredStereoDownmix {
    /// `'00'` — не указано.
    NotIndicated,
    /// `'01'` — Lt/Rt.
    LtRt,
    /// `'10'` — Lo/Ro.
    LoRo,
    /// `'11'` — зарезервирован (можно считать «не указано»).
    Reserved,
}

impl PreferredStereoDownmix {
    /// 2-битный `dmixmod` (Annex D Таблица D2.2; тот же код в E-AC-3 BSI).
    pub(crate) const fn from_code(code: u8) -> Self {
        match code & 0x3 {
            0 => Self::NotIndicated,
            1 => Self::LtRt,
            2 => Self::LoRo,
            _ => Self::Reserved,
        }
    }
}

/// Уровень центра Lt/Rt или Lo/Ro (Annex D Таблицы D2.3 и D2.5, одинаковые).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AlternateCenterMixLevel {
    code: u8,
}

impl AlternateCenterMixLevel {
    /// 3-битный `ltrtcmixlev`/`lorocmixlev` (Annex D и E-AC-3 BSI).
    pub(crate) const fn from_code(code: u8) -> Self {
        Self { code: code & 0x7 }
    }

    /// Коэффициенты Таблиц D2.3/D2.5 по коду 0–7.
    const GAINS: [f32; 8] = [1.414, 1.189, 1.000, 0.841, 0.707, 0.595, 0.500, 0.000];

    /// Исходный 3-битный код.
    #[must_use]
    pub const fn code(self) -> u8 {
        self.code
    }

    /// Коэффициент из таблицы (все 8 кодов определены).
    #[must_use]
    pub const fn gain(self) -> f32 {
        Self::GAINS[(self.code & 0x7) as usize]
    }
}

/// Уровень surround Lt/Rt или Lo/Ro (Annex D Таблицы D2.4 и D2.6, одинаковые).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AlternateSurroundMixLevel {
    code: u8,
}

impl AlternateSurroundMixLevel {
    /// 3-битный `ltrtsurmixlev`/`lorosurmixlev` (Annex D и E-AC-3 BSI).
    pub(crate) const fn from_code(code: u8) -> Self {
        Self { code: code & 0x7 }
    }

    /// Коды 0–2 зарезервированы; Annex D §2.3.1.4/§2.3.1.6 велит тогда брать 0,841.
    const GAINS: [f32; 8] = [0.841, 0.841, 0.841, 0.841, 0.707, 0.595, 0.500, 0.000];
    /// Первый определённый (не зарезервированный) код.
    const FIRST_DEFINED_CODE: u8 = 3;

    /// Исходный 3-битный код.
    #[must_use]
    pub const fn code(self) -> u8 {
        self.code
    }

    /// Зарезервирован ли код (0–2).
    #[must_use]
    pub const fn is_reserved(self) -> bool {
        self.code < Self::FIRST_DEFINED_CODE
    }

    /// Коэффициент из таблицы (для зарезервированных кодов — 0,841).
    #[must_use]
    pub const fn gain(self) -> f32 {
        Self::GAINS[(self.code & 0x7) as usize]
    }
}

/// `xbsi1` (Annex D §2.3.1.1–2.3.1.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExtendedDownmixInfo {
    /// `dmixmod`.
    pub preferred_downmix: PreferredStereoDownmix,
    /// `ltrtcmixlev`.
    pub lt_rt_center_mix_level: AlternateCenterMixLevel,
    /// `ltrtsurmixlev`.
    pub lt_rt_surround_mix_level: AlternateSurroundMixLevel,
    /// `lorocmixlev`.
    pub lo_ro_center_mix_level: AlternateCenterMixLevel,
    /// `lorosurmixlev`.
    pub lo_ro_surround_mix_level: AlternateSurroundMixLevel,
}

/// Режим Dolby Surround EX `dsurexmod` (Annex D Таблица D2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DolbySurroundExMode {
    /// `'00'` — не указано.
    NotIndicated,
    /// `'01'` — не EX / Pro Logic IIx / IIz.
    NotEncoded,
    /// `'10'` — Dolby Surround EX или Pro Logic IIx.
    SurroundExOrProLogicIIx,
    /// `'11'` — Dolby Pro Logic IIz.
    ProLogicIIz,
}

/// Режим Dolby Headphone `dheadphonmod` (Annex D Таблица D2.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DolbyHeadphoneMode {
    /// `'00'` — не указано.
    NotIndicated,
    /// `'01'` — не закодировано.
    NotEncoded,
    /// `'10'` — закодировано в Dolby Headphone.
    Encoded,
    /// `'11'` — зарезервирован.
    Reserved,
}

/// Тип АЦП `adconvtyp` (Annex D Таблица D2.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnalogDigitalConverterType {
    /// `'0'` — стандартный.
    Standard,
    /// `'1'` — HDCD.
    Hdcd,
}

/// `xbsi2` (Annex D §2.3.1.7–2.3.1.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExtendedProductionInfo {
    /// `dsurexmod`.
    pub dolby_surround_ex_mode: DolbySurroundExMode,
    /// `dheadphonmod`.
    pub dolby_headphone_mode: DolbyHeadphoneMode,
    /// `adconvtyp`.
    pub converter_type: AnalogDigitalConverterType,
    /// 8 бит `xbsi2`, зарезервированы для будущего (кодер пишет нули).
    pub reserved_bits: u8,
    /// `encinfo` — бит для нужд кодера.
    pub encoder_information: bool,
}

/// Читает расширенную часть BSI в нужной форме по `bsid`.
impl DolbySurroundExMode {
    /// 2-битный `dsurexmod` (Annex D Таблица D2.7; тот же код в E-AC-3 BSI).
    pub(crate) const fn from_code(code: u8) -> Self {
        match code & 0x3 {
            0 => Self::NotIndicated,
            1 => Self::NotEncoded,
            2 => Self::SurroundExOrProLogicIIx,
            _ => Self::ProLogicIIz,
        }
    }
}

impl DolbyHeadphoneMode {
    /// 2-битный `dheadphonmod` (Annex D Таблица D2.8; тот же код в E-AC-3 BSI).
    pub(crate) const fn from_code(code: u8) -> Self {
        match code & 0x3 {
            0 => Self::NotIndicated,
            1 => Self::NotEncoded,
            2 => Self::Encoded,
            _ => Self::Reserved,
        }
    }
}

impl AnalogDigitalConverterType {
    /// 1-битный `adconvtyp`: `1` — HDCD.
    pub(crate) const fn from_flag(hdcd: bool) -> Self {
        if hdcd { Self::Hdcd } else { Self::Standard }
    }
}

pub(crate) fn read_bsi_extension(
    reader: &mut BitReader<'_>,
    bitstream_id: u8,
) -> Result<BsiExtension, ReadPastLimit> {
    if bitstream_id == ALTERNATE_SYNTAX_BSID {
        let downmix = if reader.read_flag()? {
            Some(read_xbsi1(reader)?)
        } else {
            None
        };
        let extra = if reader.read_flag()? {
            Some(read_xbsi2(reader)?)
        } else {
            None
        };
        Ok(BsiExtension::Alternate { downmix, extra })
    } else {
        /// Ширина каждой половины тайм-кода (§5.4.2.27–5.4.2.28).
        const TIME_CODE_HALF_BITS: u32 = 14;
        let first_half = if reader.read_flag()? {
            Some(reader.read_bits(TIME_CODE_HALF_BITS)?)
        } else {
            None
        };
        let second_half = if reader.read_flag()? {
            Some(reader.read_bits(TIME_CODE_HALF_BITS)?)
        } else {
            None
        };
        Ok(BsiExtension::TimeCode {
            first_half,
            second_half,
        })
    }
}

fn read_xbsi1(reader: &mut BitReader<'_>) -> Result<ExtendedDownmixInfo, ReadPastLimit> {
    let preferred_downmix = PreferredStereoDownmix::from_code(reader.read_u8(2)?);
    let lt_rt_center_mix_level = AlternateCenterMixLevel {
        code: reader.read_u8(3)?,
    };
    let lt_rt_surround_mix_level = AlternateSurroundMixLevel {
        code: reader.read_u8(3)?,
    };
    let lo_ro_center_mix_level = AlternateCenterMixLevel {
        code: reader.read_u8(3)?,
    };
    let lo_ro_surround_mix_level = AlternateSurroundMixLevel {
        code: reader.read_u8(3)?,
    };
    Ok(ExtendedDownmixInfo {
        preferred_downmix,
        lt_rt_center_mix_level,
        lt_rt_surround_mix_level,
        lo_ro_center_mix_level,
        lo_ro_surround_mix_level,
    })
}

fn read_xbsi2(reader: &mut BitReader<'_>) -> Result<ExtendedProductionInfo, ReadPastLimit> {
    let dolby_surround_ex_mode = DolbySurroundExMode::from_code(reader.read_u8(2)?);
    let dolby_headphone_mode = DolbyHeadphoneMode::from_code(reader.read_u8(2)?);
    let converter_type = AnalogDigitalConverterType::from_flag(reader.read_flag()?);
    let reserved_bits = reader.read_u8(8)?;
    let encoder_information = reader.read_flag()?;
    Ok(ExtendedProductionInfo {
        dolby_surround_ex_mode,
        dolby_headphone_mode,
        converter_type,
        reserved_bits,
        encoder_information,
    })
}

#[cfg(test)]
mod tests {
    use super::{AlternateCenterMixLevel, AlternateSurroundMixLevel};

    #[test]
    fn alternate_mix_level_tables_follow_annex_d() {
        let center: Vec<f32> = (0..8)
            .map(|code| AlternateCenterMixLevel { code }.gain())
            .collect();
        assert_eq!(center, [1.414, 1.189, 1.0, 0.841, 0.707, 0.595, 0.5, 0.0]);
        let surround: Vec<(f32, bool)> = (0..8)
            .map(|code| {
                let level = AlternateSurroundMixLevel { code };
                (level.gain(), level.is_reserved())
            })
            .collect();
        assert_eq!(
            surround,
            [
                (0.841, true),
                (0.841, true),
                (0.841, true),
                (0.841, false),
                (0.707, false),
                (0.595, false),
                (0.5, false),
                (0.0, false)
            ]
        );
    }
}
