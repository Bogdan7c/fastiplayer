//! Содержимое D79 confirmation внутри общего central overlay host-а.

use crate::playlist_runtime::{
    PendingQueueReplacementConfirmation, QueueReplacementConfirmationAction,
    QueueReplacementConfirmationDecision,
};

/// Рендерит safe immutable model и возвращает не более одного typed action за кадр.
pub(crate) fn render(
    ui: &mut egui::Ui,
    model: &PendingQueueReplacementConfirmation,
) -> Option<QueueReplacementConfirmationAction> {
    let mut action = None;
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.heading("Заменить текущую очередь?");
            ui.label(format!(
                "Открыть «{}» и заменить непустую очередь?",
                model.safe_label()
            ));
            ui.horizontal(|ui| {
                if ui.button("Отмена").clicked() {
                    action = Some(action_for(
                        model,
                        QueueReplacementConfirmationDecision::Cancel,
                    ));
                }
                if ui.button("Заменить и открыть").clicked() {
                    action = Some(action_for(
                        model,
                        QueueReplacementConfirmationDecision::Confirm,
                    ));
                }
            });
        });
    });
    action
}

fn action_for(
    model: &PendingQueueReplacementConfirmation,
    decision: QueueReplacementConfirmationDecision,
) -> QueueReplacementConfirmationAction {
    QueueReplacementConfirmationAction {
        intent_id: model.intent_id(),
        decision,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn entity_does_not_create_a_second_overlay_host() {
        let source = include_str!("queue_replacement_confirmation.rs");
        assert!(!source.contains(&["egui::", "Window"].concat()));
        assert!(!source.contains(&["egui::", "Area"].concat()));
        assert!(!source.contains(&["Central", "Panel"].concat()));
    }
}
