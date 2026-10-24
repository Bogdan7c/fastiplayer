//! Нормализация кодов 10/12 бит и матрицы нормализованного отсчёта.
//!
//! Паритет с 16-битным путём wgpu (`host_yuv420_range_normalization` и
//! `normalize_host_planar_yuv_sample` в `render-wgpu-video`):
//! - limited: Y ограничивается кодами `16·2^s..235·2^s` и растягивается на `0..1`; U/V
//!   ограничиваются `0..max`, центр `128·2^s`, делитель `(240 − 16)·2^s` (`s = bits − 8`);
//! - full: Y `0..max` → `0..1`; U/V `0..max`, центр `128·2^s`, делитель `max`.
//!
//! Шейдер считает это одним `clamp(code × scale + offset, min, max)` на три канала: это то же
//! `(clamp(code, lo, hi) − center) × scale`, потому что `scale > 0` (ограничение переносится на
//! нормализованные границы). Ограничение посередине цепочки не даёт сложить шаг с матрицей, как
//! у 8 бит, поэтому он отдельный.

use super::{Affine3, YuvColorEncoding, YuvMatrix, YuvRange, matrix_rows};
use crate::hdr::{BT2020_KB, BT2020_KG, BT2020_KR};
use crate::planes::SampleDepth;

/// Uniform-ы нормализации: поканально (Y, U, V) `clamp(code × scale + offset, min, max)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CodeNormalization {
    pub(crate) scale: [f32; 3],
    pub(crate) offset: [f32; 3],
    pub(crate) min: [f32; 3],
    pub(crate) max: [f32; 3],
}

/// Нормализация одного канала в кодах: `(clamp(code, lowest, highest) − center) × scale`.
struct ChannelCodes {
    lowest: f64,
    highest: f64,
    center: f64,
    scale: f64,
}

impl CodeNormalization {
    /// Нормализация кодов разрядности `depth` в диапазоне `range`.
    pub(crate) fn new(depth: SampleDepth, range: YuvRange) -> Self {
        let max_code = f64::from(depth.max_code());
        // Сдвиг 8-битных границ диапазона под разрядность: `16 << s` и т. д.
        let shifted = |code_8bit: u32| f64::from(code_8bit << (depth.bits() - 8));
        let chroma_center = shifted(128);
        let (luma, chroma) = match range {
            YuvRange::Limited => (
                ChannelCodes {
                    lowest: shifted(16),
                    highest: shifted(235),
                    center: shifted(16),
                    scale: 1.0 / (shifted(235) - shifted(16)),
                },
                ChannelCodes {
                    lowest: 0.0,
                    highest: max_code,
                    center: chroma_center,
                    scale: 1.0 / (shifted(240) - shifted(16)),
                },
            ),
            YuvRange::Full => (
                ChannelCodes {
                    lowest: 0.0,
                    highest: max_code,
                    center: 0.0,
                    scale: 1.0 / max_code,
                },
                ChannelCodes {
                    lowest: 0.0,
                    highest: max_code,
                    center: chroma_center,
                    scale: 1.0 / max_code,
                },
            ),
        };
        let channels = [&luma, &chroma, &chroma];
        // Uniform-ы f32: коды до 4095 и множители ~1/4000 точны в f32 до ~1e-7.
        let narrow = |value: fn(&ChannelCodes) -> f64| channels.map(|codes| value(codes) as f32);
        Self {
            scale: narrow(|codes| codes.scale),
            offset: narrow(|codes| -codes.center * codes.scale),
            min: narrow(|codes| (codes.lowest - codes.center) * codes.scale),
            max: narrow(|codes| (codes.highest - codes.center) * codes.scale),
        }
    }

    /// Применить к кодам (для проверок): то же, что шейдер.
    #[cfg(test)]
    pub(crate) fn apply(&self, codes: [u32; 3]) -> [f64; 3] {
        [0, 1, 2].map(|channel| {
            (f64::from(codes[channel]) * f64::from(self.scale[channel])
                + f64::from(self.offset[channel]))
            .clamp(f64::from(self.min[channel]), f64::from(self.max[channel]))
        })
    }
}

/// Нормализованный отсчёт (Y `0..1`, U/V около 0) → RGB матрицей кодировки. Это строки
/// full-диапазона: у них нет множителя limited (BT.709 — те же числа, что `1,5748`,
/// `0,18732427`, `0,46812427`, `1,8556` 16-битного шейдера wgpu).
pub(crate) fn normalized_yuv_to_rgb(matrix: YuvMatrix) -> Affine3 {
    Affine3 {
        linear: matrix_rows(YuvColorEncoding {
            matrix,
            range: YuvRange::Full,
        }),
        constant: [0.0; 3],
    }
}

/// Нормализованный отсчёт → нелинейный R'G'B' BT.2020 (`bt2020_yuv_to_rgb` WGSL):
/// `R = Y + 2(1 − KR)·V`, `B = Y + 2(1 − KB)·U`, `G = (Y − KR·R − KB·B) / KG`.
pub(crate) fn bt2020_normalized_yuv_to_rgb() -> Affine3 {
    let red_from_v = 2.0 * (1.0 - BT2020_KR);
    let blue_from_u = 2.0 * (1.0 - BT2020_KB);
    Affine3 {
        linear: [
            [1.0, 0.0, red_from_v],
            // G = (Y − KR·(Y + a·V) − KB·(Y + b·U)) / KG = Y·(1 − KR − KB)/KG − …; коэффициент Y
            // взят из той же формулы (KG в WGSL — константа 0,6780, а не 1 − KR − KB).
            [
                (1.0 - BT2020_KR - BT2020_KB) / BT2020_KG,
                -BT2020_KB * blue_from_u / BT2020_KG,
                -BT2020_KR * red_from_v / BT2020_KG,
            ],
            [1.0, blue_from_u, 0.0],
        ],
        constant: [0.0; 3],
    }
}

#[cfg(test)]
mod tests;
