// Порт из libass 0.17.5: ass_strtod.c (ass_strtod) и ass_utils.h (mystrtod).
//
// Copyright (c) 1988-1993 The Regents of the University of California.
// Copyright (c) 1994 Sun Microsystems, Inc.
// Copyright (c) 2016 Oleg Oshmyan <chortos@inbox.lv>
//
// Permission to use, copy, modify, and distribute this
// software and its documentation for any purpose and without
// fee is hereby granted, provided that the above copyright
// notice appear in all copies.  The University of California
// makes no representations about the suitability of this
// software for any purpose.  It is provided "as is" without
// express or implied warranty.
//
// Rust port © Fastiplayer contributors.

//! Разбор десятичных чисел в командах рисунка «как в libass».
//!
//! libass не доверяет `strtod` стандартной библиотеки: у него свой разбор (`ass_strtod`,
//! ветка Tcl) со своими правилами пробелов, знака, десятичной точки и экспоненты и со своей
//! цепочкой умножений на степени десятки. Координаты рисунков потом переводятся в 26.6 и
//! округляются, поэтому порт повторяет и сами правила, и порядок операций: так совпадают не
//! только «обычные» числа, но и странные вроде `5e`, `.5`, `-.` или `1e999`.

/// Наибольший десятичный порядок, который ещё имеет смысл (libass: `maxExponent`): всё, что
/// больше, уже даёт переполнение или исчезновение числа.
const MAX_EXPONENT: usize = 511;

/// Наибольший десятичный порядок обычного `double` (C: `DBL_MAX_10_EXP`).
const DOUBLE_MAX_10_EXPONENT: usize = 308;

/// Порядок, начиная с которого денормализованный результат требует отдельной подгонки
/// мантиссы (C: `-DBL_MIN_10_EXP`).
const DOUBLE_MIN_10_EXPONENT_ABS: usize = 307;

/// Максимум значащих цифр мантиссы: остальные не влияют на результат (libass: 18).
const MAX_MANTISSA_DIGITS: usize = 18;

/// Степени десятки `10^(2^i)` (libass: `powersOf10`).
const POWERS_OF_10: [f64; 9] = [
    10.0, 100.0, 1.0e4, 1.0e8, 1.0e16, 1.0e32, 1.0e64, 1.0e128, 1.0e256,
];

/// Отрицательные степени десятки `10^-(2^i)` (libass: `negPowOf10`).
const NEGATIVE_POWERS_OF_10: [f64; 9] = [
    0.1, 0.01, 1.0e-4, 1.0e-8, 1.0e-16, 1.0e-32, 1.0e-64, 1.0e-128, 1.0e-256,
];

/// Байт по индексу; за концом строки — 0, как терминатор C-строки.
fn byte_at(text: &[u8], index: usize) -> u8 {
    text.get(index).copied().unwrap_or(0)
}

/// Пробельный символ libass (`ass_isspace`).
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Читает число, начиная с позиции `start` (libass: `mystrtod`).
///
/// Возвращает значение и позицию за последним символом числа. `None` — числа нет (нет ни
/// одной цифры): позиция не двигается, как у `mystrtod`, где конец совпадает с началом.
pub(super) fn parse_number(text: &[u8], start: usize) -> Option<(f64, usize)> {
    let mut pos = start;
    while is_space(byte_at(text, pos)) {
        pos += 1;
    }
    let negative = match byte_at(text, pos) {
        b'-' => {
            pos += 1;
            true
        }
        b'+' => {
            pos += 1;
            false
        }
        _ => false,
    };

    let mantissa = scan_mantissa(text, pos);
    // Позиция сразу за мантиссой (C: `pExp`) — там может стоять экспонента.
    let after_mantissa = mantissa.end;
    let shape = mantissa.shape()?;
    let fraction = read_fraction(text, &shape);
    let (value, end) = apply_exponent(text, after_mantissa, fraction, &shape);
    Some((if negative { -value } else { value }, end))
}

