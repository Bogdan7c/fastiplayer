//! CPU-формула HDR → SDR BT.2446 Method C в `f64`: источник узлов таблицы и эталон тестов.
//!
//! Шаги и константы — порт `render-wgpu-video/shaders/host_yuv420_16bit_to_sdr.wgsl` (шаги 1–8
//! плана сессии 06); совпадение констант с WGSL проверяет тест паритета в `render-wgpu-shell`
//! (разбор текста шейдера), совпадение результата — векторы эталона `bt2446c_reference` плеера
//! в тестах этого модуля.
//!
//! Ветвления повторяют WGSL, а не CPU-эталон плеера, там где они расходятся в мелочах (порог
//! «чёрного» для цветности 1e-6, а не `f64::EPSILON`): GLES2 сверяется с тем, что рисует wgpu.

use super::{HdrReferenceLevels, HdrTransfer};

/// Коэффициент R яркости BT.2020 (Rec. ITU-R BT.2020, таблица 4).
pub const BT2020_KR: f64 = 0.2627;
/// Коэффициент B яркости BT.2020.
pub const BT2020_KB: f64 = 0.0593;
/// Коэффициент G яркости BT.2020 (`1 − KR − KB`, как константа WGSL).
pub const BT2020_KG: f64 = 0.6780;
/// PQ m1 (SMPTE ST 2084).
pub const PQ_M1: f64 = 0.159_301_757_812_5;
/// PQ m2.
pub const PQ_M2: f64 = 78.843_75;
/// PQ c1.
pub const PQ_C1: f64 = 0.835_937_5;
/// PQ c2.
pub const PQ_C2: f64 = 18.851_562_5;
/// PQ c3.
pub const PQ_C3: f64 = 18.687_5;
/// Яркость PQ-кода 1,0 в нитах.
pub const PQ_REFERENCE_PEAK_NITS: f64 = 10_000.0;
/// HLG a (Rec. ITU-R BT.2100, таблица 5).
pub const HLG_A: f64 = 0.178_832_77;
/// HLG b.
pub const HLG_B: f64 = 0.284_668_92;
/// HLG c.
pub const HLG_C: f64 = 0.559_910_729_529_562;
/// Системная гамма HLG опорного дисплея 1000 нит.
pub const HLG_REFERENCE_SYSTEM_GAMMA: f64 = 1.2;
/// Crosstalk α BT.2446-C (Rec. ITU-R BT.2446, §6.2).
pub const BT2446C_CROSSTALK_ALPHA: f64 = 0.04;
/// Тон-кривая BT.2446-C: k1 (линейный участок).
pub const BT2446C_K1: f64 = 0.838_02;
/// k2 (логарифмический участок).
pub const BT2446C_K2: f64 = 15.099_68;
/// k3.
pub const BT2446C_K3: f64 = 0.742_04;
/// k4.
pub const BT2446C_K4: f64 = 78.994_39;
/// Яркость перегиба тон-кривой на выходе SDR, нит.
pub const BT2446C_SDR_INFLECTION_NITS: f64 = 58.5;
/// Гамма опорного SDR-дисплея (Rec. ITU-R BT.1886).
pub const BT1886_REFERENCE_GAMMA: f64 = 2.4;
/// Белая точка D65: x — цветность «чёрного» (сумма XYZ около нуля).
const D65_WHITE_X: f64 = 0.3127;
/// Белая точка D65: y.
const D65_WHITE_Y: f64 = 0.3290;
/// Порог «нуля» суммы XYZ и знаменателя цветности (как WGSL).
const CHROMATICITY_EPSILON: f64 = 0.000_001;
/// Линейный RGB BT.2020 → XYZ (строки), как в WGSL.
pub const BT2020_LINEAR_RGB_TO_XYZ: [[f64; 3]; 3] = [
    [0.6370, 0.1446, 0.1689],
    [0.2627, 0.6780, 0.0593],
    [0.0000, 0.0281, 1.0610],
];
/// XYZ → линейный RGB BT.709 (строки), как в WGSL.
pub const XYZ_TO_BT709_LINEAR_RGB: [[f64; 3]; 3] = [
    [1.7167, -0.3557, -0.2534],
    [-0.6667, 1.6165, 0.0158],
    [0.0176, -0.0428, 0.9421],
];

