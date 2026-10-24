//! Перевод YUV → RGB и цветокоррекция SDR: одна аффинная матрица на кадр.
//!
//! Единый цветовой модуль видеослоя: его переиспользуют высокая битность (сессия 06) и
//! аппаратный zero-copy (сессия 07). Числа и порядок операций — строгий паритет с wgpu-рендером
//! плеера (`render-wgpu-video/src/color_pipeline.rs` и `host_yuv420_8bit_to_rgba.wgsl`); паритет
//! закрепляет пиксельный тест против wgpu, а не сверка констант.
//!
//! Цепочка wgpu (`sampled` — байт / 255, как отдаёт текстура):
//! 1. нормализация диапазона: limited — `Y' = (Y − 0,0625) × 1,1643836`, `U' = U − 0,5`,
//!    `V' = V − 0,5` (16/256, а не 16/255: так считал исторический шейдер плеера); full —
//!    `Y' = Y`, `U' = U − 0,5`, `V' = V − 0,5`;
//! 2. матрица YUV → RGB (множитель цветности limited уже внутри коэффициентов);
//! 3. насыщенность вокруг яркости → контраст вокруг 0,5 → `× 2^exposure` → `+ brightness` →
//!    `× rgb_gain` → `+ rgb_offset`;
//! 4. ограничение `0..1`.
//!
//! Шаги 1–3 — аффинные преобразования (ограничения посередине нет), поэтому их композиция —
//! одна матрица 3×4, применяемая к `(Y, U, V, 1)`. Её считает CPU в `f64` один раз на кадр
//! ([`ColorTransform::new`]), шейдеру остаются три скалярных произведения и `clamp`. Почему так:
//! на Ironlake (T410) пошаговая формула на пиксель занимала заметную долю бюджета кадра, а
//! результат совпадает с пошаговым в `f32` до ~1e-6 — на три порядка точнее шага 1/255.
//!
//! 10/12 бит ([`high_bit`]) нормализуют коды отдельным шагом с ограничением (как wgpu), после
//! него — та же матрица (без множителя limited) и цветокоррекция одной матрицей. HDR → SDR
//! ([`crate::hdr`]): нормализация → матрица BT.2020 → таблица тон-маппинга; цветокоррекции SDR у
//! HDR-пути нет (у wgpu тоже).

/// Матрица YUV → RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum YuvMatrix {
    /// BT.601 (SD).
    Bt601,
    /// BT.709 (HD). BT.2020 и неизвестная матрица у wgpu тоже идут сюда — это решает адаптер.
    Bt709,
}

/// Диапазон значений YUV.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum YuvRange {
    /// Студийный диапазон (Y 16..235, UV 16..240).
    Limited,
    /// Полный диапазон 0..255.
    Full,
}

/// Как закодирован цвет кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct YuvColorEncoding {
    /// Матрица.
    pub matrix: YuvMatrix,
    /// Диапазон.
    pub range: YuvRange,
}

impl YuvColorEncoding {
    /// SDR BT.709 limited — безопасный выбор для неизвестного или неподдержанного входа.
    pub const SDR_BT709_LIMITED: Self = Self {
        matrix: YuvMatrix::Bt709,
        range: YuvRange::Limited,
    };
}

/// Цветокоррекция SDR. Порядок: насыщенность вокруг яркости → контраст вокруг 0,5 →
/// экспозиция `× 2^exposure` → яркость `+ brightness` → `× rgb_gain` → `+ rgb_offset` →
/// ограничение `0..1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorAdjustment {
    /// Прибавка к RGB после экспозиции.
    pub brightness: f32,
    /// Множитель расстояния от 0,5.
    pub contrast: f32,
    /// Множитель расстояния от серого той же яркости.
    pub saturation: f32,
    /// Ступени экспозиции: множитель `2^exposure`.
    pub exposure: f32,
    /// Поканальный множитель.
    pub rgb_gain: [f32; 3],
    /// Поканальная прибавка.
    pub rgb_offset: [f32; 3],
}

impl ColorAdjustment {
    /// Без коррекции.
    pub const IDENTITY: Self = Self {
        brightness: 0.0,
        contrast: 1.0,
        saturation: 1.0,
        exposure: 0.0,
        rgb_gain: [1.0; 3],
        rgb_offset: [0.0; 3],
    };
}

