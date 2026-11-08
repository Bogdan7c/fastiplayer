//! Возврат страницы на место (протокол 1.4, сессия 11b1, решения 240–241): «tab return magic».
//!
//! Весь договор в одном месте:
//! 1. Вкладка-источник исчезла (закрыта, ушла на другую страницу, сломался плеер сайта), но
//!    экстеншен НЕ рвёт соединение: оставляет сеанс «сиротой» (байтов больше нет) и шлёт
//!    `page_left{reason}` (`PageSessionMessage::PageLeft`). Плеер продолжает слать `clock`.
//! 2. Зритель нажимает play в плеере → плеер шлёт `reopen_page{position_us}`
//!    (`PlayerSessionMessage::ReopenPage`): «верни страницу на этой секунде».
//! 3. Экстеншен открывает страницу заново. Получилось — `page_returned` (тот же сеанс
//!    продолжается, дальше идёт НОВОЕ поколение потока). Не получилось —
//!    `page_reopen_failed{reason: PageReopenFailure}` и закрытие соединения кодом 4002
//!    (`SessionCloseReason::TabGone`).
//! 4. Если экстеншен вообще не может вернуть страницу, он по-прежнему шлёт `tab_gone`
//!    (конец сеанса, как до 1.4); `tab_gone` в состоянии «страница ушла» принимается.
//!
//! Правила порядка сообщений — в `crate::stream_gate` (состояние присутствия страницы).
//! Коды закрытия не менялись. Все новые сообщения появились в 1.4: при общей версии ниже
//! экстеншен шлёт `tab_gone` и закрывает сеанс, как раньше.

use serde::{Deserialize, Serialize};

use crate::strict_shape::strict_label;

// Метка — только строкой; чужая форма (`{"no_browser_window":null}`) — `wrong_value_type`.
strict_label!(PageReopenFailure);

/// Почему экстеншен не смог вернуть страницу (`page_reopen_failed`, протокол 1.4).
///
/// Метка строгая (snake_case, без нормализации регистра): незнакомая — `unknown_value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum PageReopenFailure {
    /// Нет окна браузера, в котором можно открыть страницу.
    NoBrowserWindow,
    /// Страница не загрузилась (навигация закончилась ошибкой).
    PageDidNotLoad,
    /// Страница открылась, но показывает другое видео (например, сайт-одностраничник).
    NotTheSameVideo,
    /// Видео не запустилось за отведённое время.
    VideoDidNotStart,
}

impl PageReopenFailure {
    /// В какой версии протокола появилось это значение (`crate::version`, правило 5).
    ///
    /// Все четыре появились вместе с самим сообщением `page_reopen_failed`. Таблица значений —
    /// `fixtures/value-versions.json`; зеркало — `pageReopenFailureIntroducedIn` (`introduced-in.ts`).
    #[must_use]
    pub const fn introduced_in(self) -> crate::version::ProtocolVersion {
        match self {
            Self::NoBrowserWindow
            | Self::PageDidNotLoad
            | Self::NotTheSameVideo
            | Self::VideoDidNotStart => crate::version::ProtocolVersion::V1_4,
        }
    }
}
