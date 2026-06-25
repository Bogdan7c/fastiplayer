//! Player controls поверх command/snapshot boundary.

use egui::{
    Button, Rect, RichText, Sense, Shape, Stroke, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType,
    pos2, vec2,
};
use player_core::{PlaybackState, PlayerSnapshot};

use crate::ui::assets::IconId;
use crate::ui::skin::{ControlsStyle, PlayerSkin, SkinId};
use crate::ui::timeline::{self, TimelineAction, TimelineUiState};

/// Действие controls, которое shell должен применить после egui pass.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlAction {
    /// Переключить play/pause.
    TogglePlayback,

    /// Открыть файл.
    OpenFile,

    /// Установить новую громкость.
    SetVolume(f32),

    /// Переключить fullscreen.
    ToggleFullscreen,

    /// Действие timeline.
    Timeline(TimelineAction),
}

/// Рисует нижнюю player controls панель и возвращает действия пользователя.
#[must_use]
pub fn render_bottom_controls(
    ui: &mut Ui,
    player_snapshot: &PlayerSnapshot,
    timeline_state: &mut TimelineUiState,
    skin: &impl PlayerSkin,
    is_window_fullscreen: bool,
) -> Vec<ControlAction> {
    let mut actions = Vec::new();
    let panel_id = bottom_panel_id(skin.id());

    egui::Panel::bottom(panel_id)
        .frame(skin.bottom_panel_frame())
        .show_inside(ui, |ui| {
            timeline::render_time_labels(ui, &player_snapshot.timeline, timeline_state);
            let timeline_interaction =
                timeline::render_timeline(ui, &player_snapshot.timeline, timeline_state, skin);
            actions.extend(
                timeline_interaction
                    .actions
                    .into_iter()
                    .map(ControlAction::Timeline),
            );

            ui.add_space(4.0);
            render_button_row(
                ui,
                player_snapshot,
                skin,
                is_window_fullscreen,
                &mut actions,
            );
        });

    actions
}

/// Рисует строку кнопок и volume slider.
fn render_button_row(
    ui: &mut Ui,
    player_snapshot: &PlayerSnapshot,
    skin: &impl PlayerSkin,
    is_window_fullscreen: bool,
    actions: &mut Vec<ControlAction>,
) {
    let controls_style = skin.controls_style();
    let play_icon = playback_toggle_icon(player_snapshot.playback_state);
    let mut volume_value = player_snapshot.volume;

    let row_width = ui.available_width();
    let row_height = controls_style.playback_button_diameter;
    let (row_rect, _) = ui.allocate_exact_size(vec2(row_width, row_height), Sense::hover());
    let playback_button_rect = playback_button_anchor_rect(row_rect, controls_style);
    let fullscreen_button_rect = fullscreen_button_anchor_rect(row_rect, controls_style);
    let zone_gap = ui.spacing().item_spacing.x;
    let left_zone = Rect::from_min_max(
        row_rect.left_top(),
        pos2(
            (playback_button_rect.left() - zone_gap).max(row_rect.left()),
            row_rect.bottom(),
        ),
    );

    ui.scope_builder(
        UiBuilder::new()
            .max_rect(left_zone)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            if icon_button(ui, IconId::OpenFile, skin).clicked() {
                actions.push(ControlAction::OpenFile);
            }

            ui.separator();
            ui.colored_label(controls_style.text_color, skin.icon_text(IconId::Volume));
            let volume_response = ui.add_sized(
                [
                    controls_style.volume_slider_width,
                    controls_style.button_height,
                ],
                egui::Slider::new(&mut volume_value, 0.0..=1.0).show_value(false),
            );
            if volume_response.changed() {
                actions.push(ControlAction::SetVolume(volume_value));
            }
            ui.monospace(format!("{:.0}%", volume_value * 100.0));
        },
    );

    if render_playback_toggle_button_at(ui, playback_button_rect, play_icon, skin).clicked() {
        actions.push(ControlAction::TogglePlayback);
    }

    if render_fullscreen_toggle_button_at(ui, fullscreen_button_rect, is_window_fullscreen, skin)
        .clicked()
    {
        actions.push(ControlAction::ToggleFullscreen);
    }
}

