//! Время TTML: параметры документа, разбор выражений времени и окна показа (par-семантика).
//!
//! Поддержано: часы `hh:mm:ss[.дробь]`, `hh:mm:ss:ff[.подкадр]`, смещения `h m s ms f t`,
//! `ttp:frameRate`, `ttp:frameRateMultiplier`, `ttp:tickRate`, `ttp:timeBase="media"`.
//! Результат — микросекунды `i64` (как `SubtitleTime`).

use super::model::{RawElement, TimingAttributes};

/// Число цифр десятичной записи, после которого запись отвергается (защита `u128`).
const MAX_DECIMAL_DIGITS: usize = 18;
/// Микросекунд в секунде.
const MICROS_PER_SECOND: u128 = 1_000_000;
/// Кадров в секунду по умолчанию (TTML).
const DEFAULT_FRAME_RATE: u32 = 30;

/// Параметры времени документа (атрибуты `ttp:*` на `<tt>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TimeParameters {
    frame_rate: u32,
    frame_rate_multiplier_numerator: u32,
    frame_rate_multiplier_denominator: u32,
    tick_rate: u32,
}

/// Итог чтения параметров: сами параметры и число непринятых атрибутов.
#[derive(Debug, Clone, Copy)]
pub(super) struct ParsedTimeParameters {
    pub(super) parameters: TimeParameters,
    /// Сколько `ttp:*` значений некорректны или не поддержаны (для счётчика отчёта).
    pub(super) ignored_count: usize,
}

impl TimeParameters {
    /// Читает параметры из корневого элемента.
    pub(super) fn from_root(root: &RawElement) -> ParsedTimeParameters {
        let mut ignored_count = 0;
        let explicit_frame_rate = root.parameter("frameRate").and_then(|value| {
            let parsed = parse_positive_u32(value);
            if parsed.is_none() {
                ignored_count += 1;
            }
            parsed
        });
        let frame_rate = explicit_frame_rate.unwrap_or(DEFAULT_FRAME_RATE);
        let (multiplier_numerator, multiplier_denominator) = root
            .parameter("frameRateMultiplier")
            .and_then(|value| {
                let parsed = parse_multiplier(value);
                if parsed.is_none() {
                    ignored_count += 1;
                }
                parsed
            })
            .unwrap_or((1, 1));
        // Без tickRate: 1 тик в секунду, а при заданном frameRate — по тику на кадр (TTML2).
        let default_tick_rate = explicit_frame_rate.unwrap_or(1);
        let tick_rate = root
            .parameter("tickRate")
            .and_then(|value| {
                let parsed = parse_positive_u32(value);
                if parsed.is_none() {
                    ignored_count += 1;
                }
                parsed
            })
            .unwrap_or(default_tick_rate);
        // Другие шкалы времени (clock, smpte) разбираются как media; это отмечается в отчёте.
        if root
            .parameter("timeBase")
            .is_some_and(|value| value.trim() != "media")
        {
            ignored_count += 1;
        }
        ParsedTimeParameters {
            parameters: Self {
                frame_rate,
                frame_rate_multiplier_numerator: multiplier_numerator,
                frame_rate_multiplier_denominator: multiplier_denominator,
                tick_rate,
            },
            ignored_count,
        }
    }

    /// Разбирает выражение времени в микросекунды. `None` — запись некорректна.
    pub(super) fn parse(&self, text: &str) -> Option<i64> {
        let text = text.trim();
        let micros = if text.contains(':') {
            self.parse_clock_time(text)?
        } else {
            self.parse_offset_time(text)?
        };
        // Верхняя граница - сам `i64` микросекунд (`SubtitleTime`): время media timeline
        // живых упаковщиков бывает эпохальным (~1.7e9 с), и искусственный потолок его резал бы.
        // Переполнение невозможно: `micros` - `u128`, `try_from` отвергает всё, что не влезло,
        // а дальнейшая арифметика окон (`TimeWindow::child`) использует `checked_add`.
        i64::try_from(micros).ok()
    }