/// Результат первого прохода по мантиссе: сколько в ней символов и где точка.
struct ScannedMantissa {
    /// Число символов мантиссы вместе с десятичной точкой (C: `mantSize`).
    size: usize,
    /// Индекс десятичной точки среди символов мантиссы (C: `decPt`).
    point: Option<usize>,
    /// Индекс первой ненулевой цифры (C: `leadZeros`, число ведущих нулей).
    first_nonzero: Option<usize>,
    /// Позиция в тексте за мантиссой.
    end: usize,
}

/// Считает символы мантиссы (цифры и одну точку) и находит точку и первую ненулевую цифру.
fn scan_mantissa(text: &[u8], start: usize) -> ScannedMantissa {
    let mut pos = start;
    let mut size = 0_usize;
    let mut point = None;
    let mut first_nonzero = None;
    loop {
        let byte = byte_at(text, pos);
        if byte.is_ascii_digit() {
            if byte != b'0' && first_nonzero.is_none() {
                first_nonzero = Some(size);
            }
        } else if byte == b'.' && point.is_none() {
            point = Some(size);
        } else {
            break;
        }
        pos += 1;
        size += 1;
    }
    ScannedMantissa {
        size,
        point,
        first_nonzero,
        end: pos,
    }
}

/// Форма числа после нормализации: сколько цифр читать и какой порядок у дробной части.
struct NumberShape {
    /// Сколько значащих цифр читать (C: `mantSize` после вычитания ведущих нулей).
    digits: usize,
    /// Позиция первой читаемой цифры (или точки перед ней), C: `p -= mantSize - leadZeros`.
    digits_start: usize,
    /// Порядок, который даёт дробная часть: знак «минус» и величина (C: `fracExp`).
    fraction_exponent: usize,
    /// Дробный порядок отрицателен (цифры после точки).
    fraction_exponent_negative: bool,
}

impl ScannedMantissa {
    /// Нормализует мантиссу; `None` — в ней нет ни цифр (число не прочитано).
    fn shape(&self) -> Option<NumberShape> {
        let mut size = self.size;
        let mut leading = self.first_nonzero.unwrap_or(size);
        let digits_start = self.end - size.saturating_sub(leading);
        let point = match self.point {
            None => size,
            Some(point) => {
                // Одним из символов была сама точка.
                size = size.saturating_sub(1);
                if point < leading {
                    leading = leading.saturating_sub(1);
                }
                point
            }
        };
        if size.saturating_sub(leading) > MAX_MANTISSA_DIGITS {
            size = leading + MAX_MANTISSA_DIGITS;
        }
        let (fraction_exponent_negative, fraction_exponent) = if point < size {
            (true, size - point)
        } else {
            (false, point - size)
        };
        if size == 0 {
            return None;
        }
        Some(NumberShape {
            digits: size - leading,
            digits_start,
            fraction_exponent,
            fraction_exponent_negative,
        })
    }
}

/// Собирает целое значение значащих цифр мантиссы в `double` (две порции по 9 цифр).
fn read_fraction(text: &[u8], shape: &NumberShape) -> f64 {
    let mut pos = shape.digits_start;
    let mut remaining = shape.digits;
    let next_digit = |pos: &mut usize| -> i32 {
        let mut byte = byte_at(text, *pos);
        *pos += 1;
        if byte == b'.' {
            byte = byte_at(text, *pos);
            *pos += 1;
        }
        i32::from(byte) - i32::from(b'0')
    };
    let mut high = 0_i32;
    while remaining > 9 {
        high = 10 * high + next_digit(&mut pos);
        remaining -= 1;
    }
    let mut low = 0_i32;
    while remaining > 0 {
        low = 10 * low + next_digit(&mut pos);
        remaining -= 1;
    }
    1.0e9 * f64::from(high) + f64::from(low)
}

