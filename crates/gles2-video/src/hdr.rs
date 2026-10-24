//! HDR → SDR BT.2446 Method C для 10/12-битных кадров: словарь и CPU-формула.
//!
//! # Почему таблица, а не формула в шейдере (решение владельца, сессия 06)
//! Точная формула wgpu (PQ/HLG EOTF → crosstalk → XYZ → тон-кривая → BT.709 → обратный
//! crosstalk → BT.1886) на пиксель стоила на T410 (Ironlake) **39,7 мс** GPU на кадр 1440×900 —
//! больше бюджета 30 к/с (33 мс). Поэтому GLES2 считает ту же формулу на CPU в `f64` один раз в
//! узлах таблицы 65³ ([`lut`]), а шейдер берёт готовый ответ и смешивает соседние узлы: **20,5 мс**
//! на том же кадре (весь проход видео 1080p; 10 бит SDR без таблицы — 14,5 мс). Цена — документированное отличие от точной формулы (замер сессии 06, CPU
//! модель с 8-битными узлами и квантованными весами фильтра; подтверждено readback-ом):
//! - худший случай — 3/255 на канал;
//! - отличие больше 1/255 — у 0,1–0,3 % отсчётов (сильно насыщенные цвета около перегиба
//!   ограничения), больше 2/255 — у < 0,01 %.
//!
//! Решение касается только этого рендера: wgpu-путь плеера считает формулу в шейдере, как раньше.
//!
//! # Что таблица хранит и почему так
//! Вся цепочка после матрицы YUV → R'G'B' BT.2020 зависит только от `clamp(R'G'B', 0, 1)` (первым
//! шагом EOTF ограничивает вход), поэтому таблица по R'G'B' покрывает область определения точно.
//! Финальное ограничение `0..1` создаёт излом; интерполяция через излом занижала бы светлые
//! участки (до 8/255 на 33³). Поэтому узел хранит значение **до** ограничения — знаковую гамму
//! `sign(x)·|x|^(1/2,4)` линейного SDR, — а `clamp` делает шейдер после смешивания. Для `x` из
//! `0..1` это ровно выход wgpu, вне — тот же результат после `clamp`.

use thiserror::Error;

use crate::color::YuvRange;

pub(crate) mod bt2446c;
pub(crate) mod lut;

pub use bt2446c::{
    BT1886_REFERENCE_GAMMA, BT2020_KB, BT2020_KG, BT2020_KR, BT2020_LINEAR_RGB_TO_XYZ,
    BT2446C_CROSSTALK_ALPHA, BT2446C_K1, BT2446C_K2, BT2446C_K3, BT2446C_K4,
    BT2446C_SDR_INFLECTION_NITS, HLG_A, HLG_B, HLG_C, HLG_REFERENCE_SYSTEM_GAMMA, PQ_C1, PQ_C2,
    PQ_C3, PQ_M1, PQ_M2, PQ_REFERENCE_PEAK_NITS, XYZ_TO_BT709_LINEAR_RGB, bt2446c_to_sdr,
};

/// Передаточная функция HDR-кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HdrTransfer {
    /// SMPTE ST 2084 (PQ), абсолютная яркость до 10 000 нит.
    Pq,
    /// ARIB STD-B67 / BT.2100 HLG: относительная яркость, пик задаёт дисплей.
    Hlg,
}

/// Опорные уровни тон-маппинга: белый SDR и пик HDR в нитах.
///
/// Проверенный тип: оба числа конечные и положительные, пик выше белого (то же условие, что
/// `HdrToSdrSettings::is_phase10_bt2446_c_sdr_bt709` у плеера).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrReferenceLevels {
    sdr_white_nits: f32,
    hdr_peak_nits: f32,
}

/// Опорные уровни не годятся.
#[derive(Debug, Clone, Copy, PartialEq, Error)]
pub enum HdrLevelsError {
    /// Число не конечное или не положительное.
    #[error(
        "HDR reference levels must be finite and positive (white {sdr_white_nits}, peak {hdr_peak_nits})"
    )]
    NotPositive {
        /// Белый SDR.
        sdr_white_nits: f32,
        /// Пик HDR.
        hdr_peak_nits: f32,
    },
    /// Пик не выше белого: тон-кривой нечего сжимать.
    #[error("HDR peak {hdr_peak_nits} nits must be above SDR white {sdr_white_nits} nits")]
    PeakNotAboveWhite {
        /// Белый SDR.
        sdr_white_nits: f32,
        /// Пик HDR.
        hdr_peak_nits: f32,
    },
}

impl HdrReferenceLevels {
    /// Уровни по умолчанию у плеера: белый 100 нит, пик 1000 нит.
    pub const REFERENCE_DEFAULT: Self = Self {
        sdr_white_nits: 100.0,
        hdr_peak_nits: 1000.0,
    };

    /// Проверенные уровни.
    ///
    /// # Errors
    /// [`HdrLevelsError`]: число не конечное/не положительное или пик не выше белого.
    pub fn new(sdr_white_nits: f32, hdr_peak_nits: f32) -> Result<Self, HdrLevelsError> {
        let positive = |value: f32| value.is_finite() && value > 0.0;
        if !positive(sdr_white_nits) || !positive(hdr_peak_nits) {
            return Err(HdrLevelsError::NotPositive {
                sdr_white_nits,
                hdr_peak_nits,
            });
        }
        if hdr_peak_nits <= sdr_white_nits {
            return Err(HdrLevelsError::PeakNotAboveWhite {
                sdr_white_nits,
                hdr_peak_nits,
            });
        }
        Ok(Self {
            sdr_white_nits,
            hdr_peak_nits,
        })
    }

    /// Белый SDR в нитах: яркость, которая станет выходом 1,0.
    #[must_use]
    pub const fn sdr_white_nits(self) -> f32 {
        self.sdr_white_nits
    }

    /// Пик HDR в нитах (у HLG — пик дисплея, у PQ в формуле не участвует, как у wgpu).
    #[must_use]
    pub const fn hdr_peak_nits(self) -> f32 {
        self.hdr_peak_nits
    }
}

/// Перевод HDR-кадра (BT.2020, PQ или HLG) в SDR BT.709 по BT.2446 Method C.
///
/// Формула считается на CPU в `f64` в узлах таблицы 65³, шейдер смешивает узлы (дешевле на
/// старом GPU: на Ironlake 20,5 мс на кадр вместо 39,7 у формулы в шейдере). Отличие от точной
/// формулы — не больше 3/255 на канал; больше 2/255 — у долей процента пикселей. Эталон для
/// сверки — [`bt2446c_to_sdr`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrToSdrConversion {
    /// Диапазон кодов кадра.
    pub range: YuvRange,
    /// Передаточная функция.
    pub transfer: HdrTransfer,
    /// Опорные уровни.
    pub levels: HdrReferenceLevels,
}

#[cfg(test)]
mod tests;
