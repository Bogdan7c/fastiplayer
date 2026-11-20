//! Настройки браузерного режима `[browser_session]` (схема v28, окно — v29, потолок памяти — v30,
//! срок возврата на секунду после смены варианта — v32, удержание кадра на смене медиа — v33):
//! сколько медиа источник «байты от страницы» держит вперёд, потолок его памяти, сколько окно
//! держит кадр прежнего ролика до первого кадра нового и запоминаемое окно браузерного режима
//! `[browser_session.window]`.
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
/// Сколько секунд после смены озвучки или качества плеер ждёт новое видео страницы, чтобы
/// вернуть его на ту же секунду (решение 280, сессия 14a моста). 120 с — с запасом на рекламу:
/// Ashdi показывает ролик около 35 с после смены озвучки.
pub const DEFAULT_BROWSER_VARIANT_RETURN_WINDOW_S: u16 = 120;
/// Минимум срока возврата: 10 с — меньше не хватает даже на открытие нового потока сайтом.
pub const MIN_BROWSER_VARIANT_RETURN_WINDOW_S: u16 = 10;
/// Максимум срока возврата: 10 минут. Дольше возврат мог бы перенести позицию на видео,
/// которое к выбору зрителя уже не относится.
pub const MAX_BROWSER_VARIANT_RETURN_WINDOW_S: u16 = 600;
/// Сколько миллисекунд окно браузерного режима держит последний кадр прежнего ролика, пока
/// новый не показал свой первый кадр (решение 380, сессия 16b3c моста). 3 с — с большим
/// запасом на установку нового поколения (на стенде — десятки миллисекунд, на слабой машине с
/// аппаратным декодером — до секунды) и всё же коротко: застывший кадр дольше выглядел бы
/// зависанием, а удержанный кадр держит память прежнего декодера.
pub const DEFAULT_BROWSER_MEDIA_SWITCH_FRAME_HOLD_MS: u16 = 3000;
/// Минимум: 0 — удержание выключено, между роликами пустое поле, как до 16b3c.
pub const MIN_BROWSER_MEDIA_SWITCH_FRAME_HOLD_MS: u16 = 0;
/// Максимум: 10 с. Дольше застывший кадр прежнего ролика уже обманывает зрителя.
pub const MAX_BROWSER_MEDIA_SWITCH_FRAME_HOLD_MS: u16 = 10_000;
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

    /// Срок возврата на ту же секунду после смены озвучки или качества (схема v32): сколько
    /// секунд плеер ждёт новое видео страницы (реклама между ними не считается видео).
    /// Читается при открытии окна браузерного режима.
    #[setting(
        id = "browser_session.variant_return_window_s",
        path = "browser_session.variant_return_window_s",
        section = "browser_mode",
        group = "variants",
        surface = "main-settings-window",
        label_id = "settings.browser_session.variant_return_window_s.label",
        label_ru = "Same second after a dub change",
        description_id = "settings.browser_session.variant_return_window_s.description",
        description_ru = "After you change the dub or quality, the page loads a new video. Fastiplayer puts it at the second you were watching if it arrives within this time; ads in between do not count. Takes effect with the next browser window.",
        help_id = "settings.browser_session.variant_return_window_s.help",
        help_ru = "Raise it if a site shows long ads after a dub change.",
        editor = "integer",
        min = crate::MIN_BROWSER_VARIANT_RETURN_WINDOW_S,
        max = crate::MAX_BROWSER_VARIANT_RETURN_WINDOW_S,
        step = 10,
        unit = "s",
        apply = "browser_session.apply"
    )]
    pub variant_return_window_s: u16,

    /// Предел удержания последнего кадра прежнего ролика на смене медиа, мс (схема v33,
    /// решение 380): 0 — не держать. Читается при открытии окна браузерного режима.
    #[setting(
        id = "browser_session.media_switch_frame_hold_ms",
        path = "browser_session.media_switch_frame_hold_ms",
        section = "browser_mode",
        group = "video",
        surface = "main-settings-window",
        label_id = "settings.browser_session.media_switch_frame_hold_ms.label",
        label_ru = "Keep last frame between videos",
        description_id = "settings.browser_session.media_switch_frame_hold_ms.description",
        description_ru = "When the page moves to the next video, the window keeps showing the last frame of the previous one until the new video shows its first frame, but no longer than this. 0 shows a black picture in between. Takes effect with the next browser window.",
        help_id = "settings.browser_session.media_switch_frame_hold_ms.help",
        help_ru = "Set it to 0 if a frozen frame between videos bothers you.",
        editor = "integer",
        min = crate::MIN_BROWSER_MEDIA_SWITCH_FRAME_HOLD_MS,
        max = crate::MAX_BROWSER_MEDIA_SWITCH_FRAME_HOLD_MS,
        step = 500,
        unit = "ms",
        apply = "browser_session.apply"
    )]
    pub media_switch_frame_hold_ms: u16,

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

    /// Срок возврата на ту же секунду после смены варианта (`variant_return_window_s`).
    pub fn variant_return_window(&self) -> Duration {
        Duration::from_secs(u64::from(self.variant_return_window_s))
    }

    /// Удержание кадра прежнего ролика на смене медиа (`media_switch_frame_hold_ms`).
    pub fn media_switch_frame_hold(&self) -> BrowserMediaSwitchFrameHold {
        match self.media_switch_frame_hold_ms {
            0 => BrowserMediaSwitchFrameHold::Disabled,
            hold_ms => BrowserMediaSwitchFrameHold::UpTo(Duration::from_millis(u64::from(hold_ms))),
        }
    }
}

impl Default for BrowserSessionConfig {
    /// Значения по умолчанию — константы выше (120 с вперёд, 320 МиБ потолок, 120 с возврата,
    /// 3 с удержания кадра) и окно 1280×720.
    fn default() -> Self {
        Self {
            buffer_ahead_s: DEFAULT_BROWSER_BUFFER_AHEAD_S,
            max_held_mb: DEFAULT_BROWSER_MAX_HELD_MB,
            variant_return_window_s: DEFAULT_BROWSER_VARIANT_RETURN_WINDOW_S,
            media_switch_frame_hold_ms: DEFAULT_BROWSER_MEDIA_SWITCH_FRAME_HOLD_MS,
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

/// Держит ли окно браузерного режима кадр прежнего ролика на смене медиа и сколько.
///
/// Значимый тип вместо «0 = выключено» на месте вызова.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserMediaSwitchFrameHold {
    /// Не держать: между роликами пустое поле.
    Disabled,
    /// Держать до первого кадра нового ролика, но не дольше этого.
    UpTo(Duration),
}
