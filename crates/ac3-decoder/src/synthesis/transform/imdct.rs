//! Обратное преобразование AC-3 с окном (A/52:2012 §7.9.4.1 — 512 точек,
//! §7.9.4.2 — пара 256-точечных при `blksw = 1`), шаги 1–5 спецификации.
//!
//! Результат — 512 взвешенных окном отсчётов `x[n]` блока; шаг 6
//! (overlap-add с хвостом прошлого блока и множитель 2) делает
//! `synthesis::overlap`, потому что хвост — состояние канала, а не
//! преобразования.
//!
//! Шаги повторяют псевдокод спецификации буквально: предповорот
//! `(X[a] + jX[b])·(xcos + j·xsin)`, комплексное обратное БПФ на N/4
//! (или N/8) точек, постповорот тем же множителем и раскладка с окном по
//! формулам шага 5. Медленная эталонная реализация (прямая сумма шага 3 в
//! f64) и проверка точного восстановления через прямое MDCT §8.2.3.2 —
//! в `synthesis::tests`.

use super::fft::{Complex, InverseFft};
use super::window::{HALF_WINDOW_LENGTH, kbd_half_window};
use crate::spectral::COEFFICIENTS_PER_BLOCK;
use crate::syntax::audblk::output::TransformLength;

/// N — длина длинного преобразования (§7.9.4.1).
pub(crate) const TRANSFORM_LENGTH: usize = 512;
/// N/4 — точек комплексного БПФ длинного преобразования.
const LONG_FFT_SIZE: usize = TRANSFORM_LENGTH / 4;
/// N/8 — точек БПФ каждого из коротких преобразований (§7.9.4.2).
const SHORT_FFT_SIZE: usize = TRANSFORM_LENGTH / 8;
/// N/8 и N/4 как смещения в формулах шага 5.
const EIGHTH: usize = TRANSFORM_LENGTH / 8;
const QUARTER: usize = TRANSFORM_LENGTH / 4;
const HALF: usize = TRANSFORM_LENGTH / 2;
/// Коэффициентов в каждом из двух коротких преобразований (`X1`, `X2`).
const SHORT_COEFFICIENTS: usize = COEFFICIENTS_PER_BLOCK / 2;

/// Взвешенный окном выход одного блока (`x[0..N]` шага 5).
pub(crate) type WindowedBlock = [f32; TRANSFORM_LENGTH];

/// Таблицы и рабочие буферы IMDCT; один экземпляр на декодер.
#[derive(Debug, Clone)]
pub(crate) struct Imdct {
    /// `w[0..256]` Таблицы 7.33 (KBD α = 5, см. `window`).
    window: [f32; HALF_WINDOW_LENGTH],
    /// `xcos1[k] + j·xsin1[k]` = `−e^{j·2π(8k+1)/(8N)}`, k < N/4.
    long_twiddles: [Complex; LONG_FFT_SIZE],
    /// `xcos2[k] + j·xsin2[k]` = `−e^{j·2π(8k+1)/(4N)}`, k < N/8.
    short_twiddles: [Complex; SHORT_FFT_SIZE],
    long_fft: InverseFft,
    short_fft: InverseFft,
    /// Рабочие буферы `Z`/`z`/`y` (для коротких — две половины по N/8).
    long_work: [Complex; LONG_FFT_SIZE],
    short_work: [[Complex; SHORT_FFT_SIZE]; 2],
}

/// `−e^{j·2π(8k+1)/divisor}` для k < `count` (множители шагов 2 и 4).
fn pre_post_twiddles<const COUNT: usize>(divisor: f64) -> [Complex; COUNT] {
    std::array::from_fn(|k| {
        let angle = 2.0 * std::f64::consts::PI * (8 * k + 1) as f64 / divisor;
        Complex::new(-angle.cos() as f32, -angle.sin() as f32)
    })
}

impl Imdct {
    pub(crate) fn new() -> Self {
        let window_f64 = kbd_half_window();
        Self {
            window: std::array::from_fn(|index| window_f64[index] as f32),
            long_twiddles: pre_post_twiddles((8 * TRANSFORM_LENGTH) as f64),
            short_twiddles: pre_post_twiddles((4 * TRANSFORM_LENGTH) as f64),
            long_fft: InverseFft::new(LONG_FFT_SIZE),
            short_fft: InverseFft::new(SHORT_FFT_SIZE),
            long_work: [Complex::ZERO; LONG_FFT_SIZE],
            short_work: [[Complex::ZERO; SHORT_FFT_SIZE]; 2],
        }
    }

    /// Обратное преобразование блока по `blksw` канала.
    pub(crate) fn inverse_transform(
        &mut self,
        length: TransformLength,
        coefficients: &[f32; COEFFICIENTS_PER_BLOCK],
        output: &mut WindowedBlock,
    ) {
        match length {
            TransformLength::Long => self.inverse_long(coefficients, output),
            TransformLength::ShortPair => self.inverse_short_pair(coefficients, output),
        }
    }

