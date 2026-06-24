//! Минимальный player-style skin.

use egui::{Color32, Margin};

use crate::ui::animation::AnimationState;
use crate::ui::assets::{AssetProvider, IconId};
use crate::ui::skin::{ControlsStyle, PlayerSkin, SkinId, TimelineStyle};

/// Первый production skin для desktop player controls.
#[derive(Debug, Clone, Copy, Default)]
pub struct MinimalSkin;

impl AssetProvider for MinimalSkin {
    /// Возвращает текстовую fallback-иконку до подключения SVG/texture assets.
    fn icon_text(&self, icon_id: IconId) -> &'static str {
        match icon_id {
            IconId::Play => "Play",
            IconId::Pause => "Pause",
            IconId::OpenFile => "Open",
            IconId::Fullscreen => "Full",
            IconId::Volume => "Vol",
        }
    }
}

impl PlayerSkin for MinimalSkin {
    /// Возвращает typed id минимального skin-а.
    fn id(&self) -> SkinId {
        SkinId::Minimal
    }

    /// Возвращает размеры и цвета timeline.
    fn timeline_style(&self) -> TimelineStyle {
        TimelineStyle {
            hit_height: 28.0,
            track_height: 5.0,
            thumb_radius: 6.0,
            horizontal_padding: 8.0,
            track_fill: Color32::from_gray(64),
            played_fill: Color32::from_rgb(220, 220, 220),
            target_fill: Color32::from_rgb(130, 190, 255),
            thumb_fill: Color32::WHITE,
            disabled_fill: Color32::from_gray(48),
        }
    }

    /// Возвращает размеры и цвета панелей controls.
    fn controls_style(&self) -> ControlsStyle {
        ControlsStyle {
            top_panel_fill: Color32::from_rgba_unmultiplied(0, 0, 0, 145),
            bottom_panel_fill: Color32::from_rgba_unmultiplied(0, 0, 0, 190),
            text_color: Color32::from_gray(230),
            button_width: 72.0,
            button_height: 28.0,
            playback_button_diameter: 48.0,
            playback_button_icon_extent: 18.0,
            playback_button_stroke_width: 1.6,
            playback_button_vertical_raise: 5.0,
            playback_button_hover_fill: Color32::from_rgba_unmultiplied(255, 255, 255, 28),
            volume_slider_width: 96.0,
            panel_margin: Margin::symmetric(10, 6),
        }
    }

    /// Возвращает overlay-цвет для stale frame.
    fn stale_frame_dim_color(&self, animation_state: AnimationState) -> Option<Color32> {
        animation_state
            .dim_stale_frame
            .then_some(Color32::from_rgba_unmultiplied(0, 0, 0, 96))
    }
}