    /// Микросекунды в одном кадре как дробь (числитель, знаменатель) секунды.
    fn frame_seconds(&self) -> (u128, u128) {
        (
            u128::from(self.frame_rate_multiplier_denominator),
            u128::from(self.frame_rate) * u128::from(self.frame_rate_multiplier_numerator),
        )
    }

    /// `hh:mm:ss`, `hh:mm:ss.дробь`, `hh:mm:ss:ff` или `hh:mm:ss:ff.подкадр`.
    fn parse_clock_time(&self, text: &str) -> Option<u128> {
        let fields: Vec<&str> = text.split(':').collect();
        let (hours, minutes, seconds, frames) = match fields.as_slice() {
            [hours, minutes, seconds] => (hours, minutes, seconds, None),
            [hours, minutes, seconds, frames] => (hours, minutes, seconds, Some(frames)),
            _ => return None,
        };
        if hours.len() < 2 || !all_ascii_digits(hours) || !is_two_digits(minutes) {
            return None;
        }
        let whole_seconds = u128::from(hours.parse::<u64>().ok()?) * 3600
            + u128::from(minutes.parse::<u64>().ok()?) * 60;
        let clock_micros = match frames {
            // Без кадров секунды могут иметь дробную часть.
            None => {
                let seconds = Decimal::parse(seconds)?;
                if seconds.integer_part_digits() != 2 {
                    return None;
                }
                whole_seconds * MICROS_PER_SECOND + seconds.to_micros(1, 1)?
            }
            Some(frames) => {
                if !is_two_digits(seconds) {
                    return None;
                }
                let frames = Decimal::parse(frames)?;
                let (frame_numerator, frame_denominator) = self.frame_seconds();
                let seconds_value = whole_seconds + u128::from(seconds.parse::<u64>().ok()?);
                seconds_value * MICROS_PER_SECOND
                    + frames.to_micros(frame_numerator, frame_denominator)?
            }
        };
        Some(clock_micros)
    }

    /// `<число><метрика>`, метрика: `h m s ms f t`.
    fn parse_offset_time(&self, text: &str) -> Option<u128> {
        let split_at = text.find(|character: char| character.is_ascii_alphabetic())?;
        let (number, metric) = text.split_at(split_at);
        let value = Decimal::parse(number)?;
        let (unit_numerator, unit_denominator) = match metric {
            "h" => (3600, 1),
            "m" => (60, 1),
            "s" => (1, 1),
            "ms" => (1, 1000),
            "f" => self.frame_seconds(),
            "t" => (1, u128::from(self.tick_rate)),
            _ => return None,
        };
        value.to_micros(unit_numerator, unit_denominator)
    }
}

/// Целое > 0.
fn parse_positive_u32(text: &str) -> Option<u32> {
    text.trim().parse::<u32>().ok().filter(|value| *value > 0)
}

/// `frameRateMultiplier="1000 1001"`.
fn parse_multiplier(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split_whitespace();
    let numerator = parse_positive_u32(parts.next()?)?;
    let denominator = parse_positive_u32(parts.next()?)?;
    parts.next().is_none().then_some((numerator, denominator))
}

fn all_ascii_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_two_digits(text: &str) -> bool {
    text.len() == 2 && all_ascii_digits(text)
}

/// Неотрицательное десятичное число без знака и степени: `12`, `1.5`, `.5` не допускается.
struct Decimal {
    mantissa: u128,
    /// Сколько цифр после точки.
    scale: u32,
    integer_digits: usize,
}

impl Decimal {
    fn parse(text: &str) -> Option<Self> {
        let (integer, fraction) = match text.split_once('.') {
            Some((integer, fraction)) => (integer, fraction),
            None => (text, ""),
        };
        let has_dot = text.contains('.');
        if !all_ascii_digits(integer)
            || (has_dot && !all_ascii_digits(fraction))
            || integer.len() + fraction.len() > MAX_DECIMAL_DIGITS
        {
            return None;
        }
        let digits = format!("{integer}{fraction}");
        Some(Self {
            mantissa: digits.parse().ok()?,
            scale: u32::try_from(fraction.len()).ok()?,
            integer_digits: integer.len(),
        })
    }

