//! Контракт UI assets без привязки к текущей минимальной отрисовке.

/// Идентификатор иконки, которую skin может отдать как текстовую fallback-метку.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconId {
    /// Команда запуска playback.
    Play,

    /// Команда паузы playback.
    Pause,

    /// Иконка громкости.
    Volume,
}

/// Провайдер assets для skin-а.
pub trait AssetProvider {
    /// Возвращает текущую текстовую fallback-метку иконки.
    #[must_use]
    fn icon_text(&self, icon_id: IconId) -> &'static str;
}