impl Default for ColorAdjustment {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Смещение яркости limited-диапазона: 16/256 (а не 16/255), как исторический шейдер плеера.
const LIMITED_LUMA_OFFSET: f64 = 0.0625;
/// Растяжение яркости limited-диапазона: 255 / 219 (то же `f32`-число, что у wgpu).
const LIMITED_LUMA_SCALE: f64 = 1.164_383_6;
/// Центр U/V в нормализованной текстуре.
const CHROMA_CENTER: f64 = 0.5;

/// BT.709 limited: строки `[Y, U, V]` для R, G, B.
const BT709_LIMITED_ROWS: [[f64; 3]; 3] = [
    [1.0, 0.0, 1.792_741_1],
    [1.0, -0.213_248_61, -0.532_909_33],
    [1.0, 2.112_401_7, 0.0],
];
/// BT.709 full.
const BT709_FULL_ROWS: [[f64; 3]; 3] = [
    [1.0, 0.0, 1.574_8],
    [1.0, -0.187_324_27, -0.468_124_27],
    [1.0, 1.855_6, 0.0],
];
/// BT.601 limited.
const BT601_LIMITED_ROWS: [[f64; 3]; 3] = [
    [1.0, 0.0, 1.596_027_1],
    [1.0, -0.391_762_3, -0.812_968],
    [1.0, 2.017_232_2, 0.0],
];
/// BT.601 full.
const BT601_FULL_ROWS: [[f64; 3]; 3] = [
    [1.0, 0.0, 1.402],
    [1.0, -0.344_136_3, -0.714_136_3],
    [1.0, 1.772, 0.0],
];
/// Веса яркости BT.709 для насыщенности.
const BT709_LUMA_WEIGHTS: [f64; 3] = [0.212_6, 0.715_2, 0.072_2];
/// Веса яркости BT.601 для насыщенности.
const BT601_LUMA_WEIGHTS: [f64; 3] = [0.299, 0.587, 0.114];

/// Аффинное преобразование `out = M × (x, y, z) + c` трёх компонент.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Affine3 {
    /// Строки матрицы.
    pub(crate) linear: [[f64; 3]; 3],
    /// Свободный член.
    pub(crate) constant: [f64; 3],
}

impl Affine3 {
    /// `self ∘ inner`: сначала `inner`, потом `self`.
    fn after(&self, inner: &Self) -> Self {
        let mut linear = [[0.0; 3]; 3];
        let mut constant = self.constant;
        for (row, (out_row, out_constant)) in linear.iter_mut().zip(&mut constant).enumerate() {
            for (column, out_cell) in out_row.iter_mut().enumerate() {
                *out_cell = (0..3)
                    .map(|k| self.linear[row][k] * inner.linear[k][column])
                    .sum();
            }
            *out_constant += (0..3)
                .map(|k| self.linear[row][k] * inner.constant[k])
                .sum::<f64>();
        }
        Self { linear, constant }
    }

    /// Поканальное `x × scale + offset`.
    fn per_channel(scale: [f64; 3], offset: [f64; 3]) -> Self {
        let mut linear = [[0.0; 3]; 3];
        for (channel, row) in linear.iter_mut().enumerate() {
            row[channel] = scale[channel];
        }
        Self {
            linear,
            constant: offset,
        }
    }

    /// Применить к трём компонентам (для проверок).
    #[cfg(test)]
    pub(crate) fn apply(&self, input: [f64; 3]) -> [f64; 3] {
        let mut output = self.constant;
        for (row, value) in output.iter_mut().enumerate() {
            *value += (0..3).map(|k| self.linear[row][k] * input[k]).sum::<f64>();
        }
        output
    }
}

/// Шаги 1–2 цепочки: нормализованный отсчёт YUV → линейный RGB до цветокоррекции.
pub(crate) fn yuv_to_rgb(encoding: YuvColorEncoding) -> Affine3 {
    let (luma_offset, luma_scale) = match encoding.range {
        YuvRange::Limited => (LIMITED_LUMA_OFFSET, LIMITED_LUMA_SCALE),
        YuvRange::Full => (0.0, 1.0),
    };
    let normalize = Affine3::per_channel(
        [luma_scale, 1.0, 1.0],
        [-luma_offset * luma_scale, -CHROMA_CENTER, -CHROMA_CENTER],
    );
    let matrix = Affine3 {
        linear: matrix_rows(encoding),
        constant: [0.0; 3],
    };
    matrix.after(&normalize)
}

