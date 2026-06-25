//! Skin boundary для controls/timeline.
//!
//! `AppState` выбирает skin по `ui.skin`, а конкретные виджеты используют только
//! этот контракт. Так будущие SVG, texture assets и анимации не потребуют менять
//! command/snapshot wiring.

pub mod minimal;

use egui::{Color32, Frame, Margin};

use crate::ui::animation::AnimationState;
use crate::ui::assets::AssetProvider;

pub use minimal::MinimalSkin;

/// Единственный skin id, который текущая schema version поддерживает.
pub const MINIMAL_SKIN_ID: &str = "minimal";

/// Typed skin id после validation config-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinId {
    /// Минимальный player-style skin.
    Minimal,
}

impl SkinId {
    /// Парсит skin id из config.
    #[must_use]
    pub fn parse(raw_skin_id: &str) -> Option<Self> {
        match raw_skin_id.trim() {
            MINIMAL_SKIN_ID => Some(Self::Minimal),
            _ => None,
        }
    }
}

/// Цвета и размеры timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineStyle {
    /// Высота интерактивной области timeline.
    pub hit_height: f32,

    /// Толщина фоновой дорожки.
    pub track_height: f32,

    /// Радиус бегунка.
    pub thumb_radius: f32,

    /// Горизонтальный отступ дорожки внутри hit area.
    pub horizontal_padding: f32,

    /// Цвет фона timeline.
    pub track_fill: Color32,

    /// Цвет пройденной части timeline.
    pub played_fill: Color32,

    /// Цвет target-позиции во время scrub.
    pub target_fill: Color32,

    /// Цвет бегунка.
    pub thumb_fill: Color32,

    /// Цвет disabled timeline.
    pub disabled_fill: Color32,
}

/// Цвета и размеры панели controls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlsStyle {
    /// Фон верхней панели.
    pub top_panel_fill: Color32,

    /// Фон нижней панели.
    pub bottom_panel_fill: Color32,

    /// Цвет текста normal controls.
    pub text_color: Color32,

    /// Ширина компактной кнопки.
    pub button_width: f32,

    /// Высота компактной кнопки.
    pub button_height: f32,

    /// Диаметр центральной круглой play/pause-кнопки.
    pub playback_button_diameter: f32,

    /// Размер glyph внутри центральной play/pause-кнопки.
    pub playback_button_icon_extent: f32,

    /// Толщина обводки центральной play/pause-кнопки.
    pub playback_button_stroke_width: f32,

    /// Смещение центральной play/pause-кнопки вверх относительно строки controls.
    pub playback_button_vertical_raise: f32,

    /// Заливка центральной play/pause-кнопки при hover.
    pub playback_button_hover_fill: Color32,

    /// Размер квадратной fullscreen-кнопки в нижней панели.
    pub fullscreen_button_size: f32,

    /// Размер hand-drawn fullscreen glyph внутри кнопки.
    pub fullscreen_icon_extent: f32,

    /// Ширина volume slider.
    pub volume_slider_width: f32,

    /// Внутренний отступ панели.
    pub panel_margin: Margin,
}

/// Общий контракт skin-а.
pub trait PlayerSkin: AssetProvider {
    /// Возвращает typed id skin-а.
    #[must_use]
    fn id(&self) -> SkinId;

    /// Возвращает стиль timeline.
    #[must_use]
    fn timeline_style(&self) -> TimelineStyle;

    /// Возвращает стиль controls.
    #[must_use]
    fn controls_style(&self) -> ControlsStyle;

    /// Возвращает frame нижней панели.
    fn bottom_panel_frame(&self) -> Frame {
        Frame::NONE
            .fill(self.controls_style().bottom_panel_fill)
            .inner_margin(self.controls_style().panel_margin)
    }

    /// Возвращает цвет затемнения stale video frame.
    #[must_use]
    fn stale_frame_dim_color(&self, animation_state: AnimationState) -> Option<Color32>;
}

/// Выбирает skin по validated config id.
#[must_use]
pub fn skin_from_config(raw_skin_id: &str) -> Option<MinimalSkin> {
    SkinId::parse(raw_skin_id).map(|skin_id| match skin_id {
        SkinId::Minimal => MinimalSkin,
    })
}

#[cfg(test)]
mod tests {
    use super::{MINIMAL_SKIN_ID, SkinId, skin_from_config};
    use crate::ui::skin::PlayerSkin;

    /// Проверяет, что config id `minimal` выбирает первый skin.
    #[test]
    fn minimal_config_id_selects_minimal_skin() {
        let skin = skin_from_config(MINIMAL_SKIN_ID).expect("minimal skin resolved");

        assert_eq!(skin.id(), SkinId::Minimal);
    }

    /// Проверяет, что неизвестный id не считается валидным skin-ом UI.
    #[test]
    fn unknown_skin_id_is_not_resolved() {
        assert_eq!(SkinId::parse("dense"), None);
    }
}
