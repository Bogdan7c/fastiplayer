//! Плашка рекламы: нейтральные типы между медиа-частью сеанса, окном и рисованием
//! (сессия 09d, решения 184, 206, 207).
//!
//! Страница сообщает плееру только числа и метки (`ad_state`, инвариант 1: текста со
//! страницы в плеере нет). Медиа-часть (`browser_session::session_media::ad_presence`)
//! превращает их в [`AdPlate`], окно хранит последнее слово и рисует
//! (`ui::ad_plate`), а клики возвращает как [`AdPlateAction`]. Модуль ничего не знает
//! ни о мосте, ни о протоколе, ни об egui — только словарь и строки журнала.
//!
//! Строки журнала ниже читает стенд и тесты процесса (уровень info, без текста
//! страницы, числа и метки — полями `ключ=значение`).

/// «Реклама появилась или изменилась»: поля `ad_index`, `ad_count` (0 — неизвестно),
/// `skip` (`none` / `countdown` / `skip` / `on_page`), `countdown_seconds`.
pub(crate) const AD_PLATE_SHOWN_LINE: &str = "Ad plate shown";

/// «Реклама кончилась или сеанс/вкладка ушли»: плашки больше нет.
pub(crate) const AD_PLATE_CLEARED_LINE: &str = "Ad plate cleared";

/// Странице ушёл `skip_ad` (зритель нажал «Skip ›», «N» или MPRIS Next).
pub(crate) const AD_SKIP_SENT_LINE: &str = "Ad skip sent to the page";

/// Перемотка зрителя отклонена, пока на экране реклама (решение 206).
pub(crate) const AD_SEEK_REFUSED_LINE: &str = "Viewer seek refused: an ad is on screen";

/// Смена скорости зрителем отклонена, пока на экране реклама (решение 206).
pub(crate) const AD_RATE_REFUSED_LINE: &str = "Viewer speed change refused: an ad is on screen";

/// Какая реклама из блока идёт.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdPodLabel {
    /// Сайт счётчика не показывает: плашка просто «Ad».
    Unknown,
    /// «Ad N of M».
    Known {
        /// Номер идущей рекламы (с единицы).
        index: u16,
        /// Сколько всего в блоке.
        count: u16,
    },
}

/// Что плашка предлагает сделать с рекламой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdSkipOffer {
    /// Ничего: пропуска нет или сайт о нём ещё не говорит.
    Nothing,
    /// Сайт показывает отсчёт до пропуска.
    Countdown {
        /// Секунды, как на сайте.
        seconds: u16,
    },
    /// Кнопка «Skip ›»: наш клик на странице работает.
    Skip,
    /// Сайт показал свою кнопку, но верит только настоящей мыши (YouTube, решения 207, 208):
    /// плашка даёт подсказку без кнопки, нажимает человек на странице.
    OnPage,
}

impl AdSkipOffer {
    /// Метка для журнала (стенд читает по ней).
    pub(crate) const fn log_label(self) -> &'static str {
        match self {
            Self::Nothing => "none",
            Self::Countdown { .. } => "countdown",
            Self::Skip => "skip",
            Self::OnPage => "on_page",
        }
    }
}

/// Действие, которое зритель просит у плашки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdPlateAction {
    /// Нажать кнопку пропуска на странице (`skip_ad`).
    Skip,
}

/// Ушла ли уже команда за кнопкой плашки (защита от повторного клика).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdActionProgress {
    /// Команды нет: кнопку можно нажимать.
    Idle,
    /// Команда ушла странице, ответа (смены состояния рекламы) ещё нет: кнопка занята.
    InFlight,
}

/// Что окно показывает про рекламу на странице (плашки нет — рекламы нет).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AdPlate {
    /// Какая из блока.
    pub(crate) pod: AdPodLabel,
    /// Что предложено сделать.
    pub(crate) offer: AdSkipOffer,
    /// Ушла ли команда за кнопкой.
    pub(crate) progress: AdActionProgress,
    /// Заперты ли перемотка и скорость (решение 206; снимается предохранителем по сроку).
    pub(crate) locks_transport: bool,
}

impl AdPlate {
    /// Действие, за которым сейчас стоит кнопка плашки (или «Next»), независимо от того,
    /// занята ли она: отсчёт и «ничего» действия не дают.
    pub(crate) const fn offered_action(&self) -> Option<AdPlateAction> {
        match self.offer {
            AdSkipOffer::Skip => Some(AdPlateAction::Skip),
            AdSkipOffer::Nothing | AdSkipOffer::Countdown { .. } | AdSkipOffer::OnPage => None,
        }
    }
}

/// Решение владельца состояния про просьбу зрителя «сделай действие плашки».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdActionDecision {
    /// Команда уходит странице.
    Send,
    /// Прошлая команда ещё в пути: вторая не уходит.
    AlreadyInFlight,
    /// Такого действия плашка сейчас не предлагает (рекламы нет, другая кнопка, отсчёт).
    NotOffered,
}