/// SDR-выход BT.709 по BT.2446-C: нелинейный R'G'B' BT.2020 (после матрицы YUV → RGB) →
/// закодированный BT.1886 RGB `0..1`. Эталон, с которым тесты сверяют GPU.
#[must_use]
pub fn bt2446c_to_sdr(
    nonlinear_bt2020_rgb: [f64; 3],
    transfer: HdrTransfer,
    levels: HdrReferenceLevels,
) -> [f64; 3] {
    unclamped_relative_sdr(nonlinear_bt2020_rgb, transfer, levels)
        .map(|relative| relative.clamp(0.0, 1.0).powf(1.0 / BT1886_REFERENCE_GAMMA))
}

/// Значение узла таблицы: та же цепочка без финального ограничения — знаковая гамма
/// `sign(x)·|x|^(1/2,4)` линейного SDR относительно белого. Для `x` из `0..1` равна
/// [`bt2446c_to_sdr`], вне — даёт тот же результат после `clamp` (см. модуль `hdr`). Таблица
/// считает то же поузлово быстрее (`lut::HdrLutTable::build`), здесь — сверка свойства.
#[cfg(test)]
pub(crate) fn unclamped_encoded_sdr(
    nonlinear_bt2020_rgb: [f64; 3],
    transfer: HdrTransfer,
    levels: HdrReferenceLevels,
) -> [f64; 3] {
    unclamped_relative_sdr(nonlinear_bt2020_rgb, transfer, levels).map(signed_inverse_bt1886)
}

/// Знаковая обратная гамма BT.1886 (таблица ищет байт по границам `lut::ByteThresholds`, эта
/// функция — эталон тестов).
#[cfg(test)]
pub(crate) fn signed_inverse_bt1886(relative_linear: f64) -> f64 {
    relative_linear.signum() * relative_linear.abs().powf(1.0 / BT1886_REFERENCE_GAMMA)
}

/// Шаги 1–7: R'G'B' → линейный SDR BT.709 в долях белого, без ограничения.
fn unclamped_relative_sdr(
    nonlinear_bt2020_rgb: [f64; 3],
    transfer: HdrTransfer,
    levels: HdrReferenceLevels,
) -> [f64; 3] {
    let linear_hdr_nits = linear_hdr_nits(nonlinear_bt2020_rgb, transfer, levels);
    relative_sdr_from_linear_hdr(linear_hdr_nits, levels)
}

/// Шаг 2: EOTF. Вход ограничивается `0..1` (как `apply_hdr_eotf` WGSL).
pub(crate) fn linear_hdr_nits(
    nonlinear_bt2020_rgb: [f64; 3],
    transfer: HdrTransfer,
    levels: HdrReferenceLevels,
) -> [f64; 3] {
    let bounded = nonlinear_bt2020_rgb.map(|value| value.clamp(0.0, 1.0));
    match transfer {
        HdrTransfer::Pq => bounded.map(pq_eotf_nits),
        HdrTransfer::Hlg => hlg_display_nits(bounded.map(hlg_inverse_oetf), levels),
    }
}

/// PQ EOTF: код `0..1` → нит (SMPTE ST 2084, уравнение 4.2).
pub(crate) fn pq_eotf_nits(encoded_value: f64) -> f64 {
    let encoded_root = encoded_value.clamp(0.0, 1.0).powf(1.0 / PQ_M2);
    let numerator = (encoded_root - PQ_C1).max(0.0);
    let denominator = PQ_C2 - PQ_C3 * encoded_root;
    PQ_REFERENCE_PEAK_NITS * (numerator / denominator).powf(1.0 / PQ_M1)
}

/// Обратная OETF HLG: код `0..1` → относительный свет сцены `0..1` (BT.2100, таблица 5).
pub(crate) fn hlg_inverse_oetf(encoded_value: f64) -> f64 {
    let bounded = encoded_value.clamp(0.0, 1.0);
    if bounded <= 0.5 {
        bounded * bounded / 3.0
    } else {
        (((bounded - HLG_C) / HLG_A).exp() + HLG_B) / 12.0
    }
}

/// OOTF HLG: свет сцены → нит дисплея с системной гаммой 1,2 вокруг яркости сцены. Пик дисплея —
/// больший из пика HDR и белого SDR (как WGSL).
pub(crate) fn hlg_display_nits(scene_rgb: [f64; 3], levels: HdrReferenceLevels) -> [f64; 3] {
    let scene_luma = BT2020_KR * scene_rgb[0] + BT2020_KG * scene_rgb[1] + BT2020_KB * scene_rgb[2];
    if scene_luma <= 0.0 {
        return [0.0; 3];
    }
    let peak_nits = f64::from(levels.hdr_peak_nits().max(levels.sdr_white_nits()));
    let system_gamma_scale = scene_luma.powf(HLG_REFERENCE_SYSTEM_GAMMA - 1.0);
    scene_rgb.map(|channel| peak_nits * system_gamma_scale * channel)
}

