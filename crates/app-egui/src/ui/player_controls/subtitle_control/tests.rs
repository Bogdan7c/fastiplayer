//! Тесты кнопки субтитров: чистая раскладка и настоящий egui-кадр с кликами.

mod layout;
mod widget;

use egui::{Rect, pos2, vec2};
use subtitle_core::{SubtitleFormat, SubtitleLanguage, SubtitleSyncOffset, SubtitleTrackId};

use super::layout::SubtitleLayoutGeometry;
use crate::state::{
    SubtitleFileOrigin, SubtitleMenuTrack, SubtitleMenuView, SubtitleTrackAvailability,
    SubtitleTrackLabel, SubtitleVisibility,
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

/// Дорожка меню: язык (`None` — без языка), выбрана ли, доступна ли.
pub(super) fn menu_track(
    raw_id: u64,
    language: Option<&str>,
    is_selected: bool,
    availability: SubtitleTrackAvailability,
) -> SubtitleMenuTrack {
    SubtitleMenuTrack {
        id: SubtitleTrackId::new(raw_id),
        label: SubtitleTrackLabel {
            language: language.map(|tag| SubtitleLanguage::parse(tag).expect("valid test tag")),
            format: SubtitleFormat::Srt,
            origin: SubtitleFileOrigin::NextToVideo,
            is_forced: false,
            is_hearing_impaired: false,
            title: None,
            file_name: None,
        },
        is_selected,
        availability,
    }
}

/// Снимок меню с заданными дорожками.
pub(super) fn menu_view(
    visibility: SubtitleVisibility,
    tracks: Vec<SubtitleMenuTrack>,
    sync_offset: SubtitleSyncOffset,
) -> SubtitleMenuView {
    SubtitleMenuView {
        has_tracks: !tracks.is_empty(),
        visibility,
        tracks,
        sync_offset,
    }
}
