//! Имена MPRIS и «личности» серверов (обычный плеер и окно браузерного режима).
//!
//! Одни и те же константы нужны и серверу MPRIS (`platform::linux`), и клиенту паузы
//! (`player_pause`): расхождение имён означало бы, что пауза уходит «не туда».
//!
//! Личностей две (04c). Обычный плеер занимает `org.mpris.MediaPlayer2.fastiplayer`, и
//! клиент паузы (решение 89) зовёт `Pause` строго на нём. Окно браузерного режима живёт в
//! другом процессе и занимает СВОЁ имя `…fastiplayer.browser`: оба видны в медиа-виджете
//! рабочего стола одновременно, и пауза обычного плеера браузерное окно не трогает.

/// Единственное process base name: fallback suffix и late retry запрещены D78.
#[cfg(target_os = "linux")]
pub(crate) const MPRIS_BUS_NAME: &str = "org.mpris.MediaPlayer2.fastiplayer";
/// Имя на шине окна браузерного режима (отдельный процесс, решение 123).
#[cfg(target_os = "linux")]
pub(crate) const MPRIS_BROWSER_BUS_NAME: &str = "org.mpris.MediaPlayer2.fastiplayer.browser";
/// Объект MPRIS по спецификации.
#[cfg(target_os = "linux")]
pub(crate) const MPRIS_OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
/// Интерфейс `Player` (`Pause`, `Play`, …).
#[cfg(target_os = "linux")]
pub(crate) const MPRIS_PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

/// Чей MPRIS-сервер поднимается: от этого зависят имя на шине, `Identity` и то, умеет ли
/// корневой интерфейс `Raise`/`Quit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MprisIdentity {
    /// Обычный плеер: имя `…fastiplayer`, `Identity` «Fastiplayer», `Raise`/`Quit` нет.
    #[default]
    StandardPlayer,
    /// Окно браузерного режима: имя `…fastiplayer.browser`, `Identity`
    /// «Fastiplayer (browser)», `Raise` и `Quit` есть (окно можно поднять и закрыть).
    BrowserSession,
}

impl MprisIdentity {
    /// Имя на шине сеансов.
    #[cfg(target_os = "linux")]
    pub(crate) const fn bus_name(self) -> &'static str {
        match self {
            Self::StandardPlayer => MPRIS_BUS_NAME,
            Self::BrowserSession => MPRIS_BROWSER_BUS_NAME,
        }
    }

    /// Свойство `Identity` корневого интерфейса: так сервер подписан в медиа-виджете.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::StandardPlayer => "Fastiplayer",
            Self::BrowserSession => "Fastiplayer (browser)",
        }
    }

    /// Просит ли `Raise`/`Quit` с шины исполнять (`CanRaise`/`CanQuit`).
    #[must_use]
    pub const fn accepts_window_requests(self) -> bool {
        match self {
            Self::StandardPlayer => false,
            Self::BrowserSession => true,
        }
    }
}