/// Шаги 3–7: линейный HDR BT.2020 (нит) → линейный SDR BT.709 в долях белого, без ограничения.
pub(crate) fn relative_sdr_from_linear_hdr(
    linear_hdr_nits: [f64; 3],
    levels: HdrReferenceLevels,
) -> [f64; 3] {
    let crosstalk_hdr = apply_crosstalk(linear_hdr_nits);
    let hdr_xyz = multiply(BT2020_LINEAR_RGB_TO_XYZ, crosstalk_hdr);
    let chromaticity = xyz_to_chromaticity(hdr_xyz);
    let tone_mapped_nits = tone_map_luminance_nits(hdr_xyz[1].max(0.0));
    let sdr_xyz = xyz_from_luminance_and_chromaticity(tone_mapped_nits, chromaticity);
    let crosstalk_sdr = multiply(XYZ_TO_BT709_LINEAR_RGB, sdr_xyz);
    let reference_white_nits = f64::from(levels.sdr_white_nits()).max(0.001);
    apply_inverse_crosstalk(crosstalk_sdr).map(|nits| nits / reference_white_nits)
}

/// Шаг 3: crosstalk α (смешивание каналов до тон-кривой — меньше сдвиг оттенка насыщенных цветов).
fn apply_crosstalk(rgb: [f64; 3]) -> [f64; 3] {
    let alpha = BT2446C_CROSSTALK_ALPHA;
    let self_weight = 1.0 - 2.0 * alpha;
    [
        self_weight * rgb[0] + alpha * rgb[1] + alpha * rgb[2],
        alpha * rgb[0] + self_weight * rgb[1] + alpha * rgb[2],
        alpha * rgb[0] + alpha * rgb[1] + self_weight * rgb[2],
    ]
}

/// Шаг 7: обратный crosstalk.
fn apply_inverse_crosstalk(rgb: [f64; 3]) -> [f64; 3] {
    let alpha = BT2446C_CROSSTALK_ALPHA;
    let denominator = 1.0 - 3.0 * alpha;
    [
        ((1.0 - alpha) * rgb[0] - alpha * rgb[1] - alpha * rgb[2]) / denominator,
        (-alpha * rgb[0] + (1.0 - alpha) * rgb[1] - alpha * rgb[2]) / denominator,
        (-alpha * rgb[0] - alpha * rgb[1] + (1.0 - alpha) * rgb[2]) / denominator,
    ]
}

/// Шаг 4: XYZ → цветность xy; у «чёрного» — белая точка D65.
fn xyz_to_chromaticity(xyz: [f64; 3]) -> [f64; 2] {
    let tristimulus_sum = xyz[0] + xyz[1] + xyz[2];
    if tristimulus_sum.abs() <= CHROMATICITY_EPSILON {
        return [D65_WHITE_X, D65_WHITE_Y];
    }
    [xyz[0] / tristimulus_sum, xyz[1] / tristimulus_sum]
}

/// Шаг 5: тон-кривая BT.2446-C по яркости Y: линейно до перегиба, дальше логарифм.
pub(crate) fn tone_map_luminance_nits(hdr_luminance_nits: f64) -> f64 {
    let hdr_inflection_nits = BT2446C_SDR_INFLECTION_NITS / BT2446C_K1;
    if hdr_luminance_nits < hdr_inflection_nits {
        BT2446C_K1 * hdr_luminance_nits
    } else {
        BT2446C_K2 * (hdr_luminance_nits / hdr_inflection_nits - BT2446C_K3).ln() + BT2446C_K4
    }
}

/// Шаг 6: XYZ из новой яркости и прежней цветности.
fn xyz_from_luminance_and_chromaticity(luminance_nits: f64, chromaticity: [f64; 2]) -> [f64; 3] {
    if luminance_nits <= 0.0 {
        return [0.0; 3];
    }
    let [x, y] = chromaticity;
    let safe_y = y.max(CHROMATICITY_EPSILON);
    [
        x / safe_y * luminance_nits,
        luminance_nits,
        (1.0 - x - y) / safe_y * luminance_nits,
    ]
}

fn multiply(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    matrix.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
}

#[cfg(test)]
mod tests;
