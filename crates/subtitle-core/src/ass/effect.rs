//! Переходные эффекты поля `Effect` (`Banner`, `Scroll up`, `Scroll down`).
//!
//! Порт разбора из `ass_apply_transition_effects` (libass 0.17.5, ass_parse.c): сравнение
//! префиксов с учётом регистра, числа читаются `atoi` после каждой `;` (не больше четырёх),
//! нехватка чисел делает эффект игнорируемым. Расчёт позиции и клипа — в растеризаторе.

/// Направление бегущей строки `Banner`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssBannerDirection {
    /// Текст въезжает справа налево (по умолчанию).
    RightToLeft,
    /// Текст едет слева направо.
    LeftToRight,
}

/// Направление вертикальной прокрутки `Scroll up` / `Scroll down`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssScrollDirection {
    /// `Scroll up;` — снизу вверх.
    Up,
    /// `Scroll down;` — сверху вниз.
    Down,
}

/// Разобранное поле `Effect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssEffect {
    /// Пусто, неизвестный эффект или не хватает чисел (libass пишет в лог и игнорирует поле).
    None,
    /// `Banner;delay[;lefttoright[;fadeawayheight]]` (`fadeawayheight` libass игнорирует).
    Banner {
        /// Миллисекунды на пиксель PlayRes «как написано»; пересчёт и `max(1)` — в растеризаторе.
        delay_ms: i32,
        direction: AssBannerDirection,
    },
    /// `Scroll up|down;y1;y2;delay[;fadeawayheight]`.
    Scroll {
        direction: AssScrollDirection,
        /// Верхняя граница области прокрутки в PlayRes (libass упорядочивает `min`/`max`).
        y_top: i32,
        /// Нижняя граница области прокрутки в PlayRes.
        y_bottom: i32,
        /// Миллисекунды на пиксель PlayRes «как написано».
        delay_ms: i32,
    },
}

/// Сколько чисел после `;` читает libass (`int v[4]`).
const MAX_EFFECT_NUMBERS: usize = 4;

impl AssEffect {
    /// Разбирает сырое поле `Effect`.
    pub fn parse(raw: &str) -> Self {
        let numbers = effect_numbers(raw);
        if raw.starts_with("Banner;") {
            // После префикса `;` уже есть, значит хотя бы одно число найдено всегда.
            let Some(delay_ms) = numbers.first().copied() else {
                return Self::None;
            };
            let left_to_right = numbers.get(1).is_some_and(|value| *value != 0);
            return Self::Banner {
                delay_ms,
                direction: if left_to_right {
                    AssBannerDirection::LeftToRight
                } else {
                    AssBannerDirection::RightToLeft
                },
            };
        }
        let direction = if raw.starts_with("Scroll up;") {
            AssScrollDirection::Up
        } else if raw.starts_with("Scroll down;") {
            AssScrollDirection::Down
        } else {
            return Self::None;
        };
        let (Some(&first), Some(&second), Some(&delay_ms)) =
            (numbers.first(), numbers.get(1), numbers.get(2))
        else {
            return Self::None;
        };
        Self::Scroll {
            direction,
            y_top: first.min(second),
            y_bottom: first.max(second),
            delay_ms,
        }
    }
}

/// Числа после каждой `;` (не больше [`MAX_EFFECT_NUMBERS`]): `strchr(p, ';')` + `atoi(++p)`.
fn effect_numbers(raw: &str) -> Vec<i32> {
    raw.split(';')
        .skip(1)
        .take(MAX_EFFECT_NUMBERS)
        .map(c_atoi)
        .collect()
}

