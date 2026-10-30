//! Профиль вида окна: какие регионы интерфейса показывает экземпляр окна.
//!
//! Одно и то же приложение (`AppState`, те же регионы `ui`) рисует два вида окна:
//! - обычный плеер — очередь, открытие файлов и ссылок, Previous/Next, Shuffle/Repeat;
//! - окно браузерного режима (мост, решение 46) — то же видео, транспорт, громкость,
//!   скорость, ♪, CC, уведомления, фуллскрин, сайдбар Settings и Info, но **без**
//!   очереди, секции URL, кнопки Open / Ctrl+O, броска файлов и навигации по очереди:
//!   источник один — страница браузера (решение 121), и окно не притворяется обычным
//!   плеером с плейлистом.
//!
//! Профиль — ТИПИЗИРОВАННЫЙ параметр экземпляра окна (задаётся при сборке `AppState`),
//! а регионы спрашивают его намерениями (`offers_media_opening`, `offers_queue_navigation`,
//! …), а не сравнением «это браузер?». Так новый вид (например «Наклейка» режима 3:
//! без кнопок окна и ресайза, решение 121) добавляется новым вариантом и новыми
//! ответами, а не россыпью условий по регионам.

use crate::state::SidebarSection;

/// Секции сайдбара обычного окна — в порядке кнопок заголовка.
const STANDARD_SIDEBAR_SECTIONS: &[SidebarSection] = &[
    SidebarSection::Playlist,
    SidebarSection::Settings,
    SidebarSection::Url,
    SidebarSection::Info,
];

/// Секции сайдбара окна браузерного режима: очереди и ввода ссылок у него нет.
const BROWSER_SESSION_SIDEBAR_SECTIONS: &[SidebarSection] =
    &[SidebarSection::Settings, SidebarSection::Info];

/// Вид окна (см. описание модуля).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum WindowViewProfile {
    /// Обычный плеер с очередью.
    #[default]
    Standard,
    /// Окно браузерного режима: один источник — страница браузера.
    BrowserSession,
}

impl WindowViewProfile {
    /// Может ли человек открыть в этом окне своё медиа: кнопка Open, Ctrl+O, бросок
    /// файлов и ссылок, импорт плейлиста. В браузерном окне источник задаёт страница —
    /// чужой файл заменил бы поток страницы молча.
    pub(crate) const fn offers_media_opening(self) -> bool {
        match self {
            Self::Standard => true,
            Self::BrowserSession => false,
        }
    }

    /// Есть ли навигация по очереди: Previous/Next (кнопки, P/N, медиаклавиши) и
    /// Shuffle/Repeat. У браузерного окна очереди нет (следующий ролик выбирает страница).
    pub(crate) const fn offers_queue_navigation(self) -> bool {
        match self {
            Self::Standard => true,
            Self::BrowserSession => false,
        }
    }

    /// Секции сайдбара, которые предлагают кнопки заголовка (в их порядке).
    ///
    /// Host сайдбара один на все виды (`mem:app-egui/reusable-ui-regions`): профиль
    /// меняет только набор сущностей, а не геометрию и не жизненный цикл области.
    pub(crate) const fn sidebar_sections(self) -> &'static [SidebarSection] {
        match self {
            Self::Standard => STANDARD_SIDEBAR_SECTIONS,
            Self::BrowserSession => BROWSER_SESSION_SIDEBAR_SECTIONS,
        }
    }

    /// Можно ли «живое» перетаскивание по таймлайну (кадры на лету во время drag).
    ///
    /// В браузерном окне — нет: каждая промежуточная цель drag вне полки источника
    /// просила бы страницу перемотаться (`seek` по мосту), и одно движение мыши
    /// превращалось бы в десятки перемоток страницы. Там drag коммитит одну точную
    /// перемотку на отпускании — одно действие зрителя = одна перемотка.
    pub(crate) const fn allows_live_scrub(self) -> bool {
        match self {
            Self::Standard => true,
            Self::BrowserSession => false,
        }
    }

    /// Текст пустого центра окна, пока медиа нет (плеер `Idle`).
    ///
    /// Браузерное окно появляется, когда страница открыла сеанс, а байты видео идут
    /// следом: честный текст — «ждём страницу», а не «откройте файл».
    pub(crate) const fn idle_center_hint(self) -> &'static str {
        match self {
            Self::Standard => "Open a file or URL to start",
            Self::BrowserSession => "Waiting for the video from the page…",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_window_keeps_every_region_of_the_normal_player() {
        let profile = WindowViewProfile::default();
        assert_eq!(profile, WindowViewProfile::Standard);
        assert!(profile.offers_media_opening());
        assert!(profile.offers_queue_navigation());
        assert!(profile.allows_live_scrub());
        assert_eq!(profile.sidebar_sections(), STANDARD_SIDEBAR_SECTIONS);
        assert_eq!(profile.idle_center_hint(), "Open a file or URL to start");
    }

    #[test]
    fn the_browser_window_has_no_queue_no_opening_and_no_url_section() {
        let profile = WindowViewProfile::BrowserSession;
        assert!(!profile.offers_media_opening());
        assert!(!profile.offers_queue_navigation());
        assert!(!profile.allows_live_scrub());
        assert_eq!(
            profile.sidebar_sections(),
            [SidebarSection::Settings, SidebarSection::Info]
        );
        assert!(!profile.idle_center_hint().contains("Open"));
    }
}
