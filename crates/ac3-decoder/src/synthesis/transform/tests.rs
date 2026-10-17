//! Проверки IMDCT против независимых источников истины:
//!
//! 1. медленная эталонная реализация — псевдокод §7.9.4.1/§7.9.4.2,
//!    перенесённый буквально (шаг 3 — прямая сумма), в f64;
//! 2. точное восстановление (TDAC): сигнал → прямое MDCT по определению
//!    §8.2.3.2 (кодер) → наш IMDCT + overlap-add §7.9.4 шаг 6 → тот же сигнал,
//!    в том числе при смене длинных и коротких блоков.

use super::imdct::{Imdct, TRANSFORM_LENGTH, WindowedBlock};
use super::window::kbd_half_window;
use crate::spectral::COEFFICIENTS_PER_BLOCK;
use crate::syntax::audblk::output::TransformLength;

const N: usize = TRANSFORM_LENGTH;
type Coefficients = [f32; COEFFICIENTS_PER_BLOCK];

/// Детерминированный псевдослучайный источник для тестов.
struct Lcg(u64);

impl Lcg {
    fn uniform(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

/// `w[n]` в f64.
fn window(index: usize) -> f64 {
    kbd_half_window()[index]
}

/// `(re, im)` · `−e^{j·2π(8k+1)/divisor}` — множители шагов 2 и 4.
fn rotate(value: (f64, f64), k: usize, divisor: f64) -> (f64, f64) {
    let angle = 2.0 * std::f64::consts::PI * (8 * k + 1) as f64 / divisor;
    let (c, s) = (-angle.cos(), -angle.sin());
    (value.0 * c - value.1 * s, value.1 * c + value.0 * s)
}

/// Прямая сумма шага 3: `z[n] = Σ Z[k]·e^{+j·2πkn/M}`.
fn direct_idft(input: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let size = input.len();
    (0..size)
        .map(|n| {
            input
                .iter()
                .enumerate()
                .fold((0.0, 0.0), |acc, (k, &(re, im))| {
                    let angle = 2.0 * std::f64::consts::PI * (k * n) as f64 / size as f64;
                    let (s, c) = angle.sin_cos();
                    (acc.0 + re * c - im * s, acc.1 + re * s + im * c)
                })
        })
        .collect()
}

/// §7.9.4.1 буквально (f64).
fn reference_long(x: &[f64]) -> Vec<f64> {
    let divisor = (8 * N) as f64;
    let z: Vec<_> = (0..N / 4)
        .map(|k| rotate((x[N / 2 - 2 * k - 1], x[2 * k]), k, divisor))
        .collect();
    let y: Vec<_> = direct_idft(&z)
        .into_iter()
        .enumerate()
        .map(|(n, value)| rotate(value, n, divisor))
        .collect();
    let (yr, yi) = (|n: usize| y[n].0, |n: usize| y[n].1);
    let mut out = vec![0.0; N];
    for n in 0..N / 8 {
        out[2 * n] = -yi(N / 8 + n) * window(2 * n);
        out[2 * n + 1] = yr(N / 8 - n - 1) * window(2 * n + 1);
        out[N / 4 + 2 * n] = -yr(n) * window(N / 4 + 2 * n);
        out[N / 4 + 2 * n + 1] = yi(N / 4 - n - 1) * window(N / 4 + 2 * n + 1);
        out[N / 2 + 2 * n] = -yr(N / 8 + n) * window(N / 2 - 2 * n - 1);
        out[N / 2 + 2 * n + 1] = yi(N / 8 - n - 1) * window(N / 2 - 2 * n - 2);
        out[3 * N / 4 + 2 * n] = yi(n) * window(N / 4 - 2 * n - 1);
        out[3 * N / 4 + 2 * n + 1] = -yr(N / 4 - n - 1) * window(N / 4 - 2 * n - 2);
    }
    out
}

/// §7.9.4.2 буквально (f64).
fn reference_short_pair(x: &[f64]) -> Vec<f64> {
    let divisor = (4 * N) as f64;
    let halves: Vec<Vec<(f64, f64)>> = (0..2)
        .map(|half| {
            let xh = |k: usize| x[2 * k + half];
            let z: Vec<_> = (0..N / 8)
                .map(|k| rotate((xh(N / 4 - 2 * k - 1), xh(2 * k)), k, divisor))
                .collect();
            direct_idft(&z)
                .into_iter()
                .enumerate()
                .map(|(n, value)| rotate(value, n, divisor))
                .collect()
        })
        .collect();
    let (y1, y2) = (&halves[0], &halves[1]);
    let mut out = vec![0.0; N];
    for n in 0..N / 8 {
        out[2 * n] = -y1[n].1 * window(2 * n);
        out[2 * n + 1] = y1[N / 8 - n - 1].0 * window(2 * n + 1);
        out[N / 4 + 2 * n] = -y1[n].0 * window(N / 4 + 2 * n);
        out[N / 4 + 2 * n + 1] = y1[N / 8 - n - 1].1 * window(N / 4 + 2 * n + 1);
        out[N / 2 + 2 * n] = -y2[n].0 * window(N / 2 - 2 * n - 1);
        out[N / 2 + 2 * n + 1] = y2[N / 8 - n - 1].1 * window(N / 2 - 2 * n - 2);
        out[3 * N / 4 + 2 * n] = y2[n].1 * window(N / 4 - 2 * n - 1);
        out[3 * N / 4 + 2 * n + 1] = -y2[N / 8 - n - 1].0 * window(N / 4 - 2 * n - 2);
    }
    out
}

fn random_coefficients(random: &mut Lcg) -> Coefficients {
    std::array::from_fn(|_| random.uniform() as f32)
}

fn fast(length: TransformLength, coefficients: &Coefficients) -> WindowedBlock {
    let mut output = [0.0; N];
    Imdct::new().inverse_transform(length, coefficients, &mut output);
    output
}

#[test]
fn fast_transforms_match_literal_spec_reference() {
    let mut random = Lcg(1);
    for _ in 0..8 {
        let coefficients = random_coefficients(&mut random);
        let as_f64: Vec<f64> = coefficients.iter().map(|&value| f64::from(value)).collect();
        for (length, expected) in [
            (TransformLength::Long, reference_long(&as_f64)),
            (TransformLength::ShortPair, reference_short_pair(&as_f64)),
        ] {
            let actual = fast(length, &coefficients);
            let peak = expected
                .iter()
                .fold(0.0f64, |peak, value| peak.max(value.abs()));
            let error = actual
                .iter()
                .zip(&expected)
                .fold(0.0f64, |worst, (&a, &e)| {
                    worst.max((f64::from(a) - e).abs())
                });
            // f32 против f64: ошибка порядка 1e-6 от пика (≈ 120 дБ).
            assert!(
                error <= peak * 2e-6,
                "{length:?}: error {error}, peak {peak}"
            );
        }
    }
}

/// Прямое MDCT по определению §8.2.3.2 для одного взвешенного сегмента.
fn forward_mdct(segment: &[f64], alpha: f64) -> Vec<f64> {
    let length = segment.len();
    (0..length / 2)
        .map(|k| {
            let sum: f64 = segment
                .iter()
                .enumerate()
                .map(|(n, &value)| {
                    let phase = 2.0 * std::f64::consts::PI / (4 * length) as f64
                        * ((2 * n + 1) * (2 * k + 1)) as f64
                        + std::f64::consts::PI / 4.0 * (2 * k + 1) as f64 * (1.0 + alpha);
                    value * phase.cos()
                })
                .sum();
            -2.0 / length as f64 * sum
        })
        .collect()
}

/// Кодер §8.2.3: окно 512 (Таблица 7.33 «спина к спине»), затем одно длинное
/// или два коротких преобразования с чередованием коэффициентов (§7.9.2).
fn encode_block(segment: &[f64], length: TransformLength) -> Coefficients {
    let windowed: Vec<f64> = segment
        .iter()
        .enumerate()
        .map(|(n, &value)| value * window(if n < N / 2 { n } else { N - 1 - n }))
        .collect();
    let mut coefficients = [0.0f32; COEFFICIENTS_PER_BLOCK];
    match length {
        TransformLength::Long => {
            for (slot, value) in coefficients.iter_mut().zip(forward_mdct(&windowed, 0.0)) {
                *slot = value as f32;
            }
        }
        TransformLength::ShortPair => {
            let first = forward_mdct(&windowed[..N / 2], -1.0);
            let second = forward_mdct(&windowed[N / 2..], 1.0);
            for k in 0..N / 4 {
                coefficients[2 * k] = first[k] as f32;
                coefficients[2 * k + 1] = second[k] as f32;
            }
        }
    }
    coefficients
}

#[test]
fn forward_then_inverse_reconstructs_signal_across_block_switching() {
    let mut random = Lcg(9);
    let pattern = [
        TransformLength::Long,
        TransformLength::Long,
        TransformLength::ShortPair,
        TransformLength::Long,
        TransformLength::ShortPair,
        TransformLength::ShortPair,
        TransformLength::Long,
        TransformLength::Long,
    ];
    let hop = N / 2;
    // Сигнал с нулями по краям: блок b кодирует отсчёты [hop·b, hop·b + N).
    let total = hop * (pattern.len() + 1);
    let signal: Vec<f64> = (0..total)
        .map(|index| {
            if index < hop || index >= total - hop {
                0.0
            } else {
                0.5 * random.uniform()
            }
        })
        .collect();
    let mut imdct = Imdct::new();
    let mut delay = [0.0f32; N / 2];
    let mut output = Vec::new();
    for (block, &length) in pattern.iter().enumerate() {
        let coefficients = encode_block(&signal[hop * block..hop * block + N], length);
        let mut windowed = [0.0f32; N];
        imdct.inverse_transform(length, &coefficients, &mut windowed);
        // Шаг 6 §7.9.4: pcm = 2·(x + delay), delay ← вторая половина.
        for n in 0..N / 2 {
            output.push(2.0 * (windowed[n] + delay[n]));
            delay[n] = windowed[N / 2 + n];
        }
    }
    // Выход блока b — отсчёты [hop·b, hop·(b+1)) исходного сигнала (блок 0
    // восстанавливает нулевой «разгон»), то есть задержки нет.
    for (index, (&decoded, &original)) in output.iter().zip(&signal).enumerate() {
        assert!(
            (f64::from(decoded) - original).abs() < 2e-6,
            "sample {index}: {decoded} vs {original}"
        );
    }
}
