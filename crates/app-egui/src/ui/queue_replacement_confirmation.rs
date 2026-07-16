//! Generalized D15+D79 confirmation внутри общего central overlay host-а.

use crate::playlist_runtime::{
    PendingPlaylistConfirmation, PlaylistConfirmationAction, QueueReplacementConfirmationDecision,
};

/// Рендерит safe immutable model и возвращает не более одного typed action за кадр.
pub(crate) fn render(
    ui: &mut egui::Ui,
    model: &PendingPlaylistConfirmation,
) -> Option<PlaylistConfirmationAction> {
    let mut action = None;
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            let reasons = model.reasons();
            ui.heading(
                if reasons.queue_replacement() && reasons.sensitive_url_persistence() {
                    "Подтвердить открытие и сохранение?"
                } else if reasons.queue_replacement() {
                    "Заменить текущую очередь?"
                } else {
                    "Сохранить URL в плейлисте?"
                },
            );
            if reasons.queue_replacement() {
                ui.label(format!(
                    "Открыть «{}» и заменить непустую очередь?",
                    model.safe_label()
                ));
            }
            if reasons.sensitive_url_persistence() {
                ui.label("URL содержит чувствительные параметры. Сохранить его в playlist-state?");
            }
            ui.horizontal(|ui| {
                if ui.button("Отмена").clicked() {
                    action = Some(action_for(
                        model,
                        QueueReplacementConfirmationDecision::Cancel,
                    ));
                }
                if ui.button("Подтвердить").clicked() {
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
    model: &PendingPlaylistConfirmation,
    decision: QueueReplacementConfirmationDecision,
) -> PlaylistConfirmationAction {
    PlaylistConfirmationAction {
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
