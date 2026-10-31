//! Настройки браузерного режима `[browser_session]` (схема v28, окно — v29): сколько медиа
//! источник «байты от страницы» держит вперёд и позади позиции воспроизведения, и
//! запоминаемое окно браузерного режима `[browser_session.window]`.
//!
//! Браузерный режим делит config с обычным плеером (решение 46); с 04c обе копии пишут
//! его как «общую доску» под замком (решение 135, `store/shared_file.rs`). Источник копит полученные от страницы коробки fMP4/WebM:
//! когда запас вперёд достигает `buffer_ahead_s`, источник считается «полным» и просит
//! страницу не слать больше. Прошлое источник хранит, пока его хранит сама страница (она
//! не пришлёт повторно то, что лежит в её буфере): уходит оно, когда страница стёрла его у
//! себя, или при нехватке памяти — и тогда `buffer_behind_s` прошлого у позиции остаётся
//! всё равно, чтобы короткая перемотка назад не уходила к странице.
//!
//! С 04c моста (решение 96) окно буфера — обычные настройки окна настроек: секция
//! «browser_mode», маршрут `browser_session.apply`. Новое значение действует на следующее
//! видео со страницы (источник уже идущего потока не перестраивается); обычному плееру
//! маршрут ничего не меняет.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::browser_session_window::BrowserWindowConfig;

/// Запас вперёд, секунд, по умолчанию (решение владельца). Порядок величины совпадает
/// с тем, что держат сами сайты: YouTube в фоне около 118 с, Tortuga 120 с.
pub const DEFAULT_BROWSER_BUFFER_AHEAD_S: u16 = 120;
/// Минимум запаса вперёд: 10 с — меньше не переживает паузу между сегментами живых потоков
/// (Twitch присылает сегмент раз в ~2 с, но сеть может задержать несколько подряд).
pub const MIN_BROWSER_BUFFER_AHEAD_S: u16 = 10;
/// Максимум запаса вперёд: 10 минут. Больше — лишняя память без пользы для перемотки.
pub const MAX_BROWSER_BUFFER_AHEAD_S: u16 = 600;

/// Сколько секунд прошлого держать позади позиции даже при нехватке памяти, по умолчанию
/// (решение владельца).
pub const DEFAULT_BROWSER_BUFFER_BEHIND_S: u16 = 30;
/// Минимум: 0 — при нехватке памяти сыгранное может уйти сразу (без нехватки прошлое всё
/// равно хранится, пока его хранит страница).
pub const MIN_BROWSER_BUFFER_BEHIND_S: u16 = 0;
/// Максимум: 5 минут.
pub const MAX_BROWSER_BUFFER_BEHIND_S: u16 = 300;

/// Секция `[browser_session]`: окно буфера источника браузерного режима.
///
/// Поля хранятся в секундах, как соседние окна `subtitles.stream.ahead_s/behind_s`.
/// Потребителям следует брать типизированное окно через [`BrowserSessionConfig::buffer_window`],
/// а не читать числа напрямую.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserSessionConfig {
    /// Порог «полон»: сколько секунд медиа вперёд от позиции копит источник.
    #[setting(
        id = "browser_session.buffer_ahead_s",
        path = "browser_session.buffer_ahead_s",
        section = "browser_mode",
        group = "buffer",
        surface = "main-settings-window",
        label_id = "settings.browser_session.buffer_ahead_s.label",
        label_ru = "Buffer ahead",
        description_id = "settings.browser_session.buffer_ahead_s.description",
        description_ru = "How much video to keep downloaded ahead of the playback position when Fastiplayer plays a video from a web page. Takes effect with the next video from the page.",
        help_id = "settings.browser_session.buffer_ahead_s.help",
        help_ru = "When this much is buffered, Fastiplayer asks the page to pause downloading.",
        editor = "integer",
        min = crate::MIN_BROWSER_BUFFER_AHEAD_S,
        max = crate::MAX_BROWSER_BUFFER_AHEAD_S,
        step = 10,
        unit = "s",
        apply = "browser_session.apply"
    )]
    pub buffer_ahead_s: u16,

    /// Сколько секунд уже сыгранного медиа источник хранит позади позиции даже при нехватке
    /// памяти (гарантированный минимум, а не окно выселения).
    #[setting(
        id = "browser_session.buffer_behind_s",
        path = "browser_session.buffer_behind_s",
        section = "browser_mode",
        group = "buffer",
        surface = "main-settings-window",
        label_id = "settings.browser_session.buffer_behind_s.label",
        label_ru = "Keep behind",
        description_id = "settings.browser_session.buffer_behind_s.description",
        description_ru = "How much of the past to keep even when memory is low, so a short rewind plays at once. Takes effect with the next video from the page.",
        help_id = "settings.browser_session.buffer_behind_s.help",
        help_ru = "Otherwise the past is kept as long as the page keeps it.",
        editor = "integer",
        min = crate::MIN_BROWSER_BUFFER_BEHIND_S,
        max = crate::MAX_BROWSER_BUFFER_BEHIND_S,
        step = 5,
        unit = "s",
        apply = "browser_session.apply"
    )]
    pub buffer_behind_s: u16,

    /// Запоминаемое окно браузерного режима (схема v29): размер и полноэкранность.
    #[serde(default)]
    #[setting(nested)]
    pub window: BrowserWindowConfig,
}

impl BrowserSessionConfig {
    /// Возвращает окно буфера источника в единицах времени.
    ///
    /// Значения не перепроверяются: config, прошедший `AppConfig::validate`, уже держит
    /// их в допустимых диапазонах.
    pub fn buffer_window(&self) -> BrowserSessionBufferWindow {
        BrowserSessionBufferWindow {
            ahead: Duration::from_secs(u64::from(self.buffer_ahead_s)),
            behind: Duration::from_secs(u64::from(self.buffer_behind_s)),
        }
    }
}

impl Default for BrowserSessionConfig {
    /// Значения по умолчанию — константы выше (120 с вперёд, 30 с назад) и окно 1280×720.
    fn default() -> Self {
        Self {
            buffer_ahead_s: DEFAULT_BROWSER_BUFFER_AHEAD_S,
            buffer_behind_s: DEFAULT_BROWSER_BUFFER_BEHIND_S,
            window: BrowserWindowConfig::default(),
        }
    }
}

/// Окно буфера источника браузерного режима: сколько держать вперёд и позади позиции.
///
/// Значимый тип вместо пары чисел, чтобы на месте вызова нельзя было перепутать
/// «вперёд» и «назад» или единицы измерения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserSessionBufferWindow {
    /// Порог «полон» вперёд от позиции.
    ahead: Duration,
    /// Сколько прошлого хранить позади позиции даже при нехватке памяти.
    behind: Duration,
}

impl BrowserSessionBufferWindow {
    /// Запас вперёд, при достижении которого источник «полон» и не берёт новые байты.
    pub fn ahead(&self) -> Duration {
        self.ahead
    }

    /// Сколько уже сыгранного медиа хранить позади позиции воспроизведения даже при нехватке
    /// памяти (остальное прошлое источник хранит, пока его хранит страница).
    pub fn behind(&self) -> Duration {
        self.behind
    }
}
