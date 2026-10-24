//! Таблица HDR → SDR: узлы 65³ по R'G'B' BT.2020, разложенные срезами в 2D-атлас RGBA8.
//!
//! # Раскладка
//! Срез — все узлы с одним индексом B: квадрат `65 × 65` (R по горизонтали, G по вертикали).
//! Срезы лежат сеткой по [`ATLAS_COLUMNS`] в ряд: атлас `520 × 585` текселей. Это меньше 2048,
//! которые дают все известные драйверы GLES2 (минимум спецификации — 64; драйвер с меньшим
//! лимитом получит типизированный отказ, а не обрезанную таблицу). 3D-текстур (`GL_OES_texture_3D`) нет в ядре GLES2, а `glow` грузит только имя ES 3
//! `glTexImage3D`, поэтому атлас: шейдер берёт два соседних среза билинейной выборкой и смешивает
//! их по B сам.
//!
//! # Точность (замер сессии 06, `user/gles2-renderer/results/06.md`)
//! 65 узлов на ось — компромисс точности и размера: 33 давали до 4/255 и 0,6–1,8 % отсчётов с
//! отличием больше 1/255, 65 — до 3/255 и 0,1–0,3 %. Узел хранит значение до ограничения (см.
//! модуль `hdr`) в диапазоне [`ENCODED_MIN`]…[`ENCODED_MAX`]: ниже −0,05 и выше 1,25 значения
//! после `clamp` всё равно дают 0 и 1, а более широкий диапазон съедал точность 8 бит
//! (шаг `1,3 / 255` ≈ 0,005).

use super::bt2446c::{
    BT1886_REFERENCE_GAMMA, hlg_display_nits, hlg_inverse_oetf, pq_eotf_nits,
    relative_sdr_from_linear_hdr,
};
use super::{HdrReferenceLevels, HdrTransfer};

/// Узлов на ось.
pub(crate) const NODES_PER_AXIS: u32 = 65;
/// Срезов в ряду атласа.
pub(crate) const ATLAS_COLUMNS: u32 = 8;
/// Нижняя граница хранимого значения узла.
pub(crate) const ENCODED_MIN: f64 = -0.05;
/// Верхняя граница хранимого значения узла.
pub(crate) const ENCODED_MAX: f64 = 1.25;
/// Байт на тексель атласа (RGBA8; альфа не используется и равна 255).
pub(crate) const BYTES_PER_TEXEL: usize = 4;

/// Ширина атласа в текселях.
pub(crate) const fn atlas_width() -> u32 {
    ATLAS_COLUMNS * NODES_PER_AXIS
}

/// Высота атласа в текселях.
pub(crate) const fn atlas_height() -> u32 {
    NODES_PER_AXIS.div_ceil(ATLAS_COLUMNS) * NODES_PER_AXIS
}

/// Чем отличается одна таблица от другой: передаточная функция и уровни. Пик у PQ в формуле не
/// участвует, но входит в ключ — одна смена настроек даёт одну перестройку, это дёшево и проще
/// доказывать, чем «пик PQ можно не сравнивать».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HdrLutKey {
    transfer: HdrTransfer,
    sdr_white_bits: u32,
    hdr_peak_bits: u32,
}

impl HdrLutKey {
    pub(crate) fn new(transfer: HdrTransfer, levels: HdrReferenceLevels) -> Self {
        Self {
            transfer,
            sdr_white_bits: levels.sdr_white_nits().to_bits(),
            hdr_peak_bits: levels.hdr_peak_nits().to_bits(),
        }
    }
}

/// Байты атласа RGBA8, готовые к `glTexImage2D` (строка 0 — первая строка текстуры).
pub(crate) struct HdrLutTable {
    pub(crate) rgba: Vec<u8>,
}

