//! Player controls поверх command/snapshot boundary.

use egui::{Button, RichText, Ui};
use player_core::{PlaybackState, PlayerSnapshot};

use crate::ui::assets::IconId;
use crate::ui::skin::{PlayerSkin, SkinId};
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
            render_button_row(ui, player_snapshot, skin, &mut actions);
        });

    actions
}

/// Рисует строку кнопок и volume slider.
fn render_button_row(
    ui: &mut Ui,
    player_snapshot: &PlayerSnapshot,
    skin: &impl PlayerSkin,
    actions: &mut Vec<ControlAction>,
) {
    let controls_style = skin.controls_style();
    let play_icon = playback_toggle_icon(player_snapshot.playback_state);
    let mut volume_value = player_snapshot.volume;

    ui.horizontal_wrapped(|ui| {
        if icon_button(ui, play_icon, skin).clicked() {
            actions.push(ControlAction::TogglePlayback);
        }

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

        if icon_button(ui, IconId::Fullscreen, skin).clicked() {
            actions.push(ControlAction::ToggleFullscreen);
        }
    });
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

/// Рисует кнопку для asset-backed иконки.
fn icon_button(ui: &mut Ui, icon_id: IconId, skin: &impl PlayerSkin) -> egui::Response {
    let asset_id = skin.icon_asset(icon_id);

    ui.push_id(asset_id, |ui| {
        fixed_button(ui, skin.icon_text(icon_id), skin)
    })
    .inner
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
}
