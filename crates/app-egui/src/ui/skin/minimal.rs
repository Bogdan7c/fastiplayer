//! Минимальный player-style skin.

use egui::{Color32, Margin};

use crate::ui::animation::AnimationState;
use crate::ui::assets::{AssetProvider, IconId};
use crate::ui::skin::{
    ControlsStyle, PersistentControlStyle, PlayerSkin, PlaylistHeaderUndoStyle, PlaylistRowStyle,
    PlaylistToolbarStyle, SkinId, TimelineStyle,
};

/// Первый production skin для desktop player controls.
#[derive(Debug, Clone, Copy, Default)]
pub struct MinimalSkin;

impl AssetProvider for MinimalSkin {
    /// Возвращает текстовую fallback-иконку до подключения SVG/texture assets.
    fn icon_text(&self, icon_id: IconId) -> &'static str {
        match icon_id {
            IconId::Play => "Play",
            IconId::Pause => "Pause",
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
            track_outline_width: 2.0,
            track_outline_fill: Color32::from_rgb(18, 18, 18),
            thumb_outline_width: 2.0,
            thumb_outline_fill: Color32::from_rgb(18, 18, 18),
            disabled_fill: Color32::from_gray(48),
        }
    }

    /// Возвращает размеры и цвета панелей controls.
    fn controls_style(&self) -> ControlsStyle {
        // Один text color остаётся источником normal и приглушённого transport-состояния.
        let text_color = Color32::from_gray(230);
        // Центральная и transport-кнопки используют общий язык hover-подсветки.
        let button_hover_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 28);

        ControlsStyle {
            top_panel_fill: Color32::from_rgba_unmultiplied(0, 0, 0, 145),
            bottom_panel_fill: Color32::from_rgba_unmultiplied(0, 0, 0, 190),
            text_color,
            button_width: 72.0,
            button_height: 28.0,
            playback_button_diameter: 48.0,
            playback_button_icon_extent: 18.0,
            playback_button_stroke_width: 1.6,
            playback_button_vertical_raise: 5.0,
            left_edge_control_center_step: 40.0,
            playback_button_hover_fill: button_hover_fill,
            transport_button_size: 32.0,
            transport_button_center_distance: 64.0,
            transport_button_icon_extent: 18.0,
            transport_button_bar_width: 2.0,
            transport_button_disabled_color: text_color.gamma_multiply(0.4),
            transport_button_hover_fill: button_hover_fill,
            persistent_control: PersistentControlStyle {
                foreground_idle: Color32::from_gray(170),
                foreground_hover: Color32::from_gray(230),
                foreground_active: Color32::from_gray(245),
                foreground_disabled: Color32::from_gray(105),
                surface_idle: Color32::TRANSPARENT,
                surface_hover: Color32::from_rgba_unmultiplied(255, 255, 255, 28),
                surface_active: Color32::from_rgba_unmultiplied(255, 255, 255, 25),
                surface_active_hover: Color32::from_rgba_unmultiplied(255, 255, 255, 46),
                surface_pressed: Color32::from_rgba_unmultiplied(255, 255, 255, 56),
                focus_outline: Color32::from_rgba_unmultiplied(245, 245, 245, 220),
            },
            queue_mode_button_center_distance: 156.0,
            queue_mode_neighbor_gap: 12.0,
            queue_mode_glyph_stroke_width: 1.6,
            queue_mode_focus_outline_width: 1.5,
            queue_mode_focus_inset: 1.5,
            playback_rate_button_width: 48.0,
            playback_rate_button_gap: 5.0,
            playback_rate_button_vertical_inset: 2.0,
            playback_rate_button_stroke_width: 1.2,
            fullscreen_button_size: 32.0,
            fullscreen_icon_extent: 16.0,
            volume_slider_width: 96.0,
            panel_margin: Margin::symmetric(10, 6),
        }
    }

    /// Возвращает белые Playlist row tokens без platform selection accent.
    fn playlist_row_style(&self) -> PlaylistRowStyle {
        let light_stroke = Color32::from_rgba_unmultiplied(245, 245, 245, 220);
        PlaylistRowStyle {
            hover_fill: Color32::from_rgba_unmultiplied(255, 255, 255, 28),
            selected_fill: Color32::from_rgba_unmultiplied(255, 255, 255, 46),
            selected_hover_fill: Color32::from_rgba_unmultiplied(255, 255, 255, 64),
            active_fill: Color32::from_rgba_unmultiplied(255, 255, 255, 18),
            separator_color: Color32::from_rgba_unmultiplied(255, 255, 255, 128),
            insertion_stroke: egui::Stroke::new(1.5, light_stroke),
            focus_stroke: egui::Stroke::new(1.0, light_stroke),
            active_stroke: egui::Stroke::new(
                1.0,
                Color32::from_rgba_unmultiplied(235, 235, 235, 180),
            ),
        }
    }

    /// Возвращает нейтральный Undo без постоянной подложки в масштабе heading.
    fn playlist_header_undo_style(&self) -> PlaylistHeaderUndoStyle {
        PlaylistHeaderUndoStyle {
            hit_area_size: 32.0,
            glyph_text_style: egui::TextStyle::Heading,
            glyph_stroke_width: 2.0,
            surface_corner_radius: 4.0,
            foreground_idle: Color32::from_gray(230),
            foreground_hover: Color32::from_gray(245),
            foreground_disabled: Color32::from_gray(105),
            surface_hover: Color32::from_rgba_unmultiplied(255, 255, 255, 28),
            surface_pressed: Color32::from_rgba_unmultiplied(255, 255, 255, 56),
            focus_outline: egui::Stroke::new(
                1.5,
                Color32::from_rgba_unmultiplied(245, 245, 245, 220),
            ),
            focus_inset: 1.5,
        }
    }

    /// Возвращает контрастный grayscale toolbar в масштабе нижних transport-кнопок.
    fn playlist_toolbar_style(&self) -> PlaylistToolbarStyle {
        let controls_style = self.controls_style();
        let button_size = 32.0;
        let first_button_center_inset =
            controls_style.left_edge_control_first_center_inset_points();
        let button_center_step = controls_style.left_edge_control_center_step;

        PlaylistToolbarStyle {
            button_size,
            button_gap: (button_center_step - button_size).max(0.0),
            left_group_padding: (first_button_center_inset - button_size * 0.5).max(0.0),
            clear_right_padding: 18.0,
            icon_extent: 23.5,
            glyph_stroke_width: 1.6,
            surface_corner_radius: 4.0,
            foreground_idle: Color32::from_gray(230),
            foreground_hover: Color32::from_gray(245),
            foreground_disabled: Color32::from_gray(105),
            surface_hover: Color32::from_rgba_unmultiplied(255, 255, 255, 28),
            surface_pressed: Color32::from_rgba_unmultiplied(255, 255, 255, 56),
            focus_outline: egui::Stroke::new(
                1.5,
                Color32::from_rgba_unmultiplied(245, 245, 245, 220),
            ),
            focus_inset: 1.5,
        }
    }

    /// Возвращает overlay-цвет для stale frame.
    fn stale_frame_dim_color(&self, animation_state: AnimationState) -> Option<Color32> {
        animation_state
            .dim_stale_frame
            .then_some(Color32::from_rgba_unmultiplied(0, 0, 0, 96))
    }
}