/// Считает rect центральной кнопки от полного rect строки, а не от соседних controls.
fn playback_button_anchor_rect(row_rect: Rect, controls_style: ControlsStyle) -> Rect {
    let button_size = Vec2::splat(controls_style.playback_button_diameter);
    let button_center = pos2(
        row_rect.center().x,
        row_rect.center().y - controls_style.playback_button_vertical_raise,
    );

    Rect::from_center_size(button_center, button_size)
}

/// Считает rect fullscreen-кнопки от всей content-строки: правый край совпадает с row,
/// а вертикальный центр синхронизирован с play/pause-кнопкой.
fn fullscreen_button_anchor_rect(row_rect: Rect, controls_style: ControlsStyle) -> Rect {
    let button_size = Vec2::splat(controls_style.fullscreen_button_size);
    let button_center = pos2(
        row_rect.right() - button_size.x * 0.5,
        row_rect.center().y - controls_style.playback_button_vertical_raise,
    );

    Rect::from_center_size(button_center, button_size)
}

/// Рисует центральную play/pause-кнопку в уже рассчитанном anchored rect.
fn render_playback_toggle_button_at(
    ui: &mut Ui,
    button_rect: Rect,
    icon_id: IconId,
    skin: &impl PlayerSkin,
) -> egui::Response {
    let button_response = ui.allocate_rect(button_rect, Sense::click());
    let accessible_label = skin.icon_text(icon_id);
    let button_response = button_response.on_hover_text(accessible_label);

    button_response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), accessible_label));

    paint_playback_button(ui, button_rect, icon_id, skin, &button_response);

    button_response
}

/// Рисует fullscreen-кнопку в заранее рассчитанном anchored rect.
fn render_fullscreen_toggle_button_at(
    ui: &mut Ui,
    button_rect: Rect,
    is_window_fullscreen: bool,
    skin: &impl PlayerSkin,
) -> egui::Response {
    let presentation = fullscreen_toggle_presentation(is_window_fullscreen);
    let button_response = ui.allocate_rect(button_rect, Sense::click());
    let button_response = button_response.on_hover_text(presentation.accessible_label);

    button_response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Button,
            ui.is_enabled(),
            presentation.accessible_label,
        )
    });

    paint_fullscreen_toggle_button(ui, button_rect, presentation.icon, skin, &button_response);

    button_response
}

