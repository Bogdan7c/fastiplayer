//! Номер поколения в тике часов (протокол 1.10, сессия 16a2, решение 311).
//!
//! Тик `clock` до 1.10 говорил «где плеер и что делает», но не «про какой ролик». Пока плеер
//! переходит с одного поколения потока на другое (реклама → фильм, серия → серия, смена
//! источника на том же или другом элементе страницы), он ещё какое-то время шлёт часы
//! ПРОШЛОГО поколения. Тень видеоэлемента на странице принимала их за часы нового видео:
//! `ended` баннера, кончившегося на 12,0 с, подтверждал старт фильма с 11,1 с и навсегда
//! защёлкивал у фильма `ended`. С 1.10 тик называет поколение, которое он описывает:
//!
//! ```json
//! {"type":"clock","seq":42,"position_us":11100000,"state":"playing","rate":1000,"generation":2}
//! ```
//!
//! - `generation` — необязательное поле, всегда ПОСЛЕДНЕЕ: тик формы 1.9 — точный префикс
//!   тика 1.10. Значение — [`StreamGeneration`](crate::StreamGeneration) (целое 1..=2³²−1),
//!   тот же номер, что в `generation_started` / `file_source_added` / `hls_source_added`.
//! - Плеер шлёт поле только странице, договорившейся не ниже
//!   [`CLOCK_GENERATION_INTRODUCED_IN`]; со стороной 1.9 тик остаётся прежним (старый
//!   экстеншен отверг бы незнакомое поле — «договорились — значит строго»).
//! - Нет ключа — «номер неизвестен» (сторона старше 1.10 или плеер, у которого ещё нет
//!   поколения). `null` — ошибка `wrong_value_type`: одна форма «номера нет» на проводе
//!   (`crate::strict_shape::optional_field`).
//!
//! Тик с полем требует 1.10 ([`PlayerSessionMessage::required_version`]); тип `clock` по-прежнему
//! из 1.0 (`introduced_in`), а общая таблица версий полей — `fixtures/field-versions.json`.
//!
//! [`PlayerSessionMessage::required_version`]: super::PlayerSessionMessage::required_version

use crate::version::ProtocolVersion;

/// Версия протокола, в которой у `clock` появилось необязательное поле `generation`.
///
/// Плеер сверяет с ней общую версию сеанса ДО отправки
/// (`agreed.includes(CLOCK_GENERATION_INTRODUCED_IN)`), экстеншен — до того, как верить номеру.
pub const CLOCK_GENERATION_INTRODUCED_IN: ProtocolVersion = ProtocolVersion::V1_10;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_generation_appears_in_1_10_and_not_earlier() {
        assert_eq!(CLOCK_GENERATION_INTRODUCED_IN, ProtocolVersion::V1_10);
        assert!(!ProtocolVersion::V1_9.includes(CLOCK_GENERATION_INTRODUCED_IN));
        assert!(ProtocolVersion::V1_10.includes(CLOCK_GENERATION_INTRODUCED_IN));
    }
}
