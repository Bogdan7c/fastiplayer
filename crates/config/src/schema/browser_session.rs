//! Настройки браузерного режима `[browser_session]` (схема v28, окно — v29, потолок памяти — v30): сколько
//! медиа источник «байты от страницы» держит вперёд, потолок его памяти и
//! запоминаемое окно браузерного режима `[browser_session.window]`.
//!
//! Браузерный режим делит config с обычным плеером (решение 46); с 04c обе копии пишут
//! его как «общую доску» под замком (решение 135, `store/shared_file.rs`). Источник копит полученные от страницы коробки fMP4/WebM:
//! когда запас вперёд достигает `buffer_ahead_s`, источник считается «полным» и просит
//! страницу не слать больше. Прошлое источник хранит, пока его хранит сама страница (она
//! не пришлёт повторно то, что лежит в её буфере): уходит оно, когда страница стёрла его у
//! себя, или когда не хватает памяти (`max_held_mb`) — тогда выкидывается, как в браузере.
//! Отдельного окна «назад» нет (до схемы v30 был `buffer_behind_s`, миграция его удаляет).
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

/// Потолок памяти одного потока моста, МиБ, по умолчанию. Источник — «зеркало браузера»:
/// браузер держит до 150 МиБ видео и 12–20 МиБ звука на дорожку; 320 МиБ дают видео
/// 216 МиБ (запас 44 %, замер YouTube 4K60 1.11.2026).
pub const DEFAULT_BROWSER_MAX_HELD_MB: u16 = 320;
/// Минимум потолка: 64 МиБ — меньше не вмещает даже один сегмент 4K.
pub const MIN_BROWSER_MAX_HELD_MB: u16 = 64;
/// Максимум потолка: 2 ГиБ.
pub const MAX_BROWSER_MAX_HELD_MB: u16 = 2048;
/// Байт в мебибайте (перевод `max_held_mb` в байты).
const BYTES_PER_MIB: usize = 1024 * 1024;

/// Секция `[browser_session]`: окно буфера источника браузерного режима.
///
/// Запас вперёд хранится в секундах, как у соседнего окна `subtitles.stream.ahead_s`.
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

    /// Жёсткий потолок памяти одного потока моста, МиБ. Порог «полон» и нормы по дорожкам
    /// считаются от него (75 %). Когда памяти не хватает, источник выкидывает сыгранное,
    /// как браузер. Смена значения действует только на НОВЫЕ потоки (следующее видео со
    /// страницы); идущий поток своего потолка не меняет.
    #[setting(
        id = "browser_session.max_held_mb",
        path = "browser_session.max_held_mb",
        section = "browser_mode",
        group = "buffer",
        surface = "main-settings-window",
        label_id = "settings.browser_session.max_held_mb.label",
        label_ru = "Memory limit",
        description_id = "settings.browser_session.max_held_mb.description",
        description_ru = "Hard memory limit for one video from a web page. When it runs low, Fastiplayer drops the already played part, like a browser does. Takes effect with the next video from the page.",
        help_id = "settings.browser_session.max_held_mb.help",
        help_ru = "Raise it only if 4K videos from a page stutter.",
        editor = "integer",
        min = crate::MIN_BROWSER_MAX_HELD_MB,
        max = crate::MAX_BROWSER_MAX_HELD_MB,
        step = 32,
        unit = "MiB",
        apply = "browser_session.apply"
    )]
    pub max_held_mb: u16,

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
            max_held_bytes: usize::from(self.max_held_mb) * BYTES_PER_MIB,
        }
    }
}

impl Default for BrowserSessionConfig {
    /// Значения по умолчанию — константы выше (120 с вперёд, 320 МиБ потолок) и окно 1280×720.
    fn default() -> Self {
        Self {
            buffer_ahead_s: DEFAULT_BROWSER_BUFFER_AHEAD_S,
            max_held_mb: DEFAULT_BROWSER_MAX_HELD_MB,
            window: BrowserWindowConfig::default(),
        }
    }
}

/// Окно буфера источника браузерного режима: сколько держать вперёд и потолок памяти потока.
///
/// Значимый тип вместо пары чисел, чтобы на месте вызова нельзя было перепутать
/// секунды и байты.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserSessionBufferWindow {
    /// Порог «полон» вперёд от позиции.
    ahead: Duration,
    /// Жёсткий потолок памяти одного потока, байт.
    max_held_bytes: usize,
}

impl BrowserSessionBufferWindow {
    /// Запас вперёд, при достижении которого источник «полон» и не берёт новые байты.
    pub fn ahead(&self) -> Duration {
        self.ahead
    }

    /// Жёсткий потолок памяти одного потока, байт (`max_held_mb` × МиБ). Для config,
    /// прошедшего `AppConfig::validate`, больше нуля.
    pub fn max_held_bytes(&self) -> usize {
        self.max_held_bytes
    }
}
