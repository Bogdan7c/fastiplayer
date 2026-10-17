//! Обратное DCT-II AHT по шести блокам кадра (A/52 Annex E §3.4.5;
//! ETSI TS 102 366 v1.4.1 §E.2.4.5).
//!
//! `C(k, m) = √2 · Σ_{j=0..5} R_j · X(k, j) · cos(j·(2m + 1)·π / 12)`,
//! `R_0 = 1/√2`, `R_j = 1` при `j ≠ 0`; `X(k, j)` — коэффициенты AHT бина `k`,
//! `C(k, m)` — мантисса MDCT бина `k` в блоке `m` (до экспоненты).

use crate::tables::eac3::vq::VQ_DIMENSION;

/// Матрица `M[m][j] = √2 · R_j · cos(j(2m+1)π/12)`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InverseAhtTransform {
    matrix: [[f32; VQ_DIMENSION]; VQ_DIMENSION],
}

impl InverseAhtTransform {
    pub(crate) fn new() -> Self {
        let mut matrix = [[0.0f32; VQ_DIMENSION]; VQ_DIMENSION];
        for (block, row) in matrix.iter_mut().enumerate() {
            for (index, value) in row.iter_mut().enumerate() {
                let normalization = if index == 0 {
                    std::f64::consts::FRAC_1_SQRT_2
                } else {
                    1.0
                };
                let angle = (index * (2 * block + 1)) as f64 * std::f64::consts::PI / 12.0;
                *value = (std::f64::consts::SQRT_2 * normalization * angle.cos()) as f32;
            }
        }
        Self { matrix }
    }

    /// Переводит шесть коэффициентов AHT одного бина в шесть мантисс блоков.
    pub(crate) fn apply(&self, aht: &[f32; VQ_DIMENSION]) -> [f32; VQ_DIMENSION] {
        self.matrix
            .map(|row| row.iter().zip(aht).map(|(m, x)| m * x).sum())
    }
}

#[cfg(test)]
mod tests {
    use super::InverseAhtTransform;

    #[test]
    fn dc_coefficient_spreads_evenly_over_blocks() {
        let transform = InverseAhtTransform::new();
        let blocks = transform.apply(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert!(blocks.iter().all(|&value| (value - 1.0).abs() < 1e-6));
    }

    /// Матрица — √6 × ортонормированное DCT-II: `MᵀM = 6·I`, поэтому энергия
    /// сохраняется с множителем 6 и обратное преобразование однозначно.
    #[test]
    fn transform_is_scaled_orthonormal_dct() {
        let transform = InverseAhtTransform::new();
        for first in 0..6 {
            for second in 0..6 {
                let dot: f32 = (0..6)
                    .map(|block| transform.matrix[block][first] * transform.matrix[block][second])
                    .sum();
                let expected = if first == second { 6.0 } else { 0.0 };
                assert!((dot - expected).abs() < 1e-5, "{first} {second}: {dot}");
            }
        }
        // Чистая гармоника j = 3: cos(3(2m+1)π/12) = cos((2m+1)π/4) → ±1 с шагом √2.
        let blocks = transform.apply(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        let half = std::f32::consts::FRAC_1_SQRT_2 * std::f32::consts::SQRT_2;
        assert!((blocks[0] - half).abs() < 1e-6);
        assert!((blocks[1] + half).abs() < 1e-6);
    }
}
