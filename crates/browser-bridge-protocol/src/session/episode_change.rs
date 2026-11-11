//! Смена серии на той же странице (протокол 1.9, сессия 16a, решения 303, 306–308).
//!
//! Плеер страницы бывает «одностраничником»: серия доиграла, а следующую он ставит тем же
//! документом — новым `MediaSource`, новым файлом того же элемента или другим элементом.
//! До 1.9 плеер узнавал о смене серии только по новому поколению потока, а заголовок вкладки —
//! один раз, при открытии сеанса (`open_session.page.title`). С 1.9 экстеншен говорит два новых
//! слова страница → плеер:
//!
//! ```json
//! {"type":"awaiting_next_episode","patience_ms":60000}
//! {"type":"page_title_changed","title":"Серія 4"}
//! ```
//!
//! - `awaiting_next_episode` — серия у нас доиграла (часы плеера сказали `ended`), страница
//!   сняла источник, и экстеншен ждёт новый источник не дольше `patience_ms`
//!   ([`NextEpisodePatience`]). Окно держит последний кадр с надписью ожидания; новое поколение
//!   на экране надпись снимает. Не пришёл новый источник в срок — экстеншен закрывает связь
//!   кодом 4011 [`SessionCloseReason::NextEpisodeNotStarted`](super::SessionCloseReason).
//! - `page_title_changed` — заголовок вкладки сменился (новая серия). Тот же тип и те же
//!   правила, что у заголовка при открытии: [`PageTitle`](crate::PageTitle) — до
//!   `MAX_PAGE_TITLE_BYTES` байт, без управляющих символов и переворота направления, пустой
//!   допустим (страница без `<title>`). Чистит и режет заголовок экстеншен до отправки.
//!
//! Оба слова не привязаны к поколению потока (как `page_navigation`) и, как всё, кроме
//! возврата страницы, запрещены, пока страница ушла (`page_left`). При общей версии ниже 1.9
//! экстеншен их не шлёт, а сторож плеера отвергает входящие как нарушение. Код 4011 на
//! проводе не версионируется (как 4009 и 4010), но экстеншен закрывает им связь только при
//! общей версии не ниже [`NEXT_EPISODE_NOT_STARTED_INTRODUCED_IN`]; со старым плеером он
//! закрывает прежним 4009 `page_took_video_back` — старый плеер 4011 не знает и молча кончил
//! бы сеанс без объяснения.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::version::ProtocolVersion;

/// Версия протокола, в которой появился `awaiting_next_episode`.
pub const AWAITING_NEXT_EPISODE_INTRODUCED_IN: ProtocolVersion = ProtocolVersion::V1_9;

/// Версия протокола, в которой появился `page_title_changed`.
pub const PAGE_TITLE_CHANGED_INTRODUCED_IN: ProtocolVersion = ProtocolVersion::V1_9;

/// С какой общей версии экстеншен вправе закрыть связь кодом 4011 `next_episode_not_started`
/// (иначе — прежним 4009). Сам код на проводе не версионируется; это правило отправителя.
pub const NEXT_EPISODE_NOT_STARTED_INTRODUCED_IN: ProtocolVersion = ProtocolVersion::V1_9;

/// Сколько экстеншен ждёт новый источник после конца серии, в миллисекундах (`patience_ms`).
///
/// Любое число `u32`: срок выбирает экстеншен (решение 308: 60 с), плеер его не оспаривает,
/// а только показывает ожидание. Отрицательное, дробное, строка или больше `u32` — ошибка
/// разбора, как у прочих чисел протокола.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NextEpisodePatience(u32);

impl NextEpisodePatience {
    /// Срок из числа миллисекунд.
    #[must_use]
    pub const fn from_millis(milliseconds: u32) -> Self {
        Self(milliseconds)
    }

    /// Число миллисекунд, как на проводе.
    #[must_use]
    pub const fn as_millis(self) -> u32 {
        self.0
    }

    /// Тот же срок как [`Duration`] — для таймеров плеера.
    #[must_use]
    pub const fn as_duration(self) -> Duration {
        // `u32` миллисекунд всегда помещается в `Duration` без потерь.
        Duration::from_millis(self.0 as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patience_keeps_the_wire_milliseconds_and_converts_to_a_duration() {
        let patience = NextEpisodePatience::from_millis(60_000);
        assert_eq!(patience.as_millis(), 60_000);
        assert_eq!(patience.as_duration(), Duration::from_secs(60));
        let longest = NextEpisodePatience::from_millis(u32::MAX);
        assert_eq!(
            longest.as_duration(),
            Duration::from_millis(u64::from(u32::MAX))
        );
    }

    #[test]
    fn everything_of_the_next_episode_wait_appears_in_1_9() {
        for introduced_in in [
            AWAITING_NEXT_EPISODE_INTRODUCED_IN,
            PAGE_TITLE_CHANGED_INTRODUCED_IN,
            NEXT_EPISODE_NOT_STARTED_INTRODUCED_IN,
        ] {
            assert_eq!(introduced_in, ProtocolVersion::V1_9);
            assert!(!ProtocolVersion::V1_8.includes(introduced_in));
        }
    }
}
