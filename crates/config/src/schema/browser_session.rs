//! Настройки браузерного режима `[browser_session]` (схема v28): сколько медиа источник
//! «байты от страницы» держит вперёд и позади позиции воспроизведения.
//!
//! Браузерный режим делит config с обычным плеером (решение 46): процесс браузерной сессии
//! только читает этот файл. Источник копит полученные от страницы коробки fMP4/WebM:
//! когда запас вперёд достигает `buffer_ahead_s`, источник считается «полным» и просит
//! страницу не слать больше; позади позиции хранится `buffer_behind_s`, чтобы короткая
//! перемотка назад не уходила к странице.
//!
//! Виджета в окне настроек у секции нет (визуальная доводка UI отложена), поэтому
//! `SettingsSchema` возвращает пустой реестр: значения меняются только правкой файла.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use settings_core::{SettingsRegistry, SettingsResult, SettingsSchema};

/// Запас вперёд, секунд, по умолчанию (решение владельца). Порядок величины совпадает
/// с тем, что держат сами сайты: YouTube в фоне около 118 с, Tortuga 120 с.
pub const DEFAULT_BROWSER_BUFFER_AHEAD_S: u16 = 120;
/// Минимум запаса вперёд: 10 с — меньше не переживает паузу между сегментами живых потоков
/// (Twitch присылает сегмент раз в ~2 с, но сеть может задержать несколько подряд).
pub const MIN_BROWSER_BUFFER_AHEAD_S: u16 = 10;
/// Максимум запаса вперёд: 10 минут. Больше — лишняя память без пользы для перемотки.
pub const MAX_BROWSER_BUFFER_AHEAD_S: u16 = 600;

/// Сколько секунд держать позади позиции, по умолчанию (решение владельца).
pub const DEFAULT_BROWSER_BUFFER_BEHIND_S: u16 = 30;
/// Минимум: 0 — прошлое сразу выбрасывается, любая перемотка назад идёт к странице.
pub const MIN_BROWSER_BUFFER_BEHIND_S: u16 = 0;
/// Максимум: 5 минут.
pub const MAX_BROWSER_BUFFER_BEHIND_S: u16 = 300;

/// Секция `[browser_session]`: окно буфера источника браузерного режима.
///
/// Поля хранятся в секундах, как соседние окна `subtitles.stream.ahead_s/behind_s`.
/// Потребителям следует брать типизированное окно через [`BrowserSessionConfig::buffer_window`],
/// а не читать числа напрямую.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserSessionConfig {
    /// Порог «полон»: сколько секунд медиа вперёд от позиции копит источник.
    pub buffer_ahead_s: u16,

    /// Сколько секунд уже сыгранного медиа источник хранит позади позиции.
    pub buffer_behind_s: u16,
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
    /// Значения по умолчанию — константы выше (120 с вперёд, 30 с назад).
    fn default() -> Self {
        Self {
            buffer_ahead_s: DEFAULT_BROWSER_BUFFER_AHEAD_S,
            buffer_behind_s: DEFAULT_BROWSER_BUFFER_BEHIND_S,
        }
    }
}

impl SettingsSchema for BrowserSessionConfig {
    /// Секция не показывается в окне настроек: реестр намеренно пуст.
    ///
    /// Трансакции настроек копируют `AppConfig` целиком, поэтому значения из файла
    /// переживают «Применить» в окне настроек без отдельной регистрации.
    fn settings_registry() -> SettingsResult<SettingsRegistry<Self>> {
        Ok(SettingsRegistry::empty())
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
    /// Сколько хранить позади позиции.
    behind: Duration,
}

impl BrowserSessionBufferWindow {
    /// Запас вперёд, при достижении которого источник «полон» и не берёт новые байты.
    pub fn ahead(&self) -> Duration {
        self.ahead
    }

    /// Сколько уже сыгранного медиа хранить позади позиции воспроизведения.
    pub fn behind(&self) -> Duration {
        self.behind
    }
}
