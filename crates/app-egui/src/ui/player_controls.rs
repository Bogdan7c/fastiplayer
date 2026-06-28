//! Player controls поверх command/snapshot boundary.

use egui::{Color32, Rect, Sense, Shape, Stroke, Ui, Vec2, WidgetInfo, WidgetType, pos2, vec2};
use player_core::{PlaybackState, PlayerSnapshot};

use crate::ui::assets::IconId;
use crate::ui::skin::{ControlsStyle, PlayerSkin, SkinId};
use crate::ui::timeline::{self, TimelineAction, TimelineUiState};

const VOLUME_SEPARATOR_WIDTH: f32 = 1.0;
const VOLUME_SEPARATOR_HEIGHT_FACTOR: f32 = 0.68;
const VOLUME_TRACK_HEIGHT: f32 = 3.0;
const VOLUME_THUMB_RADIUS: f32 = 5.0;
const VOLUME_WAVE_SEGMENTS: usize = 8;

/// Действие controls, которое shell должен применить после egui pass.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlAction {
    /// Переключить play/pause.
    TogglePlayback,

    /// Открыть файл.
    OpenFile,

    /// Установить новую громкость.
    SetVolume(f32),

    /// Переключить mute/unmute.
    ToggleMute,

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

    let row_width = ui.available_width();
    let row_height = controls_style.playback_button_diameter;
    let (row_rect, _) = ui.allocate_exact_size(vec2(row_width, row_height), Sense::hover());
    let open_file_button_rect = open_file_button_anchor_rect(row_rect, controls_style);
    let playback_button_rect = playback_button_anchor_rect(row_rect, controls_style);
    let fullscreen_button_rect = fullscreen_button_anchor_rect(row_rect, controls_style);
    let volume_to_playback_gap = ui.spacing().item_spacing.x;
    let volume_zone = volume_controls_zone_rect(
        row_rect,
        open_file_button_rect,
        playback_button_rect,
        volume_to_playback_gap,
    );

    if render_open_file_button_at(ui, open_file_button_rect, skin).clicked() {
        actions.push(ControlAction::OpenFile);
    }

    render_volume_controls(ui, player_snapshot, controls_style, volume_zone, actions);

    if render_playback_toggle_button_at(ui, playback_button_rect, play_icon, skin).clicked() {
        actions.push(ControlAction::TogglePlayback);
    }

    if render_fullscreen_toggle_button_at(ui, fullscreen_button_rect, is_window_fullscreen, skin)
        .clicked()
    {
        actions.push(ControlAction::ToggleFullscreen);
    }
}

