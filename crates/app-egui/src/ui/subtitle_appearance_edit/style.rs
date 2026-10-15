//! Токены внешнего вида режима правки; значения задаёт skin (`PlayerSkin`).

use egui::{Color32, Stroke};
use ui_artwork_egui::SubtitleEditFrameStyle;

/// Цвета и размеры рамки слоя и панели настроек.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SubtitleAppearanceEditStyle {
    /// Пунктир и маркеры рамки (отдаются в `ui-artwork-egui`).
    pub(crate) frame: SubtitleEditFrameStyle,
    /// Запас рамки вокруг слоя, points: рамка не должна лежать вплотную к буквам.
    pub(crate) frame_padding: f32,
    /// Сторона зоны нажатия маркера, points; больше нарисованного маркера, чтобы легче попасть.
    pub(crate) handle_hit_extent: f32,
    /// Отступ панели от верха свободной области видео, points.
    pub(crate) panel_top_margin: f32,
    /// Заливка панели.
    pub(crate) panel_fill: Color32,
    /// Контур панели.
    pub(crate) panel_stroke: Stroke,
    /// Скругление панели, points.
    pub(crate) panel_corner_radius: f32,
    /// Внутренний отступ панели, points.
    pub(crate) panel_inner_margin: f32,
}