    /// §7.9.4.1: одно 512-точечное преобразование.
    fn inverse_long(
        &mut self,
        coefficients: &[f32; COEFFICIENTS_PER_BLOCK],
        output: &mut WindowedBlock,
    ) {
        // Шаг 2: Z[k] = (X[N/2−2k−1] + j·X[2k]) · (xcos1[k] + j·xsin1[k]).
        for (k, (slot, &twiddle)) in self
            .long_work
            .iter_mut()
            .zip(&self.long_twiddles)
            .enumerate()
        {
            let packed = Complex::new(coefficients[HALF - 2 * k - 1], coefficients[2 * k]);
            *slot = packed.mul(twiddle);
        }
        // Шаг 3: z = IDFT_{N/4}(Z).
        self.long_fft.transform_in_place(&mut self.long_work);
        // Шаг 4: y[n] = z[n] · (xcos1[n] + j·xsin1[n]).
        for (value, &twiddle) in self.long_work.iter_mut().zip(&self.long_twiddles) {
            *value = value.mul(twiddle);
        }
        // Шаг 5: раскладка с окном.
        let y = &self.long_work;
        let w = &self.window;
        for n in 0..EIGHTH {
            output[2 * n] = -y[EIGHTH + n].im * w[2 * n];
            output[2 * n + 1] = y[EIGHTH - n - 1].re * w[2 * n + 1];
            output[QUARTER + 2 * n] = -y[n].re * w[QUARTER + 2 * n];
            output[QUARTER + 2 * n + 1] = y[QUARTER - n - 1].im * w[QUARTER + 2 * n + 1];
            output[HALF + 2 * n] = -y[EIGHTH + n].re * w[HALF - 2 * n - 1];
            output[HALF + 2 * n + 1] = y[EIGHTH - n - 1].im * w[HALF - 2 * n - 2];
            output[3 * QUARTER + 2 * n] = y[n].im * w[QUARTER - 2 * n - 1];
            output[3 * QUARTER + 2 * n + 1] = -y[QUARTER - n - 1].re * w[QUARTER - 2 * n - 2];
        }
    }

    /// §7.9.4.2: два 256-точечных преобразования; коэффициенты чередуются
    /// (`X1[k] = X[2k]`, `X2[k] = X[2k+1]`).
    fn inverse_short_pair(
        &mut self,
        coefficients: &[f32; COEFFICIENTS_PER_BLOCK],
        output: &mut WindowedBlock,
    ) {
        for (half, work) in self.short_work.iter_mut().enumerate() {
            // Шаг 1: X1 — чётные бины, X2 — нечётные.
            let short = |index: usize| coefficients[2 * index + half];
            // Шаг 2: Z[k] = (X_h[N/4−2k−1] + j·X_h[2k]) · (xcos2[k] + j·xsin2[k]).
            for (k, (slot, &twiddle)) in work.iter_mut().zip(&self.short_twiddles).enumerate() {
                let packed = Complex::new(short(SHORT_COEFFICIENTS - 2 * k - 1), short(2 * k));
                *slot = packed.mul(twiddle);
            }
            // Шаг 3: z = IDFT_{N/8}(Z).
            self.short_fft.transform_in_place(work);
            // Шаг 4: y[n] = z[n] · (xcos2[n] + j·xsin2[n]).
            for (value, &twiddle) in work.iter_mut().zip(&self.short_twiddles) {
                *value = value.mul(twiddle);
            }
        }
        // Шаг 5: первое короткое преобразование заполняет x[0..N/2] (нарастающая
        // половина окна), второе — x[N/2..N] (спадающая половина).
        let [y1, y2] = &self.short_work;
        let w = &self.window;
        for n in 0..EIGHTH {
            output[2 * n] = -y1[n].im * w[2 * n];
            output[2 * n + 1] = y1[EIGHTH - n - 1].re * w[2 * n + 1];
            output[QUARTER + 2 * n] = -y1[n].re * w[QUARTER + 2 * n];
            output[QUARTER + 2 * n + 1] = y1[EIGHTH - n - 1].im * w[QUARTER + 2 * n + 1];
            output[HALF + 2 * n] = -y2[n].re * w[HALF - 2 * n - 1];
            output[HALF + 2 * n + 1] = y2[EIGHTH - n - 1].im * w[HALF - 2 * n - 2];
            output[3 * QUARTER + 2 * n] = y2[n].im * w[QUARTER - 2 * n - 1];
            output[3 * QUARTER + 2 * n + 1] = -y2[EIGHTH - n - 1].re * w[QUARTER - 2 * n - 2];
        }
    }
}