/// Считает rect open-file кнопки от всей content-строки.
/// Левый отступ зеркалит fullscreen-кнопку: он равен нижнему отступу кнопки.
fn open_file_button_anchor_rect(row_rect: Rect, controls_style: ControlsStyle) -> Rect {
    let button_size = Vec2::splat(controls_style.fullscreen_button_size);
    let button_center_y = row_rect.center().y - controls_style.playback_button_vertical_raise;
    let bottom_inset = row_rect.bottom() - (button_center_y + button_size.y * 0.5);
    let button_center_x = row_rect.left() + bottom_inset + button_size.x * 0.5;
    let button_center = pos2(button_center_x, button_center_y);

    Rect::from_center_size(button_center, button_size)
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

/// Считает rect fullscreen-кнопки от всей content-строки.
/// Правый отступ намеренно равен нижнему отступу, чтобы кнопка не прилипала к углу панели.
fn fullscreen_button_anchor_rect(row_rect: Rect, controls_style: ControlsStyle) -> Rect {
    let button_size = Vec2::splat(controls_style.fullscreen_button_size);
    let button_center_y = row_rect.center().y - controls_style.playback_button_vertical_raise;
    let bottom_inset = row_rect.bottom() - (button_center_y + button_size.y * 0.5);
    let button_center_x = row_rect.right() - bottom_inset - button_size.x * 0.5;
    let button_center = pos2(button_center_x, button_center_y);

    Rect::from_center_size(button_center, button_size)
}

/// Ограничивает volume controls зоной между open-file и play/pause.
/// Это держит flow-layout volume-а отдельно от anchored кнопок.
fn volume_controls_zone_rect(
    row_rect: Rect,
    open_file_button_rect: Rect,
    playback_button_rect: Rect,
    volume_to_playback_gap: f32,
) -> Rect {
    let open_button_left_inset = open_file_button_rect.left() - row_rect.left();
    let zone_left = (open_file_button_rect.right() + open_button_left_inset)
        .clamp(row_rect.left(), row_rect.right());
    let zone_right =
        (playback_button_rect.left() - volume_to_playback_gap).clamp(zone_left, row_rect.right());

    Rect::from_min_max(
        pos2(zone_left, row_rect.top()),
        pos2(zone_right, row_rect.bottom()),
    )
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct VolumeControlLayout {
    /// Тонкий separator между open-file и live-volume controls.
    separator_rect: Rect,

    /// Квадратная hit-zone speaker-кнопки.
    icon_button_rect: Rect,

    /// Широкая hit-zone slider-а: клик и drag работают не только по тонкой линии.
    slider_hit_rect: Rect,

    /// Узкая видимая линия, по которой мапится pointer -> volume.
    slider_track_rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VolumeIconState {
    Audible,
    Muted,
}

/// Рисует custom volume control внутри уже рассчитанной anchored зоны.
fn render_volume_controls(
    ui: &mut Ui,
    player_snapshot: &PlayerSnapshot,
    controls_style: ControlsStyle,
    volume_zone: Rect,
    actions: &mut Vec<ControlAction>,
) {
    let layout = volume_control_layout(volume_zone, controls_style, ui.spacing().item_spacing.x);
    let icon_state = volume_icon_state(player_snapshot);

    paint_volume_separator(ui, layout.separator_rect, controls_style);

    if render_volume_icon_button_at(ui, layout.icon_button_rect, controls_style, icon_state)
        .clicked()
    {
        actions.push(ControlAction::ToggleMute);
    }

    if let Some(requested_volume) = render_volume_slider_at(
        ui,
        layout.slider_hit_rect,
        layout.slider_track_rect,
        controls_style,
        player_snapshot.volume,
    ) {
        actions.push(ControlAction::SetVolume(requested_volume));
    }
}

/// Считает geometry без egui flow-layout, чтобы volume не сдвигал anchored buttons.
fn volume_control_layout(
    zone_rect: Rect,
    controls_style: ControlsStyle,
    item_spacing_x: f32,
) -> VolumeControlLayout {
    let controls_center_y = zone_rect.center().y - controls_style.playback_button_vertical_raise;
    let separator_width = VOLUME_SEPARATOR_WIDTH.min(zone_rect.width().max(0.0));
    let separator_height = controls_style.button_height * VOLUME_SEPARATOR_HEIGHT_FACTOR;
    let separator_rect = Rect::from_center_size(
        pos2(zone_rect.left() + separator_width * 0.5, controls_center_y),
        vec2(separator_width, separator_height),
    );

    let available_after_separator = (zone_rect.right() - separator_rect.right()).max(0.0);
    let icon_gap = item_spacing_x.max(4.0).min(available_after_separator);
    let icon_size = controls_style
        .button_height
        .min((zone_rect.right() - separator_rect.right() - icon_gap).max(0.0));
    let icon_button_rect = Rect::from_min_size(
        pos2(
            separator_rect.right() + icon_gap,
            controls_center_y - icon_size * 0.5,
        ),
        Vec2::splat(icon_size),
    );

    let available_after_icon = (zone_rect.right() - icon_button_rect.right()).max(0.0);
    let slider_gap = item_spacing_x.max(6.0).min(available_after_icon);
    let slider_left = icon_button_rect.right() + slider_gap;
    let slider_width = controls_style
        .volume_slider_width
        .min((zone_rect.right() - slider_left).max(0.0));
    let slider_hit_rect = Rect::from_min_size(
        pos2(
            slider_left,
            controls_center_y - controls_style.button_height * 0.5,
        ),
        vec2(slider_width, controls_style.button_height),
    );

    let track_horizontal_inset = VOLUME_THUMB_RADIUS.min(slider_hit_rect.width() * 0.5);
    let slider_track_rect = Rect::from_min_max(
        pos2(
            slider_hit_rect.left() + track_horizontal_inset,
            controls_center_y - VOLUME_TRACK_HEIGHT * 0.5,
        ),
        pos2(
            slider_hit_rect.right() - track_horizontal_inset,
            controls_center_y + VOLUME_TRACK_HEIGHT * 0.5,
        ),
    );

    VolumeControlLayout {
        separator_rect,
        icon_button_rect,
        slider_hit_rect,
        slider_track_rect,
    }
}

/// UI считает audible так же, как player snapshot: отдельный mute-флаг важнее числа.
fn volume_icon_state(player_snapshot: &PlayerSnapshot) -> VolumeIconState {
    if !player_snapshot.muted && player_snapshot.volume > f32::EPSILON {
        VolumeIconState::Audible
    } else {
        VolumeIconState::Muted
    }
}

/// Мапит pointer по видимому track-у и жёстко держит публичный диапазон volume.
fn volume_from_pointer_x(track_rect: Rect, pointer_x: f32) -> f32 {
    let track_width = track_rect.width();
    if track_width <= f32::EPSILON {
        return 0.0;
    }

    ((pointer_x - track_rect.left()) / track_width).clamp(0.0, 1.0)
}

/// Возвращает x-позицию thumb-а по тому же track-у, по которому работает pointer mapping.
fn volume_thumb_center_x(track_rect: Rect, volume: f32) -> f32 {
    track_rect.left() + track_rect.width() * volume.clamp(0.0, 1.0)
}

fn paint_volume_separator(ui: &Ui, separator_rect: Rect, controls_style: ControlsStyle) {
    ui.painter().rect_filled(
        separator_rect,
        VOLUME_SEPARATOR_WIDTH * 0.5,
        controls_style.text_color.gamma_multiply(0.45),
    );
}

fn render_volume_icon_button_at(
    ui: &mut Ui,
    button_rect: Rect,
    controls_style: ControlsStyle,
    icon_state: VolumeIconState,
) -> egui::Response {
    let accessible_label = match icon_state {
        VolumeIconState::Audible => "Выключить звук",
        VolumeIconState::Muted => "Включить звук",
    };
    let button_response = ui.allocate_rect(button_rect, Sense::click());
    let button_response = button_response.on_hover_text(accessible_label);

    button_response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), accessible_label));

    paint_volume_icon_button(
        ui,
        button_rect,
        controls_style,
        icon_state,
        &button_response,
    );

    button_response
}

