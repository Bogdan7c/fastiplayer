//! Участок «сеанс»: WebSocket из background экстеншена прямо в процесс
//! браузерного режима (решения 82 и 85).
//!
//! Одно соединение — один сеанс. По нему идут:
//! - текстовые рамки WebSocket — JSON-сообщения [`PageSessionMessage`] и
//!   [`PlayerSessionMessage`];
//! - двоичные рамки WebSocket — куски байтов (`crate::data_frame`).
//!
//! WebSocket сохраняет порядок рамок обоих видов, поэтому «сначала объявить
//! SourceBuffer, потом слать его байты» выполняется само.
//!
//! Порядок сеанса:
//! 1. экстеншен шлёт `attach` с ключом и пропуском из `session_granted`;
//! 2. плеер отвечает `attached` или закрывает соединение с кодом
//!    [`SessionCloseReason`];
//! 3. дальше обе стороны говорят свободно, пока одна не закроет соединение.
//!
//! Коды закрытия — [`SessionCloseReason`] (4000–4011). Почти все — слова плеера; страница
//! (экстеншен) закрывает связь осмысленным кодом в четырёх случаях:
//! - 4003 `protocol_violation` — сбой на её стороне (поток битый, тень не встала);
//!   плеер показывает «Stream error — video returned to the page» (решение 132);
//! - 4009 `page_took_video_back` — страница честно забрала видео, вкладка на месте;
//!   плеер не доигрывает и показывает «The page took the video back» (решение 179).
//!   Код добавлен внутри 1.0, как `stream_rejected{playback_failed}` (решение 128):
//!   экстеншен ещё нигде не опубликован.
//! - 4010 `viewer_returned_to_page` — зритель нажал «Return» на заглушке страницы
//!   (сессия 11a, решение 236): видео играет на странице дальше, плеер молча и сразу
//!   закрывает окно — без доигрывания, сообщения и прощания, как на 1000.
//! - 4011 `next_episode_not_started` — серия доиграла, страница сняла источник, а новый не
//!   пришёл за срок ожидания (протокол 1.9, сессия 16a, решение 307): плеер коротко пишет
//!   «The page didn't start the next episode» и закрывает окно без досмотра.
//!
//! Коды закрытия не версионируются (4009 и 4010 добавлены без новой `minor`): плеер,
//! не знающий кода, видит чужой 4xxx и кончает сеанс молча — для 4010 это и есть
//! правильное поведение. Для 4011 молчание было бы потерей объяснения, поэтому экстеншен
//! шлёт его только при общей версии не ниже 1.9
//! ([`NEXT_EPISODE_NOT_STARTED_INTRODUCED_IN`]), а со старым плеером закрывает прежним 4009.
//! Договор — [`episode_change`].
//!
//! Протокол 1.4 (решения 240–241) добавил «возврат вкладки»: страница-источник ушла, но связь
//! остаётся открытой (`page_left`), зритель нажимает play (`reopen_page`), экстеншен открывает
//! страницу заново (`page_returned`) или сообщает отказ (`page_reopen_failed`, затем 4002).
//! Договор целиком — [`page_reopen`]; коды закрытия не менялись.
//!
//! Протокол 1.5 (решения 270–274) добавил субтитры страницы: заголовок
//! `subtitle_track_offered` и байты документа рамками вида 3. Договор — [`page_subtitles`].
//!
//! Обычное 1000 от страницы и кадр без кода ничего не объясняют: плеер кончает сеанс
//! молча.

pub mod ad_state;
pub mod episode_change;
pub mod live_window;
pub mod page;
pub mod page_navigation;
pub mod page_reopen;
pub mod page_subtitles;
pub mod player;
pub mod variant_outcome;
pub mod variants;

