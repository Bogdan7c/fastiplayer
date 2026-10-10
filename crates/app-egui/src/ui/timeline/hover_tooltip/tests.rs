//! Подсказка времени на таймлайне: правильное время и честное отсутствие.

use egui::{Event, Pos2, Rect};
use media_core::{MediaDuration, MediaTime, TimelineMode, TimelineRange, TimelineSnapshot};

use super::super::TimelineUiState;
use super::super::render::render_timeline;
use super::*;
use crate::ui::skin::PlayerSkin;
use crate::ui::skin::minimal::MinimalSkin;
use crate::ui::test_frame::{input_at, run_ui_frame};

fn vod(seconds: u64) -> TimelineSnapshot {
    TimelineSnapshot::seekable_vod(MediaDuration::from_secs(seconds))
}

fn bounds_of(timeline: &TimelineSnapshot) -> Option<TimelineBounds> {
    super::super::geometry::timeline_bounds(timeline)
}

#[test]
fn text_matches_the_media_position_under_the_pointer() {
    let timeline = vod(100);

    assert_eq!(
        hover_time_text(&timeline, bounds_of(&timeline), 0.0).as_deref(),
        Some("00:00")
    );
    assert_eq!(
        hover_time_text(&timeline, bounds_of(&timeline), 0.35).as_deref(),
        Some("00:35")
    );
    assert_eq!(
        hover_time_text(&timeline, bounds_of(&timeline), 1.0).as_deref(),
        Some("01:40")
    );
}

#[test]
fn long_media_uses_hours_like_the_timer() {
    let timeline = vod(7_200);

    assert_eq!(
        hover_time_text(&timeline, bounds_of(&timeline), 0.5).as_deref(),
        Some("01:00:00")
    );
}

#[test]
fn live_and_unseekable_timelines_show_no_tooltip() {
    let mut live = vod(100);
    live.mode = TimelineMode::Live;
    live.seekable_range = TimelineRange::new(MediaTime::from_secs(30), MediaTime::from_secs(70));
    assert_eq!(hover_time_text(&live, bounds_of(&live), 0.5), None);

    let unknown_duration = TimelineSnapshot::default();
    assert_eq!(
        hover_time_text(&unknown_duration, bounds_of(&unknown_duration), 0.5),
        None
    );
}

/// Тексты, нарисованные за кадр (подсказка живёт на отдельном слое egui).
fn painted_texts(output: &egui::FullOutput) -> Vec<String> {
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
        .collect()
}

/// Один кадр с настоящим `render_timeline`: вывод egui и область таймлайна.
fn timeline_frame(
    egui_ctx: &egui::Context,
    timeline: &TimelineSnapshot,
    state: &mut TimelineUiState,
    events: Vec<Event>,
) -> (egui::FullOutput, Rect) {
    let mut timeline_rect = Rect::NOTHING;
    let output = run_ui_frame(egui_ctx, input_at(0.0, events), |ui| {
        let top_left = ui.cursor().min;
        let _ = render_timeline(ui, timeline, state, &MinimalSkin, false);
        timeline_rect = Rect::from_min_max(top_left, ui.min_rect().max);
    });
    (output, timeline_rect)
}

/// Тексты кадра, когда курсор стоит в доле `fraction` дорожки таймлайна.
fn texts_with_pointer_over_timeline(timeline: &TimelineSnapshot, fraction: f32) -> Vec<String> {
    let egui_ctx = crate::ui::test_frame::app_behavior_context();
    let mut state = TimelineUiState::default();
    let (_, timeline_rect) = timeline_frame(&egui_ctx, timeline, &mut state, Vec::new());
    let track =
        super::super::geometry::timeline_track_rect(timeline_rect, MinimalSkin.timeline_style());
    let pointer = Pos2::new(
        track.left() + track.width() * fraction,
        timeline_rect.center().y,
    );
    let hover = || vec![Event::PointerMoved(pointer)];
    let _settle = timeline_frame(&egui_ctx, timeline, &mut state, hover());
    let (output, _) = timeline_frame(&egui_ctx, timeline, &mut state, hover());
    painted_texts(&output)
}

#[test]
fn hovering_the_timeline_paints_the_time_under_the_cursor() {
    let timeline = vod(100);

    let texts = texts_with_pointer_over_timeline(&timeline, 0.5);

    assert!(
        texts.iter().any(|text| text == "00:50"),
        "ожидалась подсказка 00:50, нарисовано {texts:?}"
    );
}

#[test]
fn hovering_a_live_timeline_paints_no_time_tooltip() {
    let mut live = vod(100);
    live.mode = TimelineMode::Live;

    let texts = texts_with_pointer_over_timeline(&live, 0.5);

    assert!(texts.is_empty(), "у live нет подсказки времени: {texts:?}");
}
