use egui::{Color32, Painter, Pos2, Rect, StrokeKind};
/// Полностью нейтральный стиль timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineStyle {
    pub track_height: f32,
    pub thumb_radius: f32,
    pub horizontal_padding: f32,
    pub track_fill: Color32,
    pub played_fill: Color32,
    pub target_fill: Color32,
    pub thumb_fill: Color32,
    pub track_outline_width: f32,
    pub track_outline_fill: Color32,
    pub thumb_outline_width: f32,
    pub thumb_outline_fill: Color32,
    pub disabled_fill: Color32,
}
/// Уже вычисленное визуальное состояние timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TimelinePaintState {
    Disabled,
    Enabled {
        current_fraction: f32,
        display_fraction: f32,
        target: bool,
    },
}
/// Вычисляет видимый track rect; этот же helper должен использовать hit-testing.
#[must_use]
pub fn timeline_track_rect(rect: Rect, style: TimelineStyle) -> Rect {
    let pad = style.horizontal_padding.min(rect.width() / 2.0);
    let left = rect.left() + pad;
    let right = rect.right() - pad;
    let half = style.track_height / 2.0;
    Rect::from_min_max(
        Pos2::new(left, rect.center().y - half),
        Pos2::new(right.max(left), rect.center().y + half),
    )
}
pub(crate) fn paint(p: &Painter, r: Rect, state: TimelinePaintState, style: TimelineStyle) {
    let track = timeline_track_rect(r, style);
    let radius = style.track_height / 2.0;
    p.rect_filled(
        track.expand(style.track_outline_width.max(0.0)),
        radius + style.track_outline_width.max(0.0),
        style.track_outline_fill,
    );
    p.rect_filled(
        track,
        radius,
        if matches!(state, TimelinePaintState::Disabled) {
            style.disabled_fill
        } else {
            style.track_fill
        },
    );
    let TimelinePaintState::Enabled {
        current_fraction,
        display_fraction,
        target,
    } = state
    else {
        p.rect_stroke(
            track,
            radius,
            (1.0, Color32::from_gray(72)),
            StrokeKind::Inside,
        );
        return;
    };
    let fraction_rect = |f: f32| {
        Rect::from_min_max(
            track.left_top(),
            Pos2::new(
                egui::lerp(track.left()..=track.right(), f.clamp(0.0, 1.0)),
                track.bottom(),
            ),
        )
    };
    p.rect_filled(fraction_rect(current_fraction), radius, style.played_fill);
    if target {
        p.rect_filled(fraction_rect(display_fraction), radius, style.target_fill);
    }
    let center = Pos2::new(
        egui::lerp(
            track.left()..=track.right(),
            display_fraction.clamp(0.0, 1.0),
        ),
        track.center().y,
    );
    p.circle_filled(
        center,
        style.thumb_radius + style.thumb_outline_width.max(0.0),
        style.thumb_outline_fill,
    );
    p.circle_filled(center, style.thumb_radius, style.thumb_fill);
}
