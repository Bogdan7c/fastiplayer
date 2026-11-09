//! Тесты кнопки качества: чистая раскладка в цепочке соседей и настоящий egui-кадр с кликами.

mod layout;
mod widget;

use egui::{Rect, pos2, vec2};

use super::super::audio_track_control::{
    TestAudioTrackControlMode, audio_quality_neighbor, choose_audio_mode,
};
use super::super::subtitle_control::{
    SubtitleLayoutGeometry, SubtitleNeighbor, TestSubtitleControlMode, choose_subtitle_mode,
};
use super::QualityNeighbor;
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

/// Что CC и «♪» (полностью показанные, если они есть) оставляют кнопке качества.
pub(super) struct Chain {
    pub(super) subtitle: SubtitleNeighbor,
    pub(super) audio_mode: TestAudioTrackControlMode,
    pub(super) quality: QualityNeighbor,
}

pub(super) fn chain(window_width: f32, with_subtitles: bool, with_audio: bool) -> Chain {
    let geometry = geometry_for_window_width(window_width);
    let subtitle_mode = choose_subtitle_mode(&geometry);
    let subtitle_shown = with_subtitles && subtitle_mode != TestSubtitleControlMode::Hidden;
    let subtitle = SubtitleNeighbor {
        geometry,
        mode: subtitle_mode,
        target_visible: subtitle_shown,
        reveal_progress: if subtitle_shown { 1.0 } else { 0.0 },
    };
    let audio_mode = choose_audio_mode(&subtitle);
    let audio_shown = with_audio && audio_mode != TestAudioTrackControlMode::Hidden;
    let audio_progress = if audio_shown { 1.0 } else { 0.0 };
    let quality = audio_quality_neighbor(&subtitle, audio_mode, audio_progress, audio_shown);
    Chain {
        subtitle,
        audio_mode,
        quality,
    }
}
