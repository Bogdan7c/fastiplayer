//! Тесты кнопки аудиодорожек: чистая раскладка рядом с CC и настоящий egui-кадр с кликами.

mod layout;
mod widget;

use egui::{Rect, pos2, vec2};

use super::super::subtitle_control::{
    SubtitleLayoutGeometry, SubtitleNeighbor, TestSubtitleControlMode, choose_subtitle_mode,
};
use crate::ui::skin::{MinimalSkin, PlayerSkin};

/// Обычный spacing egui между соседними widgets.
pub(super) const TEST_ITEM_SPACING: f32 = 8.0;

/// Геометрия строки для окна заданной ширины (поля панели 10 pt с каждой стороны).
pub(super) fn geometry_for_window_width(window_width: f32) -> SubtitleLayoutGeometry {
    let controls_style = MinimalSkin.controls_style();
    let row_rect = Rect::from_min_size(
        pos2(10.0, 0.0),
        vec2(window_width - 20.0, controls_style.playback_button_diameter),
    );
    let playback_button_rect =
        crate::ui::player_controls::playback_button_anchor_rect(row_rect, controls_style);
    SubtitleLayoutGeometry {
        playback_button_rect,
        base_next_button_rect: crate::ui::player_controls::transport::next_button_rect(
            playback_button_rect,
            controls_style,
        ),
        open_file_button_rect: crate::ui::player_controls::open_file_button_anchor_rect(
            row_rect,
            controls_style,
        ),
        fullscreen_button_rect: crate::ui::player_controls::fullscreen_button_anchor_rect(
            row_rect,
            controls_style,
        ),
        controls_style,
        item_spacing: TEST_ITEM_SPACING,
    }
}

/// Что видит «♪» о CC: у видео есть субтитры (`with_subtitles`) и CC полностью показана,
/// либо субтитров нет и CC не занимает места.
pub(super) fn neighbor_for_width(window_width: f32, with_subtitles: bool) -> SubtitleNeighbor {
    let geometry = geometry_for_window_width(window_width);
    let mode = choose_subtitle_mode(&geometry);
    let shown = with_subtitles && mode != TestSubtitleControlMode::Hidden;
    SubtitleNeighbor {
        geometry,
        mode,
        target_visible: shown,
        reveal_progress: if shown { 1.0 } else { 0.0 },
    }
}
