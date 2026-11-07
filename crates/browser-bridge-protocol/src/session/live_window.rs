//! Окно эфира live HLS (протокол 1.2, сессия 10a3, решение 232).
//!
//! Плеер сам ведёт live HLS и сам знает честное окно перемотки: пересечение
//! удержанных сегментов, реально наблюдённых пакетов и безопасных якорей. Страница
//! этого не видит, поэтому плеер сообщает экстеншену `live_window{generation,
//! edge_us, seekable}`, а тень страницы показывает по нему `seekable` и прижимает
//! перемотку страницы к краям окна (как это делает сам браузер).
//!
//! Все три момента — во времени плеера (то же, что `clock.position_us`), целые
//! микросекунды. Договор: `start_us ≤ end_us ≤ edge_us`. Окно без DVR
//! (`kind: "none"`) — эфир, по которому перемотки нет вовсе.
//!
//! Как часто плеер шлёт окно — [`crate::limits::MIN_LIVE_WINDOW_INTERVAL_MS`].

use serde::{Deserialize, Serialize};

use crate::error::ValueViolation;
use crate::strict_shape::strict_object;
use crate::time::MediaTime;

// Строгая форма JSON (`crate::strict_shape`): объекты и перечисления с меткой —
// только объектом, метки — только строкой; чужая форма — `wrong_value_type`.
strict_object!(LiveSeekable);

/// Куда в эфире можно перемотать.
///
/// Не необязательное поле, а явная метка: «перемотки нет» и «окно есть» не путаются
/// с пропуском поля или `null` ни в Rust, ни в TypeScript (как `ResourceByteRange`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum LiveSeekable {
    /// Эфир без DVR: перемотки нет, только край.
    None {},
    /// Окно перемотки `[start_us, end_us]` во времени плеера.
    ///
    /// `start_us == end_us` допустимо: окно в одну точку (например, единственный
    /// безопасный якорь сразу после входа в эфир).
    Range {
        /// Самое раннее место, куда плеер честно может встать.
        start_us: MediaTime,
        /// Самое позднее место перемотки (не дальше края эфира).
        end_us: MediaTime,
    },
}

impl LiveSeekable {
    /// Проверяет инварианты окна относительно края эфира `edge_us`.
    ///
    /// Нарушения — ошибка отправителя, сообщение не разбирается и не пишется:
    /// - начало позже конца (окно вывернуто) — [`ValueViolation::EmptyRange`],
    ///   как у вывернутого промежутка `source_buffer_removed`;
    /// - конец дальше края (перемотка в ещё не вышедший эфир) —
    ///   [`ValueViolation::NumberOutOfRange`].
    ///
    /// # Errors
    /// Нарушенное правило `start_us ≤ end_us ≤ edge_us`.
    pub const fn check_within_edge(self, edge_us: MediaTime) -> Result<(), ValueViolation> {
        match self {
            // Без окна сверять нечего: край сам по себе любой момент шкалы.
            Self::None {} => Ok(()),
            Self::Range { start_us, end_us } => {
                if start_us.as_micros() > end_us.as_micros() {
                    return Err(ValueViolation::EmptyRange);
                }
                if end_us.as_micros() > edge_us.as_micros() {
                    return Err(ValueViolation::NumberOutOfRange);
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(micros: u64) -> MediaTime {
        MediaTime::from_micros(micros).unwrap_or(MediaTime::ZERO)
    }

    fn range(start: u64, end: u64) -> LiveSeekable {
        LiveSeekable::Range {
            start_us: at(start),
            end_us: at(end),
        }
    }

    #[test]
    fn a_window_inside_the_edge_including_its_borders_is_accepted() {
        assert_eq!(range(40, 60).check_within_edge(at(61)), Ok(()));
        // Края включительно: окно в одну точку и конец ровно на краю.
        assert_eq!(range(50, 50).check_within_edge(at(50)), Ok(()));
        assert_eq!(range(0, 0).check_within_edge(at(0)), Ok(()));
    }

    #[test]
    fn an_inverted_window_is_an_empty_range() {
        assert_eq!(
            range(61, 60).check_within_edge(at(100)),
            Err(ValueViolation::EmptyRange)
        );
    }

    #[test]
    fn a_window_ending_beyond_the_edge_is_out_of_range() {
        assert_eq!(
            range(40, 62).check_within_edge(at(61)),
            Err(ValueViolation::NumberOutOfRange)
        );
    }

    #[test]
    fn no_dvr_has_nothing_to_check_against_the_edge() {
        assert_eq!(LiveSeekable::None {}.check_within_edge(at(0)), Ok(()));
    }
}