/// Читает экспоненту (если есть) и домножает дробь на `10^порядок` теми же шагами, что libass.
fn apply_exponent(
    text: &[u8],
    after_mantissa: usize,
    fraction: f64,
    shape: &NumberShape,
) -> (f64, usize) {
    let mut pos = after_mantissa;
    let mut exponent;
    let mut exponent_negative;
    let mut overflowed = false;
    if matches!(byte_at(text, pos), b'E' | b'e') {
        pos += 1;
        exponent_negative = match byte_at(text, pos) {
            b'-' => {
                pos += 1;
                true
            }
            b'+' => {
                pos += 1;
                false
            }
            _ => false,
        };
        let same_sign = exponent_negative == shape.fraction_exponent_negative;
        let limit = if same_sign {
            if MAX_EXPONENT < shape.fraction_exponent {
                0
            } else {
                (MAX_EXPONENT - shape.fraction_exponent) / 10
            }
        } else {
            shape.fraction_exponent / 10 + (shape.fraction_exponent % 10 + MAX_EXPONENT) / 10
        };
        exponent = 0_usize;
        let mut wrapped = false;
        while byte_at(text, pos).is_ascii_digit() {
            let digit = usize::from(byte_at(text, pos) - b'0');
            if exponent > limit || wrapped {
                // Порядок заведомо слишком велик: дочитываем цифры и сдаёмся.
                while byte_at(text, pos).is_ascii_digit() {
                    pos += 1;
                }
                overflowed = true;
                break;
            } else if exponent > (usize::MAX - digit) / 10 {
                wrapped = true;
            }
            exponent = exponent.wrapping_mul(10).wrapping_add(digit);
            pos += 1;
        }
        if !overflowed {
            if same_sign {
                exponent = shape.fraction_exponent.wrapping_add(exponent);
            } else if shape.fraction_exponent <= exponent || wrapped {
                exponent = exponent.wrapping_sub(shape.fraction_exponent);
            } else {
                exponent = shape.fraction_exponent - exponent;
                exponent_negative = shape.fraction_exponent_negative;
            }
        }
    } else {
        exponent = shape.fraction_exponent;
        exponent_negative = shape.fraction_exponent_negative;
    }
    if overflowed || exponent > MAX_EXPONENT {
        exponent = MAX_EXPONENT;
    }
    (
        scale_by_power_of_ten(fraction, exponent, exponent_negative, shape.digits),
        pos,
    )
}

/// Умножает (делит) `fraction` на `10^exponent` по двоичным степеням, как libass.
fn scale_by_power_of_ten(
    mut fraction: f64,
    mut exponent: usize,
    mut negative: bool,
    mut digits: usize,
) -> f64 {
    let table: &[f64; 9];
    // Положительные степени точнее, поэтому они предпочтительны.
    if exponent <= DOUBLE_MAX_10_EXPONENT || !negative {
        table = &POWERS_OF_10;
    } else {
        // Результат может быть денормализованным: сначала сдвигаем дробь ниже единицы.
        if exponent > DOUBLE_MIN_10_EXPONENT_ABS {
            exponent -= digits;
            let mut divisor = 1.0;
            let mut index = 0;
            while digits != 0 {
                if digits & 1 == 1 {
                    divisor *= power(&POWERS_OF_10, index);
                }
                digits >>= 1;
                index += 1;
            }
            fraction /= divisor;
        }
        table = &NEGATIVE_POWERS_OF_10;
        negative = false;
    }
    let mut multiplier = 1.0;
    let mut index = 0;
    while exponent != 0 {
        if exponent & 1 == 1 {
            multiplier *= power(table, index);
        }
        exponent >>= 1;
        index += 1;
    }
    if negative {
        fraction / multiplier
    } else {
        fraction * multiplier
    }
}

/// Элемент таблицы степеней. Индекс не выходит за таблицу: порядок ограничен `MAX_EXPONENT`
/// (9 двоичных разрядов), а число цифр — 18 (5 разрядов); при нарушении множитель нейтрален.
fn power(table: &[f64; 9], index: usize) -> f64 {
    table.get(index).copied().unwrap_or(1.0)
}

#[cfg(test)]
mod tests;
