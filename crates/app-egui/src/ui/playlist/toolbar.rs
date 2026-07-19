//! Toolbar и URL form renderer без I/O и business mutations.

mod icon_bar;

use playlist_core::PlaylistSortKey;

use crate::playlist_runtime::PlaylistInteractionModel;
use crate::ui::skin::PlaylistToolbarStyle;

use super::PlaylistUiOutput;
use super::actions::{PlaylistAction, PlaylistUrlDraftText};

const SORT_KEYS: [(PlaylistSortKey, &str); 6] = [
    (PlaylistSortKey::NaturalFilename, "Имя файла"),
    (PlaylistSortKey::Title, "Название"),
    (PlaylistSortKey::Artist, "Исполнитель"),
    (PlaylistSortKey::Album, "Альбом"),
    (PlaylistSortKey::Duration, "Длительность"),
    (PlaylistSortKey::SmartSequence, "Умная последовательность"),
];

pub(super) fn show(
    ui: &mut egui::Ui,
    model: &PlaylistInteractionModel,
    style: PlaylistToolbarStyle,
    output: &mut PlaylistUiOutput,
) {
    icon_bar::show(ui, model, style, output);

    if model.url_editor_open {
        show_url_editor(ui, model, output);
    }
}

fn show_url_editor(
    ui: &mut egui::Ui,
    model: &PlaylistInteractionModel,
    output: &mut PlaylistUiOutput,
) {
    ui.group(|ui| {
        let interaction_enabled = ui.is_enabled();
        let label = ui.label("URL медиа:");
        let mut editable_text = model.url_text.clone();
        let response = ui
            .add(
                egui::TextEdit::singleline(&mut editable_text)
                    .id_salt("playlist_inline_url")
                    .hint_text("https://…"),
            )
            .labelled_by(label.id);
        if interaction_enabled && model.url_request_focus {
            response.request_focus();
            output.push_action(PlaylistAction::UrlFocusRestored);
        }
        if response.changed() {
            output.push_action(PlaylistAction::UpdateUrlDraft(PlaylistUrlDraftText::new(
                editable_text,
            )));
        }
        let submit_by_enter = interaction_enabled
            && response.lost_focus()
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        let cancel_by_escape = interaction_enabled
            && response.has_focus()
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        ui.horizontal(|ui| {
            if ui.button("Добавить").clicked() || submit_by_enter {
                output.push_action(PlaylistAction::SubmitUrl);
            }
            if ui.button("Отмена").clicked() || cancel_by_escape {
                output.push_action(PlaylistAction::CancelUrlEditor);
            }
        });
        if let Some(error) = &model.url_safe_error {
            ui.colored_label(ui.visuals().error_fg_color, error.message());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::SORT_KEYS;

    #[test]
    fn sort_menu_exposes_every_required_key_exactly_once() {
        let actual_keys: Vec<_> = SORT_KEYS.into_iter().map(|(key, _)| key).collect();
        assert_eq!(actual_keys.len(), 6);
        for required in [
            playlist_core::PlaylistSortKey::NaturalFilename,
            playlist_core::PlaylistSortKey::Title,
            playlist_core::PlaylistSortKey::Artist,
            playlist_core::PlaylistSortKey::Album,
            playlist_core::PlaylistSortKey::Duration,
            playlist_core::PlaylistSortKey::SmartSequence,
        ] {
            assert_eq!(
                actual_keys.iter().filter(|key| **key == required).count(),
                1
            );
        }
    }

    #[test]
    fn inline_url_enter_is_consumed_after_focus_loss() {
        let source = include_str!("toolbar.rs");
        let enter_branch = source
            .split_once("let submit_by_enter")
            .expect("Enter branch должен существовать")
            .1
            .split_once("let cancel_by_escape")
            .expect("Enter branch должен быть bounded")
            .0;

        assert!(enter_branch.contains("input_mut"));
        assert!(enter_branch.contains("consume_key"));
        assert!(enter_branch.contains("egui::Key::Enter"));
        assert!(!enter_branch.contains("key_pressed"));
    }

    #[test]
    fn queue_mode_controls_are_not_duplicated_in_playlist_toolbar() {
        let toolbar_source = include_str!("toolbar.rs")
            .split_once("#[cfg(test)]")
            .expect("toolbar tests must stay after production code")
            .0;
        let icon_bar_source = include_str!("toolbar/icon_bar.rs")
            .split_once("#[cfg(test)]")
            .expect("icon bar tests must stay after production code")
            .0;
        let production_source = format!("{toolbar_source}\n{icon_bar_source}");

        assert!(!production_source.contains("SetRepeatMode"));
        assert!(!production_source.contains("SetShuffle"));
        assert!(!production_source.contains("Перемешать"));
        assert!(!production_source.contains("Повтор:"));
        assert!(!production_source.contains("SetStopAfterCurrent"));
        assert!(!production_source.contains("После текущего"));
        assert!(!production_source.contains("UndoRemoval"));
        assert!(!production_source.contains("PlaylistUndoUiSnapshot"));
        assert!(production_source.contains("PlaylistToolbarGlyph::Sort"));
        assert!(toolbar_source.contains("icon_bar::show"));
    }
}
