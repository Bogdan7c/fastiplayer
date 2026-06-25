//! Контракт UI assets без привязки к текущей минимальной отрисовке.

/// Идентификатор иконки, которую skin может отдать как текст, SVG или texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconId {
    /// Команда запуска playback.
    Play,

    /// Команда паузы playback.
    Pause,

    /// Команда открытия media-файла.
    OpenFile,

    /// Иконка громкости.
    Volume,
}

/// Идентификатор будущего skin asset-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetId {
    /// Asset является иконкой.
    Icon(IconId),
}

/// Провайдер assets для skin-а.
pub trait AssetProvider {
    /// Возвращает стабильный asset id для указанной иконки.
    #[must_use]
    fn icon_asset(&self, icon_id: IconId) -> AssetId {
        AssetId::Icon(icon_id)
    }

    /// Возвращает текущую текстовую fallback-метку иконки.
    #[must_use]
    fn icon_text(&self, icon_id: IconId) -> &'static str;
}
