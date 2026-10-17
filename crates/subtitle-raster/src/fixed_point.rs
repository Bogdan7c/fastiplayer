//! Целочисленная арифметика с фиксированной точкой, на которой стоят метрики libass и FreeType.
//!
//! libass считает координаты глифов в формате 26.6 (шесть бит дробной части, 1/64 пикселя), а
//! масштаб шрифта хранит как 16.16. Чтобы результат совпадал с libass до последнего бита,
//! округление здесь повторяет `FT_MulFix`/`FT_DivFix` (FreeType) и `ass_lrint` (libass).
//! Код написан заново по описанию поведения; код FreeType не копировался.

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

/// Дробное число пикселей -> 26.6 (libass: `double_to_d6`, `ass_utils.h`).
///
/// В libass это `int double_to_d6(double x) { return ass_lrint(x * 64); }`, а `ass_lrint` -
/// `long lrint(double)`. Значит результат: округление к ближайшему (ничьи к чётному) в
/// 64-битное целое, затем усечение до 32 бит младшими разрядами (тип возврата `int`; на
/// практике gcc/clang на x86-64 именно так и делают). Поэтому большие значения ЗАВОРАЧИВАЮТСЯ
/// по модулю 2^32, а не насыщаются, а NaN, бесконечность и всё, что не помещается в 64 бита,
/// дают 0: `lrint` по инструкции `cvtsd2si` возвращает в этих случаях `i64::MIN`
/// (0x8000000000000000), младшие 32 бита которого нулевые. Рисунок с `1e999` вместо числа
/// поэтому получает координату 0, а не огромную. Повторено ради совпадения с libass
/// на x86-64 (где снят эталон); значения на других архитектурах у самой libass не определены.
pub(crate) fn double_to_d6(value: f64) -> i32 {
    // Нижняя граница включительно, верхняя (2^63) исключена: ровно диапазон `i64`.
    const I64_RANGE: std::ops::Range<f64> =
        -9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0;
    let rounded = (value * 64.0).round_ties_even();
    if !I64_RANGE.contains(&rounded) {
        // NaN тоже сюда: `contains` для NaN ложно.
        return 0;
    }
    // `as i64` точно (значение в диапазоне и целое), `as i32` отбрасывает старшие 32 бита.
    (rounded as i64) as i32
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

    #[test]
    fn double_to_d6_wraps_like_libass_lrint_truncated_to_int() {
        // Значения сняты с живого C (`int f(double x) { return lrint(x * 64); }`, gcc, x86-64).
        // Влезает в i64, не влезает в i32: младшие 32 бита (заворот, не насыщение).
        assert_eq!(double_to_d6(99_999_999_999.0), 498_728_896);
        assert_eq!(double_to_d6(1.0e10), 49_872_896);
        assert_eq!(double_to_d6(-1.0e10), -49_872_896);
        assert_eq!(double_to_d6(46_875_000.0), -1_294_967_296);
        // Не влезает в i64, бесконечность и NaN: `cvtsd2si` даёт i64::MIN, младшие биты - 0.
        assert_eq!(double_to_d6(1.0e18), 0);
        assert_eq!(double_to_d6(f64::INFINITY), 0);
        assert_eq!(double_to_d6(f64::NEG_INFINITY), 0);
        assert_eq!(double_to_d6(f64::NAN), 0);
    }
}