fn render_volume_slider_at(
    ui: &mut Ui,
    slider_hit_rect: Rect,
    slider_track_rect: Rect,
    controls_style: ControlsStyle,
    current_volume: f32,
) -> Option<f32> {
    let slider_response = ui.allocate_rect(slider_hit_rect, Sense::click_and_drag());
    slider_response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Slider, ui.is_enabled(), "Громкость"));

    paint_volume_slider(
        ui,
        slider_track_rect,
        controls_style,
        current_volume.clamp(0.0, 1.0),
        slider_response.hovered() || slider_response.dragged(),
    );

    let pointer_position = slider_response.interact_pointer_pos()?;
    let requested_volume = volume_from_pointer_x(slider_track_rect, pointer_position.x);

    (slider_response.clicked() || slider_response.dragged())
        .then_some(requested_volume)
        .filter(|volume| (*volume - current_volume).abs() > f32::EPSILON)
}

fn paint_volume_icon_button(
    ui: &Ui,
    button_rect: Rect,
    controls_style: ControlsStyle,
    icon_state: VolumeIconState,
    button_response: &egui::Response,
) {
    if button_response.hovered() {
        ui.painter()
            .rect_filled(button_rect, 0.0, controls_style.playback_button_hover_fill);
    }

    let icon_stroke = Stroke::new(
        controls_style.playback_button_stroke_width,
        controls_style.text_color,
    );
    paint_speaker_glyph(ui.painter(), button_rect, icon_state, icon_stroke);
}