impl HdrLutTable {
    /// Посчитать все узлы по формуле [`super::bt2446c_to_sdr`] (без финального ограничения).
    ///
    /// Цена — CPU на смену передаточной функции или уровней, в потоке вызывающего (замер
    /// сессии 06, release: 780M — 16 мс, T410 — 41 мс PQ / 50 мс HLG; без второго приёма ниже —
    /// 23 и 74/82 мс):
    /// - EOTF у обеих функций поканальная: она считается один раз на значение оси (65 раз), а
    ///   не на узел;
    /// - гамма BT.1886 не считается вовсе: байт узла ищется среди 255 границ, переведённых в
    ///   линейную шкалу один раз ([`ByteThresholds`]). На узел остаются линейные шаги и один
    ///   логарифм тон-кривой (у HLG ещё степень системной гаммы).
    pub(crate) fn build(transfer: HdrTransfer, levels: HdrReferenceLevels) -> Self {
        let axis_codes = (0..NODES_PER_AXIS).map(node_code);
        let axis_values: Vec<f64> = match transfer {
            HdrTransfer::Pq => axis_codes.map(pq_eotf_nits).collect(),
            HdrTransfer::Hlg => axis_codes.map(hlg_inverse_oetf).collect(),
        };
        let thresholds = ByteThresholds::new();
        let width = atlas_width() as usize;
        let mut rgba = vec![0; width * atlas_height() as usize * BYTES_PER_TEXEL];
        for blue in 0..NODES_PER_AXIS {
            for green in 0..NODES_PER_AXIS {
                for red in 0..NODES_PER_AXIS {
                    let per_axis = [red, green, blue].map(|index| axis_values[index as usize]);
                    let linear_hdr_nits = match transfer {
                        HdrTransfer::Pq => per_axis,
                        HdrTransfer::Hlg => hlg_display_nits(per_axis, levels),
                    };
                    let [r, g, b] = relative_sdr_from_linear_hdr(linear_hdr_nits, levels)
                        .map(|relative| thresholds.byte_for_relative_linear(relative));
                    let (x, y) = node_texel(red, green, blue);
                    let offset = (y as usize * width + x as usize) * BYTES_PER_TEXEL;
                    let texel = [r, g, b, u8::MAX];
                    rgba[offset..offset + BYTES_PER_TEXEL].copy_from_slice(&texel);
                }
            }
        }
        Self { rgba }
    }
}

/// Код R'G'B' узла с номером `index`: узлы равномерно от 0 до 1 включительно.
pub(crate) fn node_code(index: u32) -> f64 {
    f64::from(index) / f64::from(NODES_PER_AXIS - 1)
}

/// Тексель атласа, где лежит узел `(red, green, blue)`.
pub(crate) const fn node_texel(red: u32, green: u32, blue: u32) -> (u32, u32) {
    let slice_column = blue % ATLAS_COLUMNS;
    let slice_row = blue / ATLAS_COLUMNS;
    (
        slice_column * NODES_PER_AXIS + red,
        slice_row * NODES_PER_AXIS + green,
    )
}

/// Значение узла → байт: `ENCODED_MIN..ENCODED_MAX` линейно в `0..255` с округлением. Эталон
/// для [`ByteThresholds`] (сборка таблицы обходится без гаммы).
#[cfg(test)]
pub(crate) fn store_byte(encoded: f64) -> u8 {
    let normalized =
        (encoded.clamp(ENCODED_MIN, ENCODED_MAX) - ENCODED_MIN) / (ENCODED_MAX - ENCODED_MIN);
    // После clamp значение в 0..255: приведение без потерь диапазона.
    (normalized * 255.0).round() as u8
}

/// Границы между соседними байтами узла, переведённые в линейную шкалу (доли белого SDR).
///
/// Байт узла — округление `(gamma(x) − MIN) / (MAX − MIN) × 255`, где `gamma` — знаковая
/// обратная гамма BT.1886 (строго возрастает). Округление до ближайшего = сравнение с серединами
/// между байтами; переведя середины обратно (`sign(m)·|m|^2,4`), то же сравнение делается с
/// линейным `x` напрямую — без степени на каждый канал узла. Совпадение со [`store_byte`] от
/// гаммы проверяет unit-тест.
pub(crate) struct ByteThresholds {
    /// `linear[k]` — граница между байтами `k` и `k + 1` (возрастает).
    linear: [f64; 255],
}

impl ByteThresholds {
    pub(crate) fn new() -> Self {
        let step = (ENCODED_MAX - ENCODED_MIN) / 255.0;
        let linear = std::array::from_fn(|index| {
            let midpoint = ENCODED_MIN + (index as f64 + 0.5) * step;
            midpoint.signum() * midpoint.abs().powf(BT1886_REFERENCE_GAMMA)
        });
        Self { linear }
    }

    /// Байт узла для линейного значения `relative` (доли белого, до ограничения).
    pub(crate) fn byte_for_relative_linear(&self, relative: f64) -> u8 {
        // Число границ ниже значения — номер байта (0..=255). NaN не бывает: вход конечен.
        let index = self
            .linear
            .partition_point(|threshold| *threshold < relative);
        u8::try_from(index).unwrap_or(u8::MAX)
    }
}

/// Байт → значение узла (то же делает шейдер: `sample × scale + offset`).
pub(crate) fn decode_scale_offset() -> [f32; 2] {
    // Две константы шейдера; f32 точнее шага 1/255 на пять порядков.
    [(ENCODED_MAX - ENCODED_MIN) as f32, ENCODED_MIN as f32]
}

#[cfg(test)]
mod tests;
