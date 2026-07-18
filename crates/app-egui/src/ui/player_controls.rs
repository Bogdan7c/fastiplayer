//! Player controls поверх command/snapshot boundary.

use egui::{Rect, Sense, Stroke, Ui, Vec2, WidgetInfo, WidgetType, pos2, vec2};
use player_core::{PlaybackState, PlayerSnapshot};
use ui_artwork_egui::{
    ArtworkPainter, ButtonVisualState, FullscreenGlyph, FullscreenStyle, PlaybackGlyph,
    PlaybackStyle, VOLUME_THUMB_RADIUS, VOLUME_TRACK_HEIGHT, VolumeGlyph,
};

use crate::ui::assets::IconId;
use crate::ui::skin::{ControlsStyle, PlayerSkin, SkinId};
use crate::ui::timeline::{self, TimelineAction, TimelineUiState};

mod playback_rate;
mod queue_mode_controls;
mod transport;
pub(crate) use playback_rate::PLAYBACK_RATE_STEP_X;
pub(crate) use transport::TransportControlAction;

const VOLUME_SEPARATOR_WIDTH: f32 = 1.0;
const VOLUME_SEPARATOR_HEIGHT_FACTOR: f32 = 0.68;

/// Преобразует egui hover/drag flag в нейтральное visual state.
fn button_visual_state(interactive: bool) -> ButtonVisualState {
    if interactive {
        ButtonVisualState::Hovered
    } else {
        ButtonVisualState::Idle
    }
}

/// Действие controls, которое shell должен применить после egui pass.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlAction {
    /// Playlist-aware transport intent, применяемый process/runtime owner-ом.
    Transport(TransportControlAction),

    /// Открыть файл.
    OpenFile,

    /// Установить новую громкость.
    SetVolume(f32),

    /// Переключить mute/unmute.
    ToggleMute,

    /// Изменить скорость playback на N UI-шагов.
    AdjustPlaybackRateSteps(i32),

    /// Вернуть скорость playback к нормальной `1.0x`.
    ResetPlaybackRate,

    /// Переключить fullscreen.
    ToggleFullscreen,

    /// Действие timeline.
    Timeline(TimelineAction),
}

/// Рисует нижнюю player controls панель и возвращает действия пользователя.
pub(crate) struct BottomControlsInput<'a, S: PlayerSkin> {
    pub(crate) player_snapshot: &'a PlayerSnapshot,
    pub(crate) timeline_state: &'a mut TimelineUiState,
    pub(crate) timeline_inline_status: Option<&'a str>,
    pub(crate) skin: &'a S,
    pub(crate) is_window_fullscreen: bool,
    pub(crate) live_scrub_enabled: bool,
    pub(crate) reduced_motion: bool,
    pub(crate) playlist_transport: &'a crate::playlist_runtime::PlaylistTransportUiModel,
}