/// Выбирает иконку toggle через player-side active semantics, а не через один `Playing`.
fn playback_toggle_icon(playback_state: PlaybackState) -> IconId {
    if playback_state.is_playback_active() {
        // Active состояния, включая EOF drain, пользователь воспринимает как pauseable playback.
        IconId::Pause
    } else {
        IconId::Play
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FullscreenToggleIcon {
    EnterFullscreen,
    ExitFullscreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FullscreenTogglePresentation {
    icon: FullscreenToggleIcon,
    accessible_label: &'static str,
}

/// Описывает вид кнопки по состоянию окна, чтобы render-код не дублировал тексты.
fn fullscreen_toggle_presentation(is_window_fullscreen: bool) -> FullscreenTogglePresentation {
    if is_window_fullscreen {
        return FullscreenTogglePresentation {
            icon: FullscreenToggleIcon::ExitFullscreen,
            accessible_label: "Выйти из полноэкранного режима",
        };
    }

    FullscreenTogglePresentation {
        icon: FullscreenToggleIcon::EnterFullscreen,
        accessible_label: "Полноэкранный режим",
    }
}

/// Рисует кнопку для asset-backed иконки.
fn icon_button(ui: &mut Ui, icon_id: IconId, skin: &impl PlayerSkin) -> egui::Response {
    let asset_id = skin.icon_asset(icon_id);

    ui.push_id(asset_id, |ui| {
        fixed_button(ui, skin.icon_text(icon_id), skin)
    })
    .inner
}

/// Рисует квадратную fullscreen-кнопку в стиле settings-кнопки: фон только на hover,
/// hand-drawn glyph строится stroke-линиями без зависимости от asset pack.
fn paint_fullscreen_toggle_button(
    ui: &Ui,
    button_rect: Rect,
    icon: FullscreenToggleIcon,
    skin: &impl PlayerSkin,
    button_response: &egui::Response,
) {
    let controls_style = skin.controls_style();
    let painter = ui.painter();
    let icon_stroke = Stroke::new(
        controls_style.playback_button_stroke_width,
        controls_style.text_color,
    );

    if button_response.hovered() {
        painter.rect_filled(button_rect, 0.0, controls_style.playback_button_hover_fill);
    }

    paint_fullscreen_corners_icon(
        painter,
        button_rect,
        controls_style.fullscreen_icon_extent,
        icon,
        icon_stroke,
    );
}

/// Рисует четыре corner-segment glyph: outward corners для входа в fullscreen,
/// inward corners для выхода из него.
fn paint_fullscreen_corners_icon(
    painter: &egui::Painter,
    button_rect: Rect,
    icon_extent: f32,
    icon: FullscreenToggleIcon,
    stroke: Stroke,
) {
    let icon_rect = Rect::from_center_size(button_rect.center(), Vec2::splat(icon_extent));
    let corner_leg = icon_extent * 0.38;

    match icon {
        FullscreenToggleIcon::EnterFullscreen => {
            paint_enter_fullscreen_corners(painter, icon_rect, corner_leg, stroke);
        }
        FullscreenToggleIcon::ExitFullscreen => {
            paint_exit_fullscreen_corners(painter, icon_rect, corner_leg, stroke);
        }
    }
}

/// Рисует outward-corners: углы стоят на внешней рамке glyph и раскрываются наружу.
fn paint_enter_fullscreen_corners(
    painter: &egui::Painter,
    icon_rect: Rect,
    corner_leg: f32,
    stroke: Stroke,
) {
    let left = icon_rect.left();
    let right = icon_rect.right();
    let top = icon_rect.top();
    let bottom = icon_rect.bottom();

    for (corner, horizontal_end, vertical_end) in [
        (
            pos2(left, top),
            pos2(left + corner_leg, top),
            pos2(left, top + corner_leg),
        ),
        (
            pos2(right, top),
            pos2(right - corner_leg, top),
            pos2(right, top + corner_leg),
        ),
        (
            pos2(left, bottom),
            pos2(left + corner_leg, bottom),
            pos2(left, bottom - corner_leg),
        ),
        (
            pos2(right, bottom),
            pos2(right - corner_leg, bottom),
            pos2(right, bottom - corner_leg),
        ),
    ] {
        painter.line_segment([corner, horizontal_end], stroke);
        painter.line_segment([corner, vertical_end], stroke);
    }
}

/// Рисует inward-corners: вершины углов смещены внутрь glyph и визуально показывают выход.
fn paint_exit_fullscreen_corners(
    painter: &egui::Painter,
    icon_rect: Rect,
    corner_leg: f32,
    stroke: Stroke,
) {
    let left = icon_rect.left();
    let right = icon_rect.right();
    let top = icon_rect.top();
    let bottom = icon_rect.bottom();

    for (corner, horizontal_end, vertical_end) in [
        (
            pos2(left + corner_leg, top + corner_leg),
            pos2(left, top + corner_leg),
            pos2(left + corner_leg, top),
        ),
        (
            pos2(right - corner_leg, top + corner_leg),
            pos2(right, top + corner_leg),
            pos2(right - corner_leg, top),
        ),
        (
            pos2(left + corner_leg, bottom - corner_leg),
            pos2(left, bottom - corner_leg),
            pos2(left + corner_leg, bottom),
        ),
        (
            pos2(right - corner_leg, bottom - corner_leg),
            pos2(right, bottom - corner_leg),
            pos2(right - corner_leg, bottom),
        ),
    ] {
        painter.line_segment([corner, horizontal_end], stroke);
        painter.line_segment([corner, vertical_end], stroke);
    }
}

/// Рисует круг, hover-заливку и glyph центральной play/pause-кнопки.
fn paint_playback_button(
    ui: &Ui,
    button_rect: Rect,
    icon_id: IconId,
    skin: &impl PlayerSkin,
    button_response: &egui::Response,
) {
    let controls_style = skin.controls_style();
    let button_center = button_rect.center();
    let button_radius = (controls_style.playback_button_diameter * 0.5)
        - (controls_style.playback_button_stroke_width * 0.5);
    let button_stroke = Stroke::new(
        controls_style.playback_button_stroke_width,
        controls_style.text_color,
    );
    let painter = ui.painter();

    if button_response.hovered() {
        painter.circle_filled(
            button_center,
            button_radius,
            controls_style.playback_button_hover_fill,
        );
    }
    painter.circle_stroke(button_center, button_radius, button_stroke);

    match icon_id {
        IconId::Play => paint_play_glyph(
            painter,
            button_rect,
            controls_style.playback_button_icon_extent,
            button_stroke,
        ),
        IconId::Pause => paint_pause_glyph(
            painter,
            button_rect,
            controls_style.playback_button_icon_extent,
            button_stroke,
        ),
        IconId::OpenFile | IconId::Volume => {}
    }
}

/// Рисует play glyph как треугольник, чтобы он был независим от текстовой fallback-иконки.
fn paint_play_glyph(
    painter: &egui::Painter,
    button_rect: Rect,
    icon_extent: f32,
    button_stroke: Stroke,
) {
    let center = button_rect.center();
    let half_extent = icon_extent * 0.5;
    let points = vec![
        pos2(center.x - half_extent * 0.45, center.y - half_extent),
        pos2(center.x - half_extent * 0.45, center.y + half_extent),
        pos2(center.x + half_extent * 0.75, center.y),
    ];

    painter.add(Shape::convex_polygon(
        points,
        button_stroke.color,
        Stroke::NONE,
    ));
}

/// Рисует pause glyph двумя вертикальными линиями.
fn paint_pause_glyph(
    painter: &egui::Painter,
    button_rect: Rect,
    icon_extent: f32,
    button_stroke: Stroke,
) {
    let center = button_rect.center();
    let half_height = icon_extent * 0.5;
    let half_gap = icon_extent * 0.16;
    let line_offset = half_gap + button_stroke.width;

    for x in [center.x - line_offset, center.x + line_offset] {
        painter.line_segment(
            [
                pos2(x, center.y - half_height),
                pos2(x, center.y + half_height),
            ],
            button_stroke,
        );
    }
}

/// Рисует кнопку фиксированного размера, чтобы layout не прыгал от текста.
fn fixed_button(ui: &mut Ui, text: impl Into<RichText>, skin: &impl PlayerSkin) -> egui::Response {
    let controls_style = skin.controls_style();
    let button_text = text.into().color(controls_style.text_color);

    ui.add_sized(
        [controls_style.button_width, controls_style.button_height],
        Button::new(button_text),
    )
}

/// Возвращает panel id нижней панели для выбранного skin-а.
fn bottom_panel_id(skin_id: SkinId) -> &'static str {
    match skin_id {
        SkinId::Minimal => "controls_minimal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::skin::minimal::MinimalSkin;

    /// Проверяет regression: audible EOF tail в `Draining` должен выглядеть как pause.
    #[test]
    fn player_controls_show_pause_icon_for_draining_toggle() {
        assert_eq!(playback_toggle_icon(PlaybackState::Draining), IconId::Pause);
    }

    /// Проверяет replay affordance: полностью завершённый media должен выглядеть как play.
    #[test]
    fn player_controls_show_play_icon_for_ended_toggle() {
        assert_eq!(playback_toggle_icon(PlaybackState::Ended), IconId::Play);
    }

    /// Проверяет, что skin владеет геометрией центральной и fullscreen-кнопки.
    #[test]
    fn minimal_skin_owns_playback_and_fullscreen_button_geometry() {
        let controls_style = MinimalSkin.controls_style();

        assert!(controls_style.playback_button_diameter > controls_style.button_height);
        assert!(controls_style.playback_button_stroke_width > 0.0);
        assert_eq!(controls_style.playback_button_vertical_raise, 5.0);
        assert!(controls_style.playback_button_icon_extent > 0.0);
        assert!(
            controls_style.playback_button_icon_extent < controls_style.playback_button_diameter
        );
        assert_eq!(controls_style.fullscreen_button_size, 32.0);
        assert_eq!(controls_style.fullscreen_icon_extent, 16.0);
        assert!(controls_style.fullscreen_icon_extent < controls_style.fullscreen_button_size);
    }

    /// Проверяет anchor: кнопка стоит по центру строки и поднята на 5 points.
    #[test]
    fn playback_button_anchor_rect_centers_button_and_raises_it() {
        let controls_style = MinimalSkin.controls_style();
        let row_rect = Rect::from_min_size(
            pos2(24.0, 80.0),
            vec2(640.0, controls_style.playback_button_diameter),
        );
        let button_rect = playback_button_anchor_rect(row_rect, controls_style);

        assert_eq!(
            button_rect.size(),
            Vec2::splat(controls_style.playback_button_diameter)
        );
        assert!((button_rect.center().x - row_rect.center().x).abs() < f32::EPSILON);
        assert!(
            (button_rect.center().y
                - (row_rect.center().y - controls_style.playback_button_vertical_raise))
                .abs()
                < f32::EPSILON
        );
    }

    /// Проверяет, что fullscreen-кнопка имеет skin-owned размер 32x32.
    #[test]
    fn fullscreen_button_anchor_rect_has_skin_owned_square_size() {
        let controls_style = MinimalSkin.controls_style();
        let row_rect = Rect::from_min_size(
            pos2(24.0, 80.0),
            vec2(640.0, controls_style.playback_button_diameter),
        );
        let button_rect = fullscreen_button_anchor_rect(row_rect, controls_style);

        assert_eq!(
            button_rect.size(),
            Vec2::splat(controls_style.fullscreen_button_size)
        );
    }

    /// Проверяет, что fullscreen-кнопка привязана к правому краю content-строки.
    #[test]
    fn fullscreen_button_anchor_rect_right_edge_matches_row_right_edge() {
        let controls_style = MinimalSkin.controls_style();
        let row_rect = Rect::from_min_size(
            pos2(24.0, 80.0),
            vec2(640.0, controls_style.playback_button_diameter),
        );
        let button_rect = fullscreen_button_anchor_rect(row_rect, controls_style);

        assert!((button_rect.right() - row_rect.right()).abs() < f32::EPSILON);
    }

    /// Проверяет, что fullscreen-кнопка стоит на той же вертикальной оси, что play/pause.
    #[test]
    fn fullscreen_button_anchor_rect_center_y_matches_playback_button_center_y() {
        let controls_style = MinimalSkin.controls_style();
        let row_rect = Rect::from_min_size(
            pos2(24.0, 80.0),
            vec2(640.0, controls_style.playback_button_diameter),
        );
        let playback_button_rect = playback_button_anchor_rect(row_rect, controls_style);
        let fullscreen_button_rect = fullscreen_button_anchor_rect(row_rect, controls_style);

        assert!(
            (fullscreen_button_rect.center().y - playback_button_rect.center().y).abs()
                < f32::EPSILON
        );
    }

    /// Проверяет, что состояние окна выбирает правильную иконку и accessible label.
    #[test]
    fn fullscreen_toggle_presentation_matches_window_fullscreen_state() {
        assert_eq!(
            fullscreen_toggle_presentation(false),
            FullscreenTogglePresentation {
                icon: FullscreenToggleIcon::EnterFullscreen,
                accessible_label: "Полноэкранный режим",
            }
        );
        assert_eq!(
            fullscreen_toggle_presentation(true),
            FullscreenTogglePresentation {
                icon: FullscreenToggleIcon::ExitFullscreen,
                accessible_label: "Выйти из полноэкранного режима",
            }
        );
    }
}
