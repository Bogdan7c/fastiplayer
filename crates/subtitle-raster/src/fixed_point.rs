//! Целочисленная арифметика с фиксированной точкой, на которой стоят метрики libass и FreeType.
//!
//! libass считает координаты глифов в формате 26.6 (шесть бит дробной части, 1/64 пикселя), а
//! масштаб шрифта хранит как 16.16. Чтобы результат совпадал с libass до последнего бита,
//! округление здесь повторяет `FT_MulFix`/`FT_DivFix` (FreeType) и `ass_lrint` (libass).
//! Код написан заново по описанию поведения; код FreeType не копировался.

use crate::outline::round_ties_even_to_i32;

/// Вектор в формате 26.6 (libass: `ASS_Vector`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Vector26 {
    /// Горизонталь, 1/64 пикселя.
    pub(crate) x: i32,
    /// Вертикаль, 1/64 пикселя (вниз).
    pub(crate) y: i32,
}

impl Vector26 {
    /// Прибавляет `other` с насыщением вместо переполнения.
    ///
    /// Позиция пера растёт с каждым глифом, а число глифов ограничено лимитом входа, который
    /// задаёт вызывающий и который может быть большим; насыщение гарантирует отсутствие паники
    /// и UB-подобного «заворота» знака при любом лимите (такая реплика получится заведомо
    /// за пределами холста и будет отсечена обрезкой по холсту).
    pub(crate) fn add_saturating(&mut self, other: Self) {
        self.x = self.x.saturating_add(other.x);
        self.y = self.y.saturating_add(other.y);
    }

    /// Сумма двух векторов с насыщением.
    pub(crate) fn plus_saturating(self, other: Self) -> Self {
        Self {
            x: self.x.saturating_add(other.x),
            y: self.y.saturating_add(other.y),
        }
    }
}

/// Умножение 16.16: `a * b / 65536`, округление к ближайшему, ничьи — от нуля
/// (поведение `FT_MulFix`).
pub(crate) fn mul_fix(a: i64, b: i64) -> i64 {
    let negative = (a < 0) != (b < 0);
    let magnitude = (a.unsigned_abs() * b.unsigned_abs() + 0x8000) >> 16;
    let magnitude = i64::try_from(magnitude).unwrap_or(i64::MAX);
    if negative { -magnitude } else { magnitude }
}

/// Деление в 16.16: `a * 65536 / b`, округление к ближайшему, ничьи — от нуля
/// (поведение `FT_DivFix`). Деление на ноль даёт 0: вызывающий проверяет делитель заранее.
pub(crate) fn div_fix(a: i64, b: i64) -> i64 {
    if b == 0 {
        return 0;
    }
    let negative = (a < 0) != (b < 0);
    let divisor = b.unsigned_abs();
    let magnitude = ((a.unsigned_abs() << 16) + divisor / 2) / divisor;
    let magnitude = i64::try_from(magnitude).unwrap_or(i64::MAX);
    if negative { -magnitude } else { magnitude }
}

/// Дробное число пикселей -> 26.6 (libass: `double_to_d6` = `lrint(x * 64)`).
pub(crate) fn double_to_d6(value: f64) -> i32 {
    round_ties_even_to_i32(value * 64.0)
}

/// 26.6 -> число пикселей (libass: `d6_to_double`).
pub(crate) fn d6_to_double(value: i32) -> f64 {
    f64::from(value) / 64.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_sums_saturate_instead_of_wrapping() {
        let mut pen = Vector26 {
            x: i32::MAX - 1,
            y: i32::MIN + 1,
        };
        pen.add_saturating(Vector26 { x: 10, y: -10 });
        assert_eq!(
            pen,
            Vector26 {
                x: i32::MAX,
                y: i32::MIN
            }
        );
        let sum = Vector26 { x: 1, y: 2 }.plus_saturating(Vector26 { x: 3, y: 4 });
        assert_eq!(sum, Vector26 { x: 4, y: 6 });
    }

    #[test]
    fn mul_fix_rounds_half_away_from_zero_like_freetype() {
        // 3 * 0.5 = 1.5 -> 2 (от нуля); -3 * 0.5 = -1.5 -> -2.
        assert_eq!(mul_fix(3, 0x8000), 2);
        assert_eq!(mul_fix(-3, 0x8000), -2);
        assert_eq!(mul_fix(3, -0x8000), -2);
        // 1124 * 706874 / 65536 = 12123.6 -> 12124 (значение из эталонного шрифта).
        assert_eq!(mul_fix(1124, 706_874), 12_124);
    }

    #[test]
    fn div_fix_matches_reference_scale_of_noto_sans() {
        // 256 px * 64 / (1124 + 395) в формате 16.16 (значение, замеренное у FreeType).
        assert_eq!(div_fix(256 * 64, 1124 + 395), 706_874);
        assert_eq!(div_fix(5, 0), 0);
        assert_eq!(div_fix(-1, 2), -0x8000);
    }

    #[test]
    fn d6_conversions_round_ties_to_even() {
        assert_eq!(double_to_d6(1.0), 64);
        // 0.5 / 64 = 0.0078125: ровно половина единицы 26.6 -> к чётному (0), а 1.5 -> 2.
        assert_eq!(double_to_d6(0.5 / 64.0), 0);
        assert_eq!(double_to_d6(1.5 / 64.0), 2);
        assert!((d6_to_double(96) - 1.5).abs() < f64::EPSILON);
    }
}
