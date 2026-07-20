//! Explicit append/replace menu icon-only Import control-а.

use egui::{Popup, Response, Ui};

use crate::playlist_runtime::{PlaylistImportIntent, PlaylistInteractionModel};

use super::super::{PlaylistAction, PlaylistUiOutput};

/// Два menu item-а являются разными product intents, а не label-driven bool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportMenuItem {
    AppendToQueue,
    ReplaceQueue,
}

const IMPORT_MENU_ITEMS: [ImportMenuItem; 2] =
    [ImportMenuItem::AppendToQueue, ImportMenuItem::ReplaceQueue];

impl ImportMenuItem {
    const fn label(self) -> &'static str {
        match self {
            Self::AppendToQueue => "Добавить к плейлисту",
            Self::ReplaceQueue => "Открыть как новый плейлист",
        }
    }

    const fn action(self) -> PlaylistAction {
        let intent = match self {
            Self::AppendToQueue => PlaylistImportIntent::AppendToQueue,
            Self::ReplaceQueue => PlaylistImportIntent::ReplaceQueue,
        };
        PlaylistAction::StartImport(intent)
    }
}

/// Import menu публикует typed post-render intent и не открывает файл само.
pub(super) fn show(
    ui: &Ui,
    response: &Response,
    model: &PlaylistInteractionModel,
    output: &mut PlaylistUiOutput,
) {
    let model_enabled =
        !model.import_dialog_open && model.structural_action_availability.allows_interaction();
    let effective_enabled = ui.is_enabled() && model_enabled;
    let popup_id = Popup::default_response_id(response);
    if !effective_enabled {
        Popup::close_id(ui.ctx(), popup_id);
        return;
    }

    Popup::menu(response).show(|ui| {
        for item in IMPORT_MENU_ITEMS {
            if ui.button(item.label()).clicked() {
                output.push_action(item.action());
                ui.close();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_items_preserve_exact_visible_order_and_typed_intents() {
        assert_eq!(
            IMPORT_MENU_ITEMS.map(ImportMenuItem::label),
            ["Добавить к плейлисту", "Открыть как новый плейлист",]
        );
        assert_eq!(
            IMPORT_MENU_ITEMS.map(ImportMenuItem::action),
            [
                PlaylistAction::StartImport(PlaylistImportIntent::AppendToQueue),
                PlaylistAction::StartImport(PlaylistImportIntent::ReplaceQueue),
            ]
        );
    }
}