pub use ad_state::{
    AdCountdownSeconds, AdPod, AdPodNumber, AdSkip, MAX_AD_COUNTDOWN_SECONDS, MAX_ADS_IN_POD,
    PageAdState,
};
pub use episode_change::{
    AWAITING_NEXT_EPISODE_INTRODUCED_IN, NEXT_EPISODE_NOT_STARTED_INTRODUCED_IN,
    NextEpisodePatience, PAGE_TITLE_CHANGED_INTRODUCED_IN,
};
pub use live_window::LiveSeekable;
pub use page::{
    PagePlaybackRequest, PageSessionMessage, RangeFetchFailure, RangeFetchOutcome, RemovalEnd,
    ResourceSize, SeekCause, StreamScope, TabGoneReason,
};
pub use page_navigation::{NextEpisodeAvailability, PAGE_NAVIGATION_INTRODUCED_IN};
pub use page_reopen::PageReopenFailure;
pub use page_subtitles::{
    PAGE_SUBTITLES_INTRODUCED_IN, PageSubtitleByteLength, PageSubtitleDisplay, PageSubtitleLabel,
    PageSubtitleLanguage, PageSubtitleOrigin, PageSubtitleTrackId, SUBTITLE_CHOICE_INTRODUCED_IN,
    SubtitleChoice, SubtitleFormatHint,
};
pub use player::{
    BufferIntake, ClockSequence, ClockState, PlayerSessionMessage, ResumeState, StreamRejection,
};
pub use variant_outcome::{VARIANT_OUTCOME_INTRODUCED_IN, VariantOutcome, VariantRequest};
pub use variants::{VariantKind, VariantMenu, VariantOption, VariantSelection};

/// Почему сеанс закрыт — код закрытия WebSocket.
///
/// Коды 4000–4999 стандарт WebSocket (RFC 6455, 7.4.2) оставляет приложениям.
/// Причина передаётся кодом, а не текстом: текст закрытия ограничен 123 байтами
/// и для машин ненадёжен.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionCloseReason {
    /// Человек закрыл окно плеера.
    PlayerClosed,
    /// Видео из другой вкладки заменило этот сеанс (решение 40).
    ReplacedByAnotherTab,
    /// Вкладка ушла (решение 42); закрывает экстеншен.
    TabGone,
    /// Нарушение протокола: битое сообщение, «рамка из будущего», пропуск номера.
    ProtocolViolation,
    /// Пропуск не подошёл.
    InvalidToken,
    /// Сеанса с таким ключом нет (истёк, закрыт).
    UnknownSession,
    /// Плеер завершается.
    PlayerShuttingDown,
    /// `attach` не пришёл вовремя.
    AttachTimeout,
    /// К сеансу уже подключено другое соединение.
    AlreadyAttached,
    /// Страница честно забрала видео, а вкладка на месте (решение 179, сессия 09a2): поставила
    /// элементу обычный файл или HLS-адрес вместо потока, оставила его без источника дольше
    /// допуска тени или убрала элемент из документа. Закрывает экстеншен. Плеер не доигрывает
    /// (страница уже играет сама — было бы два звука) и не пишет «Source tab closed»: короткое
    /// «The page took the video back» и закрытие окна. Добавлен внутри 1.0, как
    /// `stream_rejected{playback_failed}` (решение 128): экстеншен ещё нигде не опубликован.
    PageTookVideoBack,
    /// Зритель нажал «Return» на заглушке страницы (сессия 11a, решение 236): тень снята на
    /// своей позиции, видео страницы играет дальше. Закрывает экстеншен. Плеер закрывает окно
    /// сразу и молча: без доигрывания (было бы два звука), без сообщения и без прощания
    /// `return_to_page` (видео уже у страницы). Коды не версионируются: старый плеер видит
    /// незнакомый 4xxx и тоже кончает сеанс молча.
    ViewerReturnedToPage,
    /// Серия доиграла, страница сняла источник, а нового не поставила за срок ожидания
    /// (протокол 1.9, сессия 16a, решения 303, 307): ждали после `awaiting_next_episode`.
    /// Закрывает экстеншен. Плеер не доигрывает (досматривать нечего — серия кончилась),
    /// коротко пишет «The page didn't start the next episode» и закрывает окно. Со старым
    /// плеером (общая версия ниже 1.9) экстеншен вместо этого кода шлёт 4009.
    NextEpisodeNotStarted,
}

