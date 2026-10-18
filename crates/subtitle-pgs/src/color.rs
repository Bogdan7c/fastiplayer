//! Перевод цвета палитры PGS: YCbCr (limited range) в RGB.

/// Матрица YCbCr -> RGB.
///
/// Спецификация Blu-ray привязывает матрицу палитры к формату видео диска: SD-форматы
/// (высота до 576 строк включительно) используют BT.601, HD (720/1080) — BT.709.
/// Поэтому матрица выбирается по высоте системы координат из PCS
/// ([`PgsColorMatrix::for_reference_height`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgsColorMatrix {
    Bt601,
    Bt709,
}

impl PgsColorMatrix {
    /// Наибольшая высота кадра, для которой применяется BT.601.
    const MAX_BT601_HEIGHT: u32 = 576;

    /// Выбор матрицы по высоте опорного размера: `height <= 576` -> BT.601, иначе BT.709.
    pub const fn for_reference_height(height: u32) -> Self {
        if height <= Self::MAX_BT601_HEIGHT {
            Self::Bt601
        } else {
            Self::Bt709
        }
    }

    /// Коэффициенты `(Cr->R, Cb->G, Cr->G, Cb->B)` для limited range (Y 16..235, C 16..240).
    const fn coefficients(self) -> (f32, f32, f32, f32) {
        match self {
            Self::Bt601 => (1.596, 0.392, 0.813, 2.017),
            Self::Bt709 => (1.793, 0.213, 0.533, 2.112),
        }
    }

    /// Прямое преобразование RGB -> YCbCr (limited range) для тестового кодировщика.
    /// Возвращает `(Y, Cb, Cr)`.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn rgb_to_ycbcr(self, red: u8, green: u8, blue: u8) -> (u8, u8, u8) {
        let (r, g, b) = (f32::from(red), f32::from(green), f32::from(blue));
        let ((yr, yg, yb), (cbr, cbg), (crg, crb)) = match self {
            Self::Bt601 => (
                (65.481, 128.553, 24.966),
                (-37.797, -74.203),
                (-93.786, -18.214),
            ),
            Self::Bt709 => (
                (46.559, 156.629, 15.812),
                (-25.664, -86.336),
                (-101.730, -10.270),
            ),
        };
        let luma = 16.0 + (yr * r + yg * g + yb * b) / 255.0;
        let cb = 128.0 + (cbr * r + cbg * g + 112.0 * b) / 255.0;
        let cr = 128.0 + (112.0 * r + crg * g + crb * b) / 255.0;
        (clamp_to_u8(luma), clamp_to_u8(cb), clamp_to_u8(cr))
    }
}

/// Округляет и ограничивает значение диапазоном байта.
fn clamp_to_u8(value: f32) -> u8 {
    // Значение уже зажато в 0..=255, приведение не теряет данных.
    value.round().clamp(0.0, 255.0) as u8
}

/// YCbCr (limited range) -> RGB8 с зажимом результата в 0..=255.
/// Значения Y/C вне номинального диапазона допустимы: они просто зажимаются на выходе.
pub fn ycbcr_to_rgb(matrix: PgsColorMatrix, luma: u8, cb: u8, cr: u8) -> [u8; 3] {
    let (cr_to_red, cb_to_green, cr_to_green, cb_to_blue) = matrix.coefficients();
    let scaled_luma = 1.164 * (f32::from(luma) - 16.0);
    let cb_centered = f32::from(cb) - 128.0;
    let cr_centered = f32::from(cr) - 128.0;
    [
        clamp_to_u8(scaled_luma + cr_to_red * cr_centered),
        clamp_to_u8(scaled_luma - cb_to_green * cb_centered - cr_to_green * cr_centered),
        clamp_to_u8(scaled_luma + cb_to_blue * cb_centered),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_choice_switches_after_576_lines() {
        assert_eq!(
            PgsColorMatrix::for_reference_height(480),
            PgsColorMatrix::Bt601
        );
        assert_eq!(
            PgsColorMatrix::for_reference_height(576),
            PgsColorMatrix::Bt601
        );
        assert_eq!(
            PgsColorMatrix::for_reference_height(577),
            PgsColorMatrix::Bt709
        );
        assert_eq!(
            PgsColorMatrix::for_reference_height(1080),
            PgsColorMatrix::Bt709
        );
    }

    #[test]
    fn limited_range_black_and_white() {
        for matrix in [PgsColorMatrix::Bt601, PgsColorMatrix::Bt709] {
            assert_eq!(ycbcr_to_rgb(matrix, 16, 128, 128), [0, 0, 0]);
            assert_eq!(ycbcr_to_rgb(matrix, 235, 128, 128), [255, 255, 255]);
            // Вне диапазона — зажим, а не переполнение.
            assert_eq!(ycbcr_to_rgb(matrix, 0, 128, 128), [0, 0, 0]);
            assert_eq!(ycbcr_to_rgb(matrix, 255, 128, 128), [255, 255, 255]);
        }
    }

    #[test]
    fn known_red_differs_between_matrices() {
        // Красный 255,0,0: BT.601 -> (81, 90, 240), BT.709 -> (63, 102, 240).
        let red_601 = ycbcr_to_rgb(PgsColorMatrix::Bt601, 81, 90, 240);
        assert!(
            red_601[0] >= 253 && red_601[1] <= 2 && red_601[2] <= 2,
            "{red_601:?}"
        );
        let red_709 = ycbcr_to_rgb(PgsColorMatrix::Bt709, 63, 102, 240);
        assert!(
            red_709[0] >= 253 && red_709[1] <= 2 && red_709[2] <= 2,
            "{red_709:?}"
        );
        // Те же YCbCr, прочитанные «чужой» матрицей, заметно уходят от красного.
        let wrong = ycbcr_to_rgb(PgsColorMatrix::Bt601, 63, 102, 240);
        assert!(wrong[1] > 10 || wrong[0] < 245, "{wrong:?}");
    }

    #[test]
    fn rgb_roundtrip_stays_within_two_levels() {
        for matrix in [PgsColorMatrix::Bt601, PgsColorMatrix::Bt709] {
            for rgb in [
                [200u8, 30, 90],
                [10, 220, 40],
                [20, 40, 230],
                [128, 128, 128],
            ] {
                let (y, cb, cr) = matrix.rgb_to_ycbcr(rgb[0], rgb[1], rgb[2]);
                let back = ycbcr_to_rgb(matrix, y, cb, cr);
                for channel in 0..3 {
                    assert!(
                        back[channel].abs_diff(rgb[channel]) <= 3,
                        "{matrix:?} {rgb:?} -> {back:?}"
                    );
                }
            }
        }
    }
}
