//! Фиксированные имена MPRIS обычного экземпляра плеера.
//!
//! Одни и те же константы нужны и серверу MPRIS (`platform::linux`), и клиенту паузы
//! (`player_pause`): расхождение имён означало бы, что пауза уходит «не туда».

/// Единственное process base name: fallback suffix и late retry запрещены D78.
pub(crate) const MPRIS_BUS_NAME: &str = "org.mpris.MediaPlayer2.fastiplayer";
/// Объект MPRIS по спецификации.
pub(crate) const MPRIS_OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
/// Интерфейс `Player` (`Pause`, `Play`, …).
pub(crate) const MPRIS_PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