fn paint_speaker_glyph(
    painter: &egui::Painter,
    button_rect: Rect,
    icon_state: VolumeIconState,
    stroke: Stroke,
) {
    let icon_side = button_rect.width().min(button_rect.height()) * 0.68;
    let icon_rect = Rect::from_center_size(button_rect.center(), Vec2::splat(icon_side));
    let center = icon_rect.center();
    let speaker_points = vec![
        pos2(icon_rect.left(), center.y - icon_side * 0.14),
        pos2(center.x - icon_side * 0.18, center.y - icon_side * 0.14),
        pos2(center.x + icon_side * 0.06, icon_rect.top()),
        pos2(center.x + icon_side * 0.06, icon_rect.bottom()),
        pos2(center.x - icon_side * 0.18, center.y + icon_side * 0.14),
        pos2(icon_rect.left(), center.y + icon_side * 0.14),
        pos2(icon_rect.left(), center.y - icon_side * 0.14),
    ];

    painter.add(Shape::line(speaker_points, stroke));

    match icon_state {
        VolumeIconState::Audible => {
            let wave_origin = pos2(center.x + icon_side * 0.05, center.y);
            paint_volume_wave(painter, wave_origin, icon_side * 0.25, stroke);
            paint_volume_wave(painter, wave_origin, icon_side * 0.39, stroke);
        }
        VolumeIconState::Muted => {
            painter.line_segment(
                [
                    pos2(icon_rect.right(), icon_rect.top() + icon_side * 0.08),
                    pos2(icon_rect.left() + icon_side * 0.08, icon_rect.bottom()),
                ],
                stroke,
            );
        }
    }
}

fn paint_volume_wave(painter: &egui::Painter, origin: egui::Pos2, radius: f32, stroke: Stroke) {
    let mut points = Vec::with_capacity(VOLUME_WAVE_SEGMENTS + 1);
    for segment_index in 0..=VOLUME_WAVE_SEGMENTS {
        let progress = segment_index as f32 / VOLUME_WAVE_SEGMENTS as f32;
        let angle = -0.72 + progress * 1.44;
        points.push(pos2(
            origin.x + radius * angle.cos(),
            origin.y + radius * angle.sin(),
        ));
    }

    painter.add(Shape::line(points, stroke));
}

fn paint_volume_slider(
    ui: &Ui,
    track_rect: Rect,
    controls_style: ControlsStyle,
    volume: f32,
    is_interactive: bool,
) {
    let painter = ui.painter();
    let track_radius = VOLUME_TRACK_HEIGHT * 0.5;
    let track_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 52);
    let active_fill = controls_style.text_color;
    let thumb_center_x = volume_thumb_center_x(track_rect, volume);
    let active_rect = Rect::from_min_max(
        track_rect.left_top(),
        pos2(thumb_center_x.max(track_rect.left()), track_rect.bottom()),
    );
    let thumb_radius = if is_interactive {
        VOLUME_THUMB_RADIUS + 1.0
    } else {
        VOLUME_THUMB_RADIUS
    };

    painter.rect_filled(track_rect, track_radius, track_fill);
    if active_rect.width() > f32::EPSILON {
        painter.rect_filled(active_rect, track_radius, active_fill.gamma_multiply(0.85));
    }
    painter.circle_filled(
        pos2(thumb_center_x, track_rect.center().y),
        thumb_radius,
        active_fill,
    );
    painter.circle_stroke(
        pos2(thumb_center_x, track_rect.center().y),
        thumb_radius,
        Stroke::new(
            controls_style.playback_button_stroke_width,
            Color32::from_rgba_unmultiplied(0, 0, 0, 130),
        ),
    );
}

