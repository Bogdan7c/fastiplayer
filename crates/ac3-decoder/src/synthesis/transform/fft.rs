//! Комплексное обратное БПФ для шага 3 IMDCT (A/52:2012 §7.9.4.1–7.9.4.2).
//!
//! Спецификация задаёт шаг 3 прямой суммой
//! `z[n] = Σ_k Z[k]·(cos(2πkn/M) + j·sin(2πkn/M))` (M = 128 для 512-точечного
//! и M = 64 для 256-точечных преобразований, без нормировки 1/M). Здесь та же
//! сумма считается классическим итеративным алгоритмом Кули–Тьюки по
//! основанию 2 (перестановка с обращением бит + бабочки), что даёт тот же
//! результат за O(M·log M). Тест сверяет его с прямой суммой.

/// Комплексное число одинарной точности.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Complex {
    pub(crate) re: f32,
    pub(crate) im: f32,
}

impl Complex {
    pub(crate) const ZERO: Self = Self { re: 0.0, im: 0.0 };

    pub(crate) const fn new(re: f32, im: f32) -> Self {
        Self { re, im }
    }

    /// Произведение `self · other`.
    pub(crate) fn mul(self, other: Self) -> Self {
        Self {
            re: self.re * other.re - self.im * other.im,
            im: self.re * other.im + self.im * other.re,
        }
    }

    fn add(self, other: Self) -> Self {
        Self::new(self.re + other.re, self.im + other.im)
    }

    fn sub(self, other: Self) -> Self {
        Self::new(self.re - other.re, self.im - other.im)
    }
}

/// Обратное БПФ фиксированного размера M (степень двойки).
#[derive(Debug, Clone)]
pub(crate) struct InverseFft {
    /// `e^{+j·2πk/M}`, k = 0…M/2−1 (посчитаны в f64, округлены в f32).
    twiddles: Vec<Complex>,
    /// Индекс с обращённым порядком бит для каждой позиции.
    bit_reversed: Vec<usize>,
}

impl InverseFft {
    /// Готовит таблицы для размера `size` (должен быть степенью двойки ≥ 2;
    /// размеры фиксированы кодом: 128 и 64).
    pub(crate) fn new(size: usize) -> Self {
        debug_assert!(size.is_power_of_two() && size >= 2);
        let bits = size.trailing_zeros();
        let twiddles = (0..size / 2)
            .map(|k| {
                let angle = 2.0 * std::f64::consts::PI * k as f64 / size as f64;
                Complex::new(angle.cos() as f32, angle.sin() as f32)
            })
            .collect();
        let bit_reversed = (0..size)
            .map(|index| index.reverse_bits() >> (usize::BITS - bits))
            .collect();
        Self {
            twiddles,
            bit_reversed,
        }
    }

    /// Размер преобразования M.
    pub(crate) fn size(&self) -> usize {
        self.bit_reversed.len()
    }

    /// `values ← IDFT(values)` без нормировки. Длина `values` должна быть
    /// равна [`Self::size`]; иное — ошибка программы (размеры заданы
    /// константами вызывающего кода), лишние элементы не трогаются.
    pub(crate) fn transform_in_place(&self, values: &mut [Complex]) {
        let size = self.size();
        debug_assert_eq!(values.len(), size);
        if values.len() < size {
            return;
        }
        for (index, &reversed) in self.bit_reversed.iter().enumerate() {
            if index < reversed {
                values.swap(index, reversed);
            }
        }
        let mut half_span = 1;
        while half_span < size {
            // На этапе с бабочками длины 2·half_span нужен поворот
            // e^{+j·2π·m/(2·half_span)} = twiddles[m · size/(2·half_span)].
            let twiddle_stride = size / (2 * half_span);
            for group_start in (0..size).step_by(2 * half_span) {
                for offset in 0..half_span {
                    let twiddle = self.twiddles[offset * twiddle_stride];
                    let top = values[group_start + offset];
                    let bottom = values[group_start + offset + half_span].mul(twiddle);
                    values[group_start + offset] = top.add(bottom);
                    values[group_start + offset + half_span] = top.sub(bottom);
                }
            }
            half_span *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Complex, InverseFft};

    /// Прямая сумма шага 3 из §7.9.4.1 в f64.
    fn direct_inverse_dft(input: &[Complex]) -> Vec<(f64, f64)> {
        let size = input.len();
        (0..size)
            .map(|n| {
                input
                    .iter()
                    .enumerate()
                    .fold((0.0, 0.0), |(re, im), (k, value)| {
                        let angle = 2.0 * std::f64::consts::PI * (k * n) as f64 / size as f64;
                        let (sin, cos) = angle.sin_cos();
                        let (vr, vi) = (f64::from(value.re), f64::from(value.im));
                        (re + vr * cos - vi * sin, im + vr * sin + vi * cos)
                    })
            })
            .collect()
    }

    #[test]
    fn fast_transform_equals_direct_sum_for_ac3_sizes() {
        for size in [64, 128] {
            let fft = InverseFft::new(size);
            let input: Vec<Complex> = (0..size)
                .map(|index| {
                    let phase = index as f32 * 0.37;
                    Complex::new(phase.sin() * 0.8, (phase * 1.7).cos() * 0.5)
                })
                .collect();
            let expected = direct_inverse_dft(&input);
            let mut actual = input.clone();
            fft.transform_in_place(&mut actual);
            for (index, (value, (re, im))) in actual.iter().zip(&expected).enumerate() {
                assert!(
                    (f64::from(value.re) - re).abs() < 2e-5
                        && (f64::from(value.im) - im).abs() < 2e-5,
                    "size {size}, n {index}: {value:?} vs ({re}, {im})"
                );
            }
        }
    }

    #[test]
    fn impulse_at_bin_one_rotates_counterclockwise() {
        // Знак экспоненты «+j» (обратное преобразование): Z[1] = 1 → z[n] = e^{+j2πn/M}.
        let fft = InverseFft::new(64);
        let mut values = vec![Complex::ZERO; 64];
        values[1] = Complex::new(1.0, 0.0);
        fft.transform_in_place(&mut values);
        let quarter = values[16];
        assert!(quarter.re.abs() < 1e-6 && (quarter.im - 1.0).abs() < 1e-6);
    }
}