/// Рисует нижнюю player controls панель и возвращает действия пользователя.
#[must_use]
pub fn render_bottom_controls<S: PlayerSkin>(
    ui: &mut Ui,
    input: BottomControlsInput<'_, S>,
) -> Vec<ControlAction> {
    let BottomControlsInput {
        player_snapshot,
        timeline_state,
        timeline_inline_status,
        skin,
        is_window_fullscreen,
        live_scrub_enabled,
        reduced_motion,
        playlist_transport,
    } = input;
    let mut actions = Vec::new();
    let panel_id = bottom_panel_id(skin.id());

    egui::Panel::bottom(panel_id)
        .frame(skin.bottom_panel_frame())
        .show_inside(ui, |ui| {
            timeline::render_time_labels(
                ui,
                &player_snapshot.timeline,
                timeline_state,
                timeline_inline_status,
            );
            let timeline_interaction = timeline::render_timeline(
                ui,
                &player_snapshot.timeline,
                timeline_state,
                skin,
                live_scrub_enabled,
            );
            actions.extend(
                timeline_interaction
                    .actions
                    .into_iter()
                    .map(ControlAction::Timeline),
            );

            ui.add_space(4.0);
            transport::render_global_status(ui, playlist_transport, &mut actions);
            render_button_row(
                ui,
                player_snapshot,
                skin,
                is_window_fullscreen,
                reduced_motion,
                playlist_transport,
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
    reduced_motion: bool,
    playlist_transport: &crate::playlist_runtime::PlaylistTransportUiModel,
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
    let queue_mode_layout = queue_mode_controls::control_layout(
        playback_button_rect,
        open_file_button_rect,
        fullscreen_button_rect,
        controls_style,
        ui.spacing().item_spacing.x,
    );
    let base_next_button_rect = transport::next_button_rect(playback_button_rect, controls_style);
    let playback_rate_reveal_progress =
        playback_rate::reveal_progress(ui, player_snapshot.playback_rate, reduced_motion);
    let playback_rate_layout = playback_rate::control_layout(
        playback_button_rect,
        base_next_button_rect,
        queue_mode_layout.repeat_rect,
        controls_style,
        ui.spacing().item_spacing.x,
        playback_rate_reveal_progress,
    );
    let volume_to_playback_gap = ui.spacing().item_spacing.x;
    let previous_button_rect =
        transport::previous_button_rect(playback_button_rect, controls_style);
    let volume_zone = volume_controls_zone_rect(
        row_rect,
        open_file_button_rect,
        queue_mode_layout.shuffle_rect,
        volume_to_playback_gap,
    );

    if render_open_file_button_at(ui, open_file_button_rect, skin).clicked() {
        actions.push(ControlAction::OpenFile);
    }

    render_volume_controls(ui, player_snapshot, controls_style, volume_zone, actions);

    transport::render_previous_button(
        ui,
        previous_button_rect,
        playlist_transport.previous,
        controls_style,
        actions,
    );

    queue_mode_controls::render(
        ui,
        queue_mode_layout,
        playlist_transport,
        controls_style,
        reduced_motion,
        actions,
    );

    let playback_button_response =
        render_playback_toggle_button_at(ui, playback_button_rect, play_icon, skin);
    playback_rate::collect_input_actions(ui, &playback_button_response, actions);

    if playback_button_response.clicked() {
        actions.push(ControlAction::Transport(
            TransportControlAction::TogglePlayback,
        ));
    }

    playback_rate::render_reset_button_at(
        ui,
        playback_rate_layout,
        playback_button_rect,
        player_snapshot,
        controls_style,
        actions,
    );

    transport::render_next_button(
        ui,
        playback_rate_layout.next_button_rect,
        playlist_transport.next,
        controls_style,
        actions,
    );

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
    let preferred_zone_left = (open_file_button_rect.right() + open_button_left_inset)
        .clamp(row_rect.left(), row_rect.right());
    let zone_right = (playback_button_rect.left() - volume_to_playback_gap)
        .clamp(open_file_button_rect.right(), row_rect.right());
    // На минимальной ширине volume схлопывается у правой границы своей зоны,
    // а не выталкивается в Shuffle из-за предпочтительного левого отступа.
    let zone_left = preferred_zone_left.min(zone_right);

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

fn paint_volume_separator(ui: &Ui, separator_rect: Rect, controls_style: ControlsStyle) {
    ArtworkPainter::new(ui.painter()).volume_separator(
        separator_rect,
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
    ArtworkPainter::new(ui.painter()).volume_button(
        button_rect,
        match icon_state {
            VolumeIconState::Audible => VolumeGlyph::Audible,
            VolumeIconState::Muted => VolumeGlyph::Muted,
        },
        button_visual_state(button_response.hovered()),
        Stroke::new(
            controls_style.playback_button_stroke_width,
            controls_style.text_color,
        ),
        controls_style.playback_button_hover_fill,
    );
}

fn paint_volume_slider(
    ui: &Ui,
    track_rect: Rect,
    controls_style: ControlsStyle,
    volume: f32,
    is_interactive: bool,
) {
    ArtworkPainter::new(ui.painter()).volume_slider(
        track_rect,
        volume,
        button_visual_state(is_interactive),
        controls_style.text_color,
        controls_style.playback_button_stroke_width,
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

    if button_response.clicked() {
        button_response.request_focus();
    }

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
    if crate::transport_runtime::playback_toggle_will_pause(playback_state) {
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

fn paint_fullscreen_toggle_button(
    ui: &Ui,
    button_rect: Rect,
    icon: FullscreenToggleIcon,
    skin: &impl PlayerSkin,
    button_response: &egui::Response,
) {
    let style = skin.controls_style();
    ArtworkPainter::new(ui.painter()).fullscreen_button(
        button_rect,
        match icon {
            FullscreenToggleIcon::EnterFullscreen => FullscreenGlyph::Enter,
            FullscreenToggleIcon::ExitFullscreen => FullscreenGlyph::Exit,
        },
        button_visual_state(button_response.hovered()),
        FullscreenStyle {
            icon_extent: style.fullscreen_icon_extent,
            stroke: Stroke::new(style.playback_button_stroke_width, style.text_color),
            hover_fill: style.playback_button_hover_fill,
        },
    );
}

fn paint_open_file_button(
    ui: &Ui,
    button_rect: Rect,
    skin: &impl PlayerSkin,
    button_response: &egui::Response,
) {
    let style = skin.controls_style();
    ArtworkPainter::new(ui.painter()).open_media_button(
        button_rect,
        button_visual_state(button_response.hovered()),
        Stroke::new(style.playback_button_stroke_width, style.text_color),
        style.playback_button_hover_fill,
    );
}

fn paint_playback_button(
    ui: &Ui,
    button_rect: Rect,
    icon_id: IconId,
    skin: &impl PlayerSkin,
    button_response: &egui::Response,
) {
    let style = skin.controls_style();
    ArtworkPainter::new(ui.painter()).playback_button(
        button_rect,
        match icon_id {
            IconId::Play => PlaybackGlyph::Play,
            IconId::Pause => PlaybackGlyph::Pause,
        },
        button_visual_state(button_response.hovered()),
        PlaybackStyle {
            diameter: style.playback_button_diameter,
            icon_extent: style.playback_button_icon_extent,
            stroke_width: style.playback_button_stroke_width,
            color: style.text_color,
            hover_fill: style.playback_button_hover_fill,
        },
    );
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

    /// Проверяет, что skin владеет геометрией центральной, transport и fullscreen-кнопок.
    #[test]
    fn minimal_skin_owns_playback_transport_and_fullscreen_button_geometry() {
        let controls_style = MinimalSkin.controls_style();

        assert!(controls_style.playback_button_diameter > controls_style.button_height);
        assert!(controls_style.playback_button_stroke_width > 0.0);
        assert_eq!(controls_style.playback_button_vertical_raise, 5.0);
        assert!(controls_style.playback_button_icon_extent > 0.0);
        assert!(
            controls_style.playback_button_icon_extent < controls_style.playback_button_diameter
        );
        assert_eq!(controls_style.transport_button_size, 32.0);
        assert_eq!(controls_style.transport_button_center_distance, 64.0);
        assert_eq!(controls_style.transport_button_icon_extent, 18.0);
        assert!(controls_style.transport_button_icon_extent < controls_style.transport_button_size);
        assert!(controls_style.transport_button_bar_width > 0.0);
        assert_eq!(controls_style.queue_mode_button_center_distance, 156.0);
        assert_eq!(controls_style.queue_mode_neighbor_gap, 12.0);
        assert!(controls_style.queue_mode_glyph_stroke_width > 0.0);
        assert_eq!(
            controls_style.persistent_control.foreground_idle,
            egui::Color32::from_gray(170)
        );
        assert_eq!(
            controls_style.persistent_control.foreground_hover,
            egui::Color32::from_gray(230)
        );
        assert_eq!(
            controls_style.persistent_control.foreground_active,
            egui::Color32::from_gray(245)
        );
        assert_eq!(
            controls_style.persistent_control.foreground_disabled,
            egui::Color32::from_gray(105)
        );
        assert_eq!(controls_style.playback_rate_button_width, 48.0);
        assert_eq!(controls_style.playback_rate_button_gap, 5.0);
        assert_eq!(controls_style.playback_rate_button_vertical_inset, 2.0);
        assert!(controls_style.playback_rate_button_stroke_width > 0.0);
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

    /// Проверяет, что volume зона остаётся между open-file и Shuffle.
    #[test]
    fn volume_controls_zone_rect_stays_between_open_file_and_shuffle() {
        let controls_style = MinimalSkin.controls_style();
        let row_rect = Rect::from_min_size(
            pos2(24.0, 80.0),
            vec2(640.0, controls_style.playback_button_diameter),
        );
        let open_file_button_rect = open_file_button_anchor_rect(row_rect, controls_style);
        let playback_button_rect = playback_button_anchor_rect(row_rect, controls_style);
        let fullscreen_button_rect = fullscreen_button_anchor_rect(row_rect, controls_style);
        let shuffle_button_rect = queue_mode_controls::control_layout(
            playback_button_rect,
            open_file_button_rect,
            fullscreen_button_rect,
            controls_style,
            8.0,
        )
        .shuffle_rect;
        let volume_to_playback_gap = 8.0;
        let volume_zone_rect = volume_controls_zone_rect(
            row_rect,
            open_file_button_rect,
            shuffle_button_rect,
            volume_to_playback_gap,
        );
        let open_button_left_inset = open_file_button_rect.left() - row_rect.left();
        let open_button_to_volume_gap = volume_zone_rect.left() - open_file_button_rect.right();

        assert!((open_button_to_volume_gap - open_button_left_inset).abs() < f32::EPSILON);
        assert!(volume_zone_rect.right() <= shuffle_button_rect.left() - volume_to_playback_gap);
        assert!(volume_zone_rect.left() >= open_file_button_rect.right());
        assert!(volume_zone_rect.right() <= shuffle_button_rect.left());
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