/// C `atoi` на glibc: пробелы, знак, десятичные цифры; результат `strtol` (64 бита, с
/// насыщением) усекается до `int`, мусор даёт 0.
fn c_atoi(text: &str) -> i32 {
    let bytes = text.as_bytes();
    let mut pos = bytes
        .iter()
        .take_while(|byte| matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'))
        .count();
    let negative = match bytes.get(pos) {
        Some(b'-') => {
            pos += 1;
            true
        }
        Some(b'+') => {
            pos += 1;
            false
        }
        _ => false,
    };
    // Модуль копим в i128 с насыщением, чтобы знак применялся к ненасыщенному числу.
    let mut magnitude: i128 = 0;
    for byte in bytes.get(pos..).unwrap_or_default() {
        if !byte.is_ascii_digit() {
            break;
        }
        magnitude = magnitude
            .saturating_mul(10)
            .saturating_add(i128::from(byte - b'0'))
            .min(i128::from(u64::MAX));
    }
    let signed = if negative { -magnitude } else { magnitude };
    // `strtol` при выходе за диапазон `long` возвращает `LONG_MAX` / `LONG_MIN` (насыщение
    // с учётом знака), поэтому зажимаем знаковое значение, а не модуль.
    let value = signed.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
    // Усечение 64 -> 32 бита намеренное: так делает `(int) strtol(...)`.
    value as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_reads_delay_and_direction() {
        assert_eq!(
            AssEffect::parse("Banner;50"),
            AssEffect::Banner {
                delay_ms: 50,
                direction: AssBannerDirection::RightToLeft
            }
        );
        assert_eq!(
            AssEffect::parse("Banner;25;1;80"),
            AssEffect::Banner {
                delay_ms: 25,
                direction: AssBannerDirection::LeftToRight
            }
        );
        // Любое ненулевое число — слева направо; ноль и мусор — справа налево.
        assert_eq!(
            AssEffect::parse("Banner;25;-3"),
            AssEffect::Banner {
                delay_ms: 25,
                direction: AssBannerDirection::LeftToRight
            }
        );
        assert_eq!(
            AssEffect::parse("Banner;25;abc"),
            AssEffect::Banner {
                delay_ms: 25,
                direction: AssBannerDirection::RightToLeft
            }
        );
    }

    #[test]
    fn banner_without_number_text_still_counts_as_zero_delay() {
        // `;` есть, значит cnt >= 1; atoi пустой строки даёт 0 (минимум 1 поднимает растеризатор).
        assert_eq!(
            AssEffect::parse("Banner;"),
            AssEffect::Banner {
                delay_ms: 0,
                direction: AssBannerDirection::RightToLeft
            }
        );
    }

    #[test]
    fn scroll_orders_bounds_and_reads_delay() {
        assert_eq!(
            AssEffect::parse("Scroll up;100;20;5;0"),
            AssEffect::Scroll {
                direction: AssScrollDirection::Up,
                y_top: 20,
                y_bottom: 100,
                delay_ms: 5
            }
        );
        assert_eq!(
            AssEffect::parse("Scroll down;10;300;7"),
            AssEffect::Scroll {
                direction: AssScrollDirection::Down,
                y_top: 10,
                y_bottom: 300,
                delay_ms: 7
            }
        );
    }

    #[test]
    fn scroll_needs_three_numbers() {
        assert_eq!(AssEffect::parse("Scroll up;10;20"), AssEffect::None);
        assert_eq!(AssEffect::parse("Scroll down;"), AssEffect::None);
        // Третье число есть (пусть и пустое): cnt == 3, delay = atoi("") = 0.
        assert_eq!(
            AssEffect::parse("Scroll up;10;20;"),
            AssEffect::Scroll {
                direction: AssScrollDirection::Up,
                y_top: 10,
                y_bottom: 20,
                delay_ms: 0
            }
        );
    }

    #[test]
    fn matching_is_case_and_prefix_sensitive() {
        for raw in [
            "",
            "banner;50",
            "Banner",
            "Banner 50",
            " Banner;50",
            "scroll up;1;2;3",
            "Scroll Up;1;2;3",
            "Scroll up 1;2;3;4",
            "Scrollup;1;2;3",
            "Karaoke;1",
            "garbage;1;2;3;4",
        ] {
            assert_eq!(AssEffect::parse(raw), AssEffect::None, "{raw:?}");
        }
    }

    #[test]
    fn numbers_follow_atoi_rules() {
        // Пробелы и плюс допустимы, хвост после цифр игнорируется.
        assert_eq!(
            AssEffect::parse("Scroll up; +5x; -7 ;  9"),
            AssEffect::Scroll {
                direction: AssScrollDirection::Up,
                y_top: -7,
                y_bottom: 5,
                delay_ms: 9
            }
        );
        // 4294967297 = 2^32 + 1: strtol даёт long, приведение к int оставляет 1.
        assert_eq!(c_atoi("4294967297"), 1);
        assert_eq!(c_atoi("-2147483648"), i32::MIN);
        assert_eq!(c_atoi("1e3"), 1);
        assert_eq!(c_atoi("--1"), 0);
        // strtol насыщается по знаку: LONG_MIN -> (int) 0, LONG_MAX -> (int) -1.
        assert_eq!(c_atoi("-9223372036854775808"), 0);
        assert_eq!(c_atoi("-99999999999999999999"), 0);
        assert_eq!(c_atoi("9223372036854775807"), -1);
        assert_eq!(c_atoi("99999999999999999999"), -1);
        assert_eq!(c_atoi("-9223372036854775807"), 1);
    }

    #[test]
    fn only_four_numbers_are_read() {
        assert_eq!(
            AssEffect::parse("Scroll down;1;2;3;4;5;6"),
            AssEffect::Scroll {
                direction: AssScrollDirection::Down,
                y_top: 1,
                y_bottom: 2,
                delay_ms: 3
            }
        );
        assert_eq!(effect_numbers("Banner;1;2;3;4;5;6"), vec![1, 2, 3, 4]);
    }
}
