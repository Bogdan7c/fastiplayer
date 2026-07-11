//! Egui adapter и painting timeline; gesture policy остаётся в соседнем pure module.

use egui::{Color32, Pos2, Rect, Response, Sense, StrokeKind, Ui, Vec2};
use media_core::TimelineSnapshot;

use crate::ui::skin::{PlayerSkin, TimelineStyle};

use super::geometry::{
    TimelineBounds, format_media_duration, format_media_time, rect_from_fraction,
    thumb_outline_radius, timeline_bounds, timeline_track_outline_radius,
    timeline_track_outline_rect, timeline_track_rect,
};
use super::gesture::{
    TimelineInteraction, TimelinePointerInput, TimelineUiState, map_timeline_interaction,
};

/// Рисует timeline и возвращает user actions без отправки player commands.
#[must_use]
pub fn render_timeline(
    ui: &mut Ui,
    timeline: &TimelineSnapshot,
    state: &mut TimelineUiState,
    skin: &impl PlayerSkin,
    live_scrub_enabled: bool,
) -> TimelineInteraction {
    let style = skin.timeline_style();
    let bounds = timeline_bounds(timeline);
    let enabled = bounds.is_some();
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let desired_size = Vec2::new(ui.available_width(), style.hit_height);
    let (response, painter) = ui.allocate_painter(desired_size, sense);
    let pointer_input = pointer_input_from_response(&response, bounds, style);
    let interaction =
        map_timeline_interaction(timeline, state, bounds, pointer_input, live_scrub_enabled);

    // Сохраняем прежнюю focus semantics custom widget-а: mapper получает именно
    // `Response::lost_focus`, без отдельного synthetic focus/playback state.
    if pointer_input.drag_started
        || pointer_input.dragged
        || state.has_active_drag()
        || state.has_active_live_scrub_gesture()
    {
        ui.ctx().request_repaint();
    }
    paint_timeline(
        &painter,
        response.rect,
        timeline,
        state,
        bounds,
        style,
        enabled,
    );
    interaction
}

/// Рисует прежнюю строку position/status/duration вокруг timeline.
pub fn render_time_labels(
    ui: &mut Ui,
    timeline: &TimelineSnapshot,
    state: &TimelineUiState,
    inline_status: Option<&str>,
) {
    let position_text = format_media_time(Some(state.display_position(timeline)));
    let duration_text = format_media_duration(timeline.duration);
    ui.horizontal(|ui| {
        ui.monospace(position_text);
        if let Some(inline_status) = inline_status {
            ui.add_space(8.0);
            ui.colored_label(Color32::from_rgb(255, 180, 120), inline_status);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.monospace(duration_text);
        });
    });
}

fn pointer_input_from_response(
    response: &Response,
    bounds: Option<TimelineBounds>,
    style: TimelineStyle,
) -> TimelinePointerInput {
    let pointer_fraction = bounds
        .zip(response.interact_pointer_pos())
        .map(|(_, pointer_position)| pointer_fraction(response.rect, style, pointer_position));
    TimelinePointerInput {
        clicked: response.clicked(),
        pointer_down_on_timeline: response.is_pointer_button_down_on(),
        drag_started: response.drag_started(),
        dragged: response.dragged(),
        drag_stopped: response.drag_stopped(),
        lost_focus: response.lost_focus(),
        pointer_fraction,
    }
}

fn pointer_fraction(rect: Rect, style: TimelineStyle, pointer_position: Pos2) -> f64 {
    let track_rect = timeline_track_rect(rect, style);
    let width = track_rect.width().max(1.0);
    f64::from(((pointer_position.x - track_rect.left()) / width).clamp(0.0, 1.0))
}

fn paint_timeline(
    painter: &egui::Painter,
    rect: Rect,
    timeline: &TimelineSnapshot,
    state: &TimelineUiState,
    bounds: Option<TimelineBounds>,
    style: TimelineStyle,
    enabled: bool,
) {
    let track_rect = timeline_track_rect(rect, style);
    let track_radius = style.track_height / 2.0;
    painter.rect_filled(
        timeline_track_outline_rect(track_rect, style),
        timeline_track_outline_radius(style),
        style.track_outline_fill,
    );
    painter.rect_filled(
        track_rect,
        track_radius,
        if enabled {
            style.track_fill
        } else {
            style.disabled_fill
        },
    );
    let Some(bounds) = bounds else {
        painter.rect_stroke(
            track_rect,
            track_radius,
            (1.0, Color32::from_gray(72)),
            StrokeKind::Inside,
        );
        return;
    };

    let current_fraction = bounds.fraction_from_position(timeline.current_position);
    let display_position = state.display_position(timeline);
    let display_fraction = bounds.fraction_from_position(display_position);
    painter.rect_filled(
        rect_from_fraction(track_rect, current_fraction),
        track_radius,
        style.played_fill,
    );
    if state.has_active_drag() || timeline.scrubbing {
        painter.rect_filled(
            rect_from_fraction(track_rect, display_fraction),
            track_radius,
            style.target_fill,
        );
    }
    let thumb_center = Pos2::new(
        egui::lerp(track_rect.left()..=track_rect.right(), display_fraction),
        track_rect.center().y,
    );
    painter.circle_filled(
        thumb_center,
        thumb_outline_radius(style),
        style.thumb_outline_fill,
    );
    painter.circle_filled(thumb_center, style.thumb_radius, style.thumb_fill);
}

#[cfg(test)]
mod tests {
    use egui::{Color32, Pos2, Rect, Vec2};

    use super::*;

    fn style() -> TimelineStyle {
        TimelineStyle {
            hit_height: 28.0,
            track_height: 5.0,
            thumb_radius: 6.0,
            horizontal_padding: 8.0,
            track_fill: Color32::from_gray(64),
            played_fill: Color32::WHITE,
            target_fill: Color32::from_rgb(130, 190, 255),
            thumb_fill: Color32::WHITE,
            track_outline_width: 3.0,
            track_outline_fill: Color32::BLACK,
            thumb_outline_width: 2.0,
            thumb_outline_fill: Color32::BLACK,
            disabled_fill: Color32::from_gray(48),
        }
    }

    #[test]
    fn pointer_mapping_uses_track_not_visual_outline() {
        let style = style();
        let hit_rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(220.0, 28.0));
        let track_rect = timeline_track_rect(hit_rect, style);
        assert_eq!(
            pointer_fraction(hit_rect, style, track_rect.left_center()),
            0.0
        );
        assert_eq!(
            pointer_fraction(hit_rect, style, track_rect.right_center()),
            1.0
        );
        assert!(timeline_track_outline_rect(track_rect, style).width() > track_rect.width());
    }

    #[test]
    fn thumb_outline_remains_larger_than_thumb() {
        let style = style();
        assert_eq!(
            thumb_outline_radius(style),
            style.thumb_radius + style.thumb_outline_width
        );
    }
}
