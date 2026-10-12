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
                if reasons
                    .sensitive_playlist_export_locator_count()
                    .is_some()
                {
                    "Export sensitive addresses?"
                } else if reasons.queue_replacement() && reasons.sensitive_url_persistence() {
                    "Confirm opening and saving?"
                } else if reasons.queue_replacement() {
                    "Replace the current queue?"
                } else {
                    "Save URL in Playlist?"
                },
            );
            if reasons.queue_replacement() {
                ui.label(format!(
                    "Open “{}” and replace the nonempty queue?",
                    model.safe_label()
                ));
            }
            if reasons.sensitive_url_persistence() {
                ui.label("The URL contains sensitive parameters. Save it in Playlist storage?");
            }
            if let Some(locator_count) = reasons.sensitive_playlist_export_locator_count() {
                ui.label(format!(
                    "The file will contain sensitive addresses ({locator_count}). Save it with access restricted to the current user?"
                ));
            }
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    action = Some(action_for(
                        model,
                        QueueReplacementConfirmationDecision::Cancel,
                    ));
                }
                if ui.button("Confirm").clicked() {
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