impl SessionCloseReason {
    /// Все причины — для обратного перевода кода.
    const ALL: [Self; 12] = [
        Self::PlayerClosed,
        Self::ReplacedByAnotherTab,
        Self::TabGone,
        Self::ProtocolViolation,
        Self::InvalidToken,
        Self::UnknownSession,
        Self::PlayerShuttingDown,
        Self::AttachTimeout,
        Self::AlreadyAttached,
        Self::PageTookVideoBack,
        Self::ViewerReturnedToPage,
        Self::NextEpisodeNotStarted,
    ];

    /// Код закрытия WebSocket.
    #[must_use]
    pub const fn close_code(self) -> u16 {
        match self {
            Self::PlayerClosed => 4000,
            Self::ReplacedByAnotherTab => 4001,
            Self::TabGone => 4002,
            Self::ProtocolViolation => 4003,
            Self::InvalidToken => 4004,
            Self::UnknownSession => 4005,
            Self::PlayerShuttingDown => 4006,
            Self::AttachTimeout => 4007,
            Self::AlreadyAttached => 4008,
            Self::PageTookVideoBack => 4009,
            Self::ViewerReturnedToPage => 4010,
            Self::NextEpisodeNotStarted => 4011,
        }
    }

    /// Причина по коду закрытия; `None` — код не наш (обрыв сети, 1006 и т. п.).
    #[must_use]
    pub fn from_close_code(code: u16) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.close_code() == code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_close_reason_has_a_unique_application_code_that_maps_back() {
        let mut seen_codes = std::collections::BTreeSet::new();
        for reason in SessionCloseReason::ALL {
            let code = reason.close_code();
            assert!((4000..=4999).contains(&code), "{reason:?} → {code}");
            assert!(seen_codes.insert(code), "повтор кода {code}");
            assert_eq!(SessionCloseReason::from_close_code(code), Some(reason));
        }
        assert_eq!(SessionCloseReason::from_close_code(1006), None);
        assert_eq!(SessionCloseReason::from_close_code(4999), None);
    }

    #[test]
    fn the_page_took_the_video_back_is_4009_both_ways() {
        // Решение 179 (сессия 09a2): код закреплён числом — его же зеркалит экстеншен
        // (`SESSION_CLOSE_CODES.page_took_video_back` в `close-reason.ts`).
        assert_eq!(SessionCloseReason::PageTookVideoBack.close_code(), 4009);
        assert_eq!(
            SessionCloseReason::from_close_code(4009),
            Some(SessionCloseReason::PageTookVideoBack)
        );
    }

    #[test]
    fn the_viewer_returning_to_the_page_is_4010_both_ways() {
        // Решение 236 (сессия 11a): код закреплён числом — его же зеркалит экстеншен
        // (`SESSION_CLOSE_CODES.viewer_returned_to_page` в `close-reason.ts`).
        assert_eq!(SessionCloseReason::ViewerReturnedToPage.close_code(), 4010);
        assert_eq!(
            SessionCloseReason::from_close_code(4010),
            Some(SessionCloseReason::ViewerReturnedToPage)
        );
    }

    #[test]
    fn the_next_episode_not_started_is_4011_both_ways() {
        // Сессия 16a (решение 307): код закреплён числом — его же зеркалит экстеншен
        // (`SESSION_CLOSE_CODES.next_episode_not_started` в `close-reason.ts`).
        assert_eq!(SessionCloseReason::NextEpisodeNotStarted.close_code(), 4011);
        assert_eq!(
            SessionCloseReason::from_close_code(4011),
            Some(SessionCloseReason::NextEpisodeNotStarted)
        );
        // Следующий код ещё ничей.
        assert_eq!(SessionCloseReason::from_close_code(4012), None);
    }
}