    fn integer_part_digits(&self) -> usize {
        self.integer_digits
    }

    /// Значение `число * numerator / denominator` секунд в микросекундах (округление к ближайшему).
    fn to_micros(&self, numerator: u128, denominator: u128) -> Option<u128> {
        let divisor = 10u128.checked_pow(self.scale)?.checked_mul(denominator)?;
        let scaled = self
            .mantissa
            .checked_mul(MICROS_PER_SECOND)?
            .checked_mul(numerator)?;
        Some((scaled + divisor / 2) / divisor)
    }
}

/// Окно показа элемента в абсолютном времени документа, `[begin, end)`.
///
/// `end == None` — конец не задан ни у элемента, ни у его предков («до конца документа»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TimeWindow {
    pub(super) begin_micros: i64,
    pub(super) end_micros: Option<i64>,
}

/// Почему окно элемента не получилось.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WindowError {
    /// Одно из выражений времени не разобралось.
    InvalidTime,
    /// Элемент никогда не активен внутри окна родителя (пустое пересечение).
    NeverActive,
}

impl TimeWindow {
    /// Окно всего документа: с нуля, без конца.
    pub(super) const DOCUMENT: Self = Self {
        begin_micros: 0,
        end_micros: None,
    };

    /// Окно дочернего элемента по par-семантике: `begin`/`end` отсчитываются от начала
    /// родителя, `dur` — от собственного начала; берётся более ранний конец и пересекается
    /// с окном родителя.
    pub(super) fn child(
        self,
        timing: &TimingAttributes,
        parameters: &TimeParameters,
    ) -> Result<Self, WindowError> {
        let offset = |text: &Option<String>| -> Result<Option<i64>, WindowError> {
            match text {
                None => Ok(None),
                Some(text) => parameters
                    .parse(text)
                    .map(Some)
                    .ok_or(WindowError::InvalidTime),
            }
        };
        let begin_offset = offset(&timing.begin)?.unwrap_or(0);
        let end_offset = offset(&timing.end)?;
        let duration = offset(&timing.dur)?;

        let begin = self
            .begin_micros
            .checked_add(begin_offset)
            .ok_or(WindowError::InvalidTime)?;
        let end_from_attribute = end_offset
            .map(|value| self.begin_micros.checked_add(value))
            .map(|sum| sum.ok_or(WindowError::InvalidTime))
            .transpose()?;
        let end_from_duration = duration
            .map(|value| begin.checked_add(value))
            .map(|sum| sum.ok_or(WindowError::InvalidTime))
            .transpose()?;
        let end = [end_from_attribute, end_from_duration, self.end_micros]
            .into_iter()
            .flatten()
            .min();
        if end.is_some_and(|end| end <= begin) || self.end_micros.is_some_and(|end| begin >= end) {
            return Err(WindowError::NeverActive);
        }
        Ok(Self {
            begin_micros: begin,
            end_micros: end,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameters(frame_rate: u32, multiplier: (u32, u32), tick_rate: u32) -> TimeParameters {
        TimeParameters {
            frame_rate,
            frame_rate_multiplier_numerator: multiplier.0,
            frame_rate_multiplier_denominator: multiplier.1,
            tick_rate,
        }
    }

    fn default_parameters() -> TimeParameters {
        parameters(30, (1, 1), 1)
    }

    #[test]
    fn clock_time_with_fraction() {
        assert_eq!(default_parameters().parse("00:00:01.5"), Some(1_500_000));
        assert_eq!(default_parameters().parse("01:02:03"), Some(3_723_000_000));
        assert_eq!(
            default_parameters().parse("100:00:00.001"),
            Some(360_000_001_000)
        );
    }

    #[test]
    fn clock_time_with_frames_uses_effective_frame_rate() {
        assert_eq!(default_parameters().parse("00:00:01:15"), Some(1_500_000));
        // 30000/1001 кадра/с: 30 кадров = 1,001 с.
        let ntsc = parameters(30, (1000, 1001), 1);
        assert_eq!(ntsc.parse("00:00:00:30"), Some(1_001_000));
    }

    #[test]
    fn offset_times_in_every_metric() {
        let time = parameters(25, (1, 1), 10_000_000);
        assert_eq!(time.parse("2h"), Some(7_200_000_000));
        assert_eq!(time.parse("1.5m"), Some(90_000_000));
        assert_eq!(time.parse("3s"), Some(3_000_000));
        assert_eq!(time.parse("250ms"), Some(250_000));
        assert_eq!(time.parse("50f"), Some(2_000_000));
        assert_eq!(time.parse("15000000t"), Some(1_500_000));
    }

    #[test]
    fn malformed_and_oversized_times_are_rejected() {
        let time = default_parameters();
        for text in [
            "",
            "1",
            "s",
            "1x",
            "1:2:3",
            "00:00:61:",
            "-1s",
            "00:00:1.5.5",
            "1e3s",
            ".5s",
        ] {
            assert_eq!(time.parse(text), None, "{text}");
        }
        assert_eq!(time.parse("99999999999999s"), None);
    }

    fn window(
        parent: TimeWindow,
        begin: Option<&str>,
        end: Option<&str>,
        dur: Option<&str>,
    ) -> Result<TimeWindow, WindowError> {
        let timing = TimingAttributes {
            begin: begin.map(str::to_owned),
            end: end.map(str::to_owned),
            dur: dur.map(str::to_owned),
            time_container: None,
        };
        parent.child(&timing, &default_parameters())
    }

    #[test]
    fn child_times_are_relative_to_parent_begin() {
        let parent = window(TimeWindow::DOCUMENT, Some("10s"), Some("20s"), None).unwrap();
        let child = window(parent, Some("2s"), Some("5s"), None).unwrap();
        assert_eq!(
            (child.begin_micros, child.end_micros),
            (12_000_000, Some(15_000_000))
        );
    }

    #[test]
    fn child_without_end_inherits_parent_end_and_is_clamped_to_it() {
        let parent = window(TimeWindow::DOCUMENT, Some("10s"), Some("20s"), None).unwrap();
        assert_eq!(
            window(parent, Some("1s"), None, None).unwrap().end_micros,
            Some(20_000_000)
        );
        assert_eq!(
            window(parent, None, Some("99s"), None).unwrap().end_micros,
            Some(20_000_000)
        );
    }

    #[test]
    fn dur_counts_from_own_begin_and_earliest_end_wins() {
        let parent = window(TimeWindow::DOCUMENT, Some("10s"), None, None).unwrap();
        let by_dur = window(parent, Some("2s"), None, Some("3s")).unwrap();
        assert_eq!(by_dur.end_micros, Some(15_000_000));
        let both = window(parent, Some("2s"), Some("4s"), Some("9s")).unwrap();
        assert_eq!(both.end_micros, Some(14_000_000));
    }

    #[test]
    fn open_ended_document_stays_open_and_empty_intersection_is_never_active() {
        assert_eq!(
            window(TimeWindow::DOCUMENT, Some("1s"), None, None)
                .unwrap()
                .end_micros,
            None
        );
        let parent = window(TimeWindow::DOCUMENT, Some("0s"), Some("5s"), None).unwrap();
        assert_eq!(
            window(parent, Some("5s"), None, None),
            Err(WindowError::NeverActive)
        );
        assert_eq!(
            window(parent, Some("3s"), Some("2s"), None),
            Err(WindowError::NeverActive)
        );
        assert_eq!(
            window(parent, Some("zzz"), None, None),
            Err(WindowError::InvalidTime)
        );
    }
}