/// Рисует open-file кнопку в заранее рассчитанном anchored rect.
fn render_open_file_button_at(
    ui: &mut Ui,
    button_rect: Rect,
    skin: &impl PlayerSkin,
) -> egui::Response {
    let accessible_label = "Открыть файл";
    let button_response = ui.allocate_rect(button_rect, Sense::click());
    let button_response = button_response.on_hover_text(accessible_label);

    button_response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), accessible_label));

    paint_open_file_button(ui, button_rect, skin, &button_response);

    button_response
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
    match playback_state {
        // Active состояния, включая EOF drain, пользователь воспринимает как pauseable playback.
        PlaybackState::Playing
        | PlaybackState::Buffering
        | PlaybackState::Seeking
        | PlaybackState::Draining => IconId::Pause,
        PlaybackState::Idle
        | PlaybackState::Opening
        | PlaybackState::Paused
        | PlaybackState::Scrubbing
        | PlaybackState::Ended
        | PlaybackState::Stopped
        | PlaybackState::Failed => IconId::Play,
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

/// Рисует квадратную open-file кнопку: hover-фон общий с fullscreen,
/// glyph полностью векторный и не зависит от asset pack.
fn paint_open_file_button(
    ui: &Ui,
    button_rect: Rect,
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

    paint_open_file_concept_icon(painter, button_rect, icon_stroke);
}

/// Рисует hand-drawn glyph "media file": контур файла, play-треугольник
/// и три короткие строки справа, чтобы кнопка читалась как открытие media.
fn paint_open_file_concept_icon(painter: &egui::Painter, button_rect: Rect, stroke: Stroke) {
    let icon_side = button_rect.width().min(button_rect.height()) * 0.64;
    let icon_rect = Rect::from_center_size(button_rect.center(), vec2(icon_side * 1.12, icon_side));
    let file_rect = Rect::from_min_size(
        icon_rect.left_top(),
        vec2(icon_rect.width() * 0.58, icon_rect.height()),
    );
    let fold_size = file_rect.width() * 0.24;
    let file_top_right_before_fold = pos2(file_rect.right() - fold_size, file_rect.top());
    let file_fold_corner = pos2(file_rect.right(), file_rect.top() + fold_size);

    painter.line_segment([file_rect.left_top(), file_top_right_before_fold], stroke);
    painter.line_segment([file_top_right_before_fold, file_fold_corner], stroke);
    painter.line_segment([file_fold_corner, file_rect.right_bottom()], stroke);
    painter.line_segment([file_rect.right_bottom(), file_rect.left_bottom()], stroke);
    painter.line_segment([file_rect.left_bottom(), file_rect.left_top()], stroke);

    let play_center = pos2(
        file_rect.center().x - file_rect.width() * 0.03,
        file_rect.center().y + file_rect.height() * 0.04,
    );
    let play_half_height = file_rect.height() * 0.19;
    let play_half_width = file_rect.width() * 0.17;
    let play_points = vec![
        pos2(
            play_center.x - play_half_width,
            play_center.y - play_half_height,
        ),
        pos2(
            play_center.x - play_half_width,
            play_center.y + play_half_height,
        ),
        pos2(play_center.x + play_half_width, play_center.y),
    ];
    painter.add(Shape::convex_polygon(
        play_points,
        stroke.color,
        Stroke::NONE,
    ));

    let detail_dot_x = file_rect.right() + icon_rect.width() * 0.12;
    let detail_line_start_x = detail_dot_x + icon_rect.width() * 0.08;
    let detail_line_end_x = icon_rect.right();
    for detail_line_y in [
        icon_rect.center().y - icon_rect.height() * 0.24,
        icon_rect.center().y,
        icon_rect.center().y + icon_rect.height() * 0.24,
    ] {
        painter.circle_filled(
            pos2(detail_dot_x, detail_line_y),
            stroke.width * 0.55,
            stroke.color,
        );
        painter.line_segment(
            [
                pos2(detail_line_start_x, detail_line_y),
                pos2(detail_line_end_x, detail_line_y),
            ],
            stroke,
        );
    }
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

    /// Scrubbing frozen state не считается active playback для toggle affordance.
    #[test]
    fn player_controls_show_play_icon_for_scrubbing_toggle() {
        assert_eq!(playback_toggle_icon(PlaybackState::Scrubbing), IconId::Play);
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

    /// Проверяет, что open-file кнопка имеет тот же skin-owned размер, что fullscreen.
    #[test]
    fn open_file_button_anchor_rect_has_fullscreen_square_size() {
        let controls_style = MinimalSkin.controls_style();
        let row_rect = Rect::from_min_size(
            pos2(24.0, 80.0),
            vec2(640.0, controls_style.playback_button_diameter),
        );
        let open_file_button_rect = open_file_button_anchor_rect(row_rect, controls_style);
        let fullscreen_button_rect = fullscreen_button_anchor_rect(row_rect, controls_style);

        assert_eq!(open_file_button_rect.size(), fullscreen_button_rect.size());
        assert_eq!(
            open_file_button_rect.size(),
            Vec2::splat(controls_style.fullscreen_button_size)
        );
    }

    /// Проверяет, что левый отступ open-file кнопки равен её нижнему отступу.
    #[test]
    fn open_file_button_anchor_rect_left_inset_matches_bottom_inset() {
        let controls_style = MinimalSkin.controls_style();
        let row_rect = Rect::from_min_size(
            pos2(24.0, 80.0),
            vec2(640.0, controls_style.playback_button_diameter),
        );
        let button_rect = open_file_button_anchor_rect(row_rect, controls_style);
        let left_inset = button_rect.left() - row_rect.left();
        let bottom_inset = row_rect.bottom() - button_rect.bottom();

        assert!((left_inset - bottom_inset).abs() < f32::EPSILON);
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

    /// Проверяет, что правый отступ fullscreen-кнопки равен нижнему отступу.
    #[test]
    fn fullscreen_button_anchor_rect_right_inset_matches_bottom_inset() {
        let controls_style = MinimalSkin.controls_style();
        let row_rect = Rect::from_min_size(
            pos2(24.0, 80.0),
            vec2(640.0, controls_style.playback_button_diameter),
        );
        let button_rect = fullscreen_button_anchor_rect(row_rect, controls_style);
        let right_inset = row_rect.right() - button_rect.right();
        let bottom_inset = row_rect.bottom() - button_rect.bottom();

        assert!((right_inset - bottom_inset).abs() < f32::EPSILON);
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

    /// Проверяет, что open-file, play/pause и fullscreen стоят на одной вертикальной оси.
    #[test]
    fn open_file_button_anchor_rect_center_y_matches_other_anchored_buttons() {
        let controls_style = MinimalSkin.controls_style();
        let row_rect = Rect::from_min_size(
            pos2(24.0, 80.0),
            vec2(640.0, controls_style.playback_button_diameter),
        );
        let open_file_button_rect = open_file_button_anchor_rect(row_rect, controls_style);
        let playback_button_rect = playback_button_anchor_rect(row_rect, controls_style);
        let fullscreen_button_rect = fullscreen_button_anchor_rect(row_rect, controls_style);

        assert!(
            (open_file_button_rect.center().y - playback_button_rect.center().y).abs()
                < f32::EPSILON
        );
        assert!(
            (open_file_button_rect.center().y - fullscreen_button_rect.center().y).abs()
                < f32::EPSILON
        );
    }

    /// Проверяет, что volume зона начинается после open-file кнопки с зеркальным отступом.
    #[test]
    fn volume_controls_zone_rect_stays_between_open_file_and_playback_buttons() {
        let controls_style = MinimalSkin.controls_style();
        let row_rect = Rect::from_min_size(
            pos2(24.0, 80.0),
            vec2(640.0, controls_style.playback_button_diameter),
        );
        let open_file_button_rect = open_file_button_anchor_rect(row_rect, controls_style);
        let playback_button_rect = playback_button_anchor_rect(row_rect, controls_style);
        let volume_to_playback_gap = 8.0;
        let volume_zone_rect = volume_controls_zone_rect(
            row_rect,
            open_file_button_rect,
            playback_button_rect,
            volume_to_playback_gap,
        );
        let open_button_left_inset = open_file_button_rect.left() - row_rect.left();
        let open_button_to_volume_gap = volume_zone_rect.left() - open_file_button_rect.right();

        assert!((open_button_to_volume_gap - open_button_left_inset).abs() < f32::EPSILON);
        assert!(volume_zone_rect.right() <= playback_button_rect.left() - volume_to_playback_gap);
        assert!(volume_zone_rect.left() >= open_file_button_rect.right());
        assert!(volume_zone_rect.right() <= playback_button_rect.left());
    }

    /// Проверяет, что custom volume widgets остаются внутри volume-zone и центрируются как buttons.
    #[test]
    fn volume_control_layout_stays_in_zone_and_matches_button_center_y() {
        let controls_style = MinimalSkin.controls_style();
        let row_rect = Rect::from_min_size(
            pos2(24.0, 80.0),
            vec2(640.0, controls_style.playback_button_diameter),
        );
        let open_file_button_rect = open_file_button_anchor_rect(row_rect, controls_style);
        let playback_button_rect = playback_button_anchor_rect(row_rect, controls_style);
        let volume_zone_rect =
            volume_controls_zone_rect(row_rect, open_file_button_rect, playback_button_rect, 8.0);
        let layout = volume_control_layout(volume_zone_rect, controls_style, 8.0);

        assert!(volume_zone_rect.contains_rect(layout.separator_rect));
        assert!(volume_zone_rect.contains_rect(layout.icon_button_rect));
        assert!(volume_zone_rect.contains_rect(layout.slider_hit_rect));
        assert!(volume_zone_rect.contains_rect(layout.slider_track_rect));
        assert!(layout.separator_rect.right() <= layout.icon_button_rect.left());
        assert!(layout.icon_button_rect.right() <= layout.slider_hit_rect.left());
        assert!(
            (layout.icon_button_rect.center().y - playback_button_rect.center().y).abs()
                < f32::EPSILON
        );
        assert!(
            (layout.slider_hit_rect.center().y - playback_button_rect.center().y).abs()
                < f32::EPSILON
        );
        assert!(
            (layout.slider_track_rect.center().y - playback_button_rect.center().y).abs()
                < f32::EPSILON
        );
    }

    /// Проверяет узкий viewport: volume layout не выходит из своей зоны даже без места.
    #[test]
    fn volume_control_layout_clamps_widgets_inside_tiny_zone() {
        let controls_style = MinimalSkin.controls_style();
        let tiny_zone_rect = Rect::from_min_size(
            pos2(42.0, 80.0),
            vec2(0.0, controls_style.playback_button_diameter),
        );
        let layout = volume_control_layout(tiny_zone_rect, controls_style, 8.0);

        assert!(tiny_zone_rect.contains_rect(layout.separator_rect));
        assert!(tiny_zone_rect.contains_rect(layout.icon_button_rect));
        assert!(tiny_zone_rect.contains_rect(layout.slider_hit_rect));
        assert!(tiny_zone_rect.contains_rect(layout.slider_track_rect));
    }

    /// Проверяет icon-state: звук есть только при `!muted` и volume выше EPSILON.
    #[test]
    fn volume_icon_state_is_audible_only_for_unmuted_nonzero_volume() {
        let mut snapshot = PlayerSnapshot::empty();

        snapshot.volume = 0.5;
        snapshot.muted = false;
        assert_eq!(volume_icon_state(&snapshot), VolumeIconState::Audible);

        snapshot.volume = 0.5;
        snapshot.muted = true;
        assert_eq!(volume_icon_state(&snapshot), VolumeIconState::Muted);

        snapshot.volume = 0.0;
        snapshot.muted = false;
        assert_eq!(volume_icon_state(&snapshot), VolumeIconState::Muted);

        snapshot.volume = f32::EPSILON;
        snapshot.muted = false;
        assert_eq!(volume_icon_state(&snapshot), VolumeIconState::Muted);
    }

    /// Проверяет, что pointer mapping не выпускает slider за public volume диапазон.
    #[test]
    fn volume_from_pointer_x_clamps_to_public_volume_range() {
        let track_rect = Rect::from_min_max(pos2(10.0, 0.0), pos2(110.0, 3.0));

        assert_eq!(volume_from_pointer_x(track_rect, -50.0), 0.0);
        assert_eq!(volume_from_pointer_x(track_rect, 10.0), 0.0);
        assert_eq!(volume_from_pointer_x(track_rect, 60.0), 0.5);
        assert_eq!(volume_from_pointer_x(track_rect, 110.0), 1.0);
        assert_eq!(volume_from_pointer_x(track_rect, 500.0), 1.0);
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