/// Шаг 3 цепочки: цветокоррекция SDR над RGB (до ограничения).
pub(crate) fn sdr_adjustment(encoding: YuvColorEncoding, adjustment: ColorAdjustment) -> Affine3 {
    let weights = match encoding.matrix {
        YuvMatrix::Bt601 => BT601_LUMA_WEIGHTS,
        YuvMatrix::Bt709 => BT709_LUMA_WEIGHTS,
    };
    let saturation = f64::from(adjustment.saturation);
    // Насыщенность: `luma + (rgb − luma) × s = s × rgb + (1 − s) × (w · rgb)` для каждого канала.
    let mut saturate = Affine3 {
        linear: [weights.map(|weight| (1.0 - saturation) * weight); 3],
        constant: [0.0; 3],
    };
    for (channel, row) in saturate.linear.iter_mut().enumerate() {
        row[channel] += saturation;
    }
    let contrast = f64::from(adjustment.contrast);
    let contrast_step = Affine3::per_channel([contrast; 3], [0.5 - 0.5 * contrast; 3]);
    // Экспозиция — одна константа на кадр: `2^exposure` считает CPU.
    let exposure = f64::from(adjustment.exposure).exp2();
    let exposure_step = Affine3::per_channel([exposure; 3], [0.0; 3]);
    let gain = adjustment.rgb_gain.map(f64::from);
    let brightness = f64::from(adjustment.brightness);
    let offset = adjustment.rgb_offset.map(f64::from);
    // `(x + brightness) × gain + offset`.
    let finish = Affine3::per_channel(
        gain,
        [0, 1, 2].map(|channel| brightness * gain[channel] + offset[channel]),
    );
    finish
        .after(&exposure_step)
        .after(&contrast_step)
        .after(&saturate)
}

/// Строки матрицы кодировки. У full-диапазона это матрица нормализованного отсчёта (Y `0..1`,
/// U/V `−0,5..0,5`): её же берёт путь 10/12 бит, где нормализацию делает отдельный шаг.
fn matrix_rows(encoding: YuvColorEncoding) -> [[f64; 3]; 3] {
    match (encoding.matrix, encoding.range) {
        (YuvMatrix::Bt601, YuvRange::Limited) => BT601_LIMITED_ROWS,
        (YuvMatrix::Bt601, YuvRange::Full) => BT601_FULL_ROWS,
        (YuvMatrix::Bt709, YuvRange::Limited) => BT709_LIMITED_ROWS,
        (YuvMatrix::Bt709, YuvRange::Full) => BT709_FULL_ROWS,
    }
}

/// Цвет кадра для шейдера: `rgb = clamp(rows × (Y, U, V, 1), 0, 1)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ColorTransform {
    /// Строки `[Y, U, V, константа]` для R, G, B.
    pub(crate) rows: [[f32; 4]; 3],
}

impl ColorTransform {
    /// Вся SDR-цепочка 8 бит (шаги 1–3) одной матрицей.
    pub(crate) fn new(encoding: YuvColorEncoding, adjustment: ColorAdjustment) -> Self {
        Self::from_affine(&sdr_adjustment(encoding, adjustment).after(&yuv_to_rgb(encoding)))
    }

    /// Матрица для отсчёта, уже нормализованного [`high_bit::CodeNormalization`]: матрица
    /// кодировки и цветокоррекция.
    pub(crate) fn for_normalized_sdr(
        encoding: YuvColorEncoding,
        adjustment: ColorAdjustment,
    ) -> Self {
        Self::from_affine(
            &sdr_adjustment(encoding, adjustment)
                .after(&high_bit::normalized_yuv_to_rgb(encoding.matrix)),
        )
    }

    /// Нормализованный отсчёт → нелинейный R'G'B' BT.2020 (вход таблицы HDR → SDR).
    pub(crate) fn bt2020_nonlinear_rgb() -> Self {
        Self::from_affine(&high_bit::bt2020_normalized_yuv_to_rgb())
    }

    fn from_affine(combined: &Affine3) -> Self {
        // Uniform-ы шейдера — f32: сужение один раз на элемент, точность f32 на три порядка выше
        // шага 1/255.
        let narrow = |value: f64| value as f32;
        let row = |index: usize| {
            let linear = combined.linear[index];
            [
                narrow(linear[0]),
                narrow(linear[1]),
                narrow(linear[2]),
                narrow(combined.constant[index]),
            ]
        };
        Self {
            rows: [row(0), row(1), row(2)],
        }
    }
}

pub(crate) mod high_bit;

#[cfg(test)]
mod tests;
