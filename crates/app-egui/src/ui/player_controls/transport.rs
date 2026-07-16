//! Prototype transport/status UI поверх typed runtime model.
//!
//! Standard egui buttons отвечают за interaction/accessibility. Traversal и wait semantics
//! остаются у `PlaylistRuntime`; этот модуль только возвращает typed UI intents.

use egui::{Button, Rect, Ui, vec2};

use crate::playlist_runtime::{NavigationControlAvailability, PlaylistTransportUiModel};

use super::ControlAction;

const NAVIGATION_BUTTON_WIDTH: f32 = 104.0;

/// Один transport intent; origin фиксируется adapter-ом как `Ui`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportControlAction {
    Previous,
    TogglePlayback,
    Next,
    CancelNavigation,
    UndoRemoval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrototypeButtonSpec {
    label: &'static str,
    action: TransportControlAction,
}

const PROTOTYPE_BUTTONS: [PrototypeButtonSpec; 3] = [
    PrototypeButtonSpec {
        label: "Предыдущий",
        action: TransportControlAction::Previous,
    },
    PrototypeButtonSpec {
        label: "Воспроизвести / Пауза",
        action: TransportControlAction::TogglePlayback,
    },
    PrototypeButtonSpec {
        label: "Следующий",
        action: TransportControlAction::Next,
    },
];

pub(super) fn render_previous_button(
    ui: &mut Ui,
    playback_button_rect: Rect,
    availability: NavigationControlAvailability,
    actions: &mut Vec<ControlAction>,
) {
    let rect = previous_button_rect(ui, playback_button_rect);
    let response = ui
        .add_enabled_ui(availability.is_enabled(), |ui| {
            ui.put(rect, Button::new(PROTOTYPE_BUTTONS[0].label))
        })
        .inner
        .on_disabled_hover_text(availability.explanation());
    if response.clicked() {
        actions.push(ControlAction::Transport(TransportControlAction::Previous));
    }
}

pub(super) fn previous_button_rect(ui: &Ui, playback_button_rect: Rect) -> Rect {
    let spacing = ui.spacing().item_spacing.x;
    Rect::from_center_size(
        egui::pos2(
            playback_button_rect.left() - spacing - NAVIGATION_BUTTON_WIDTH / 2.0,
            playback_button_rect.center().y,
        ),
        vec2(NAVIGATION_BUTTON_WIDTH, ui.spacing().interact_size.y),
    )
}

pub(super) fn render_next_button(
    ui: &mut Ui,
    playback_button_rect: Rect,
    availability: NavigationControlAvailability,
    actions: &mut Vec<ControlAction>,
) {
    let rect = next_button_rect(ui, playback_button_rect);
    let response = ui
        .add_enabled_ui(availability.is_enabled(), |ui| {
            ui.put(rect, Button::new(PROTOTYPE_BUTTONS[2].label))
        })
        .inner
        .on_disabled_hover_text(availability.explanation());
    if response.clicked() {
        actions.push(ControlAction::Transport(TransportControlAction::Next));
    }
}

pub(super) fn next_button_rect(ui: &Ui, playback_button_rect: Rect) -> Rect {
    next_button_rect_with_spacing(
        playback_button_rect,
        ui.spacing().item_spacing.x,
        ui.spacing().interact_size.y,
    )
}

fn next_button_rect_with_spacing(
    playback_button_rect: Rect,
    spacing: f32,
    button_height: f32,
) -> Rect {
    Rect::from_center_size(
        egui::pos2(
            playback_button_rect.right() + spacing + NAVIGATION_BUTTON_WIDTH / 2.0,
            playback_button_rect.center().y,
        ),
        vec2(NAVIGATION_BUTTON_WIDTH, button_height),
    )
}

/// D80 status и recovery actions не зависят от sidebar visibility.
pub(super) fn render_global_status(
    ui: &mut Ui,
    model: &PlaylistTransportUiModel,
    actions: &mut Vec<ControlAction>,
) {
    if model.global_status.is_none() && model.undo.is_none() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        if let Some(status) = model.global_status {
            ui.label(status.label());
            if status.can_cancel() && ui.button("Отменить ожидание").clicked() {
                actions.push(ControlAction::Transport(
                    TransportControlAction::CancelNavigation,
                ));
            }
        }
        if let Some(undo) = model.undo {
            let label = format!(
                "Отменить {} ({} с)",
                undo.kind_label, undo.seconds_remaining
            );
            if ui.button(label).clicked() {
                actions.push(ControlAction::Transport(
                    TransportControlAction::UndoRemoval,
                ));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::skin::PlayerSkin;

    #[test]
    fn prototype_transport_order_and_accessible_labels_are_stable() {
        assert_eq!(
            PROTOTYPE_BUTTONS.map(|button| (button.label, button.action)),
            [
                ("Предыдущий", TransportControlAction::Previous),
                (
                    "Воспроизвести / Пауза",
                    TransportControlAction::TogglePlayback,
                ),
                ("Следующий", TransportControlAction::Next),
            ]
        );
    }

    #[test]
    fn availability_preserves_disabled_wait_and_pending_explanations() {
        assert!(!NavigationControlAvailability::Disabled.is_enabled());
        assert!(NavigationControlAvailability::PotentialWait.is_enabled());
        assert!(NavigationControlAvailability::Pending.is_enabled());
        assert_ne!(
            NavigationControlAvailability::Disabled.explanation(),
            NavigationControlAvailability::PotentialWait.explanation()
        );
        assert_ne!(
            NavigationControlAvailability::PotentialWait.explanation(),
            NavigationControlAvailability::Pending.explanation()
        );
    }

    #[test]
    fn next_and_playback_rate_buttons_do_not_overlap() {
        let row_rect = Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(640.0, 48.0));
        let playback_rect = Rect::from_center_size(row_rect.center(), vec2(48.0, 48.0));
        let next_rect = next_button_rect_with_spacing(playback_rect, 8.0, 24.0);
        let fullscreen_rect = Rect::from_center_size(
            egui::pos2(row_rect.right() - 24.0, row_rect.center().y),
            vec2(32.0, 32.0),
        );
        let rate_rect = super::super::playback_rate::button_rect(
            row_rect,
            next_rect,
            fullscreen_rect,
            crate::ui::skin::MinimalSkin.controls_style(),
            8.0,
        );

        assert!(next_rect.right() <= rate_rect.left());
        assert!(rate_rect.right() <= fullscreen_rect.left());
    }
}
