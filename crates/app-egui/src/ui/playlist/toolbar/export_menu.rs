//! Explicit scope/format menu icon-only Export control-а.

use egui::{Popup, Response, Ui};
use playlist_io::PlaylistExportFormat;

use crate::playlist_runtime::{
    PlaylistExportRequest, PlaylistExportScopeIntent, PlaylistInteractionModel,
};

use super::super::{PlaylistAction, PlaylistUiOutput};

/// Format выбирается отдельным menu item-ом после explicit scope.
const EXPORT_FORMATS: [(PlaylistExportFormat, &str); 2] = [
    (PlaylistExportFormat::M3u8, "M3U8"),
    (PlaylistExportFormat::Xspf, "XSPF"),
];

/// Export menu не открывает dialog и не читает queue: он публикует typed intent.
pub(super) fn show(
    ui: &Ui,
    response: &Response,
    model: &PlaylistInteractionModel,
    output: &mut PlaylistUiOutput,
) {
    let effective_enabled = ui.is_enabled() && model.item_count > 0 && !model.export_dialog_open;
    let popup_id = Popup::default_response_id(response);
    if !effective_enabled {
        Popup::close_id(ui.ctx(), popup_id);
        return;
    }

    Popup::menu(response).show(|ui| {
        show_scope_branch(
            ui,
            "Весь плейлист",
            PlaylistExportScopeIntent::FullPlaylist,
            scope_enabled(model, PlaylistExportScopeIntent::FullPlaylist),
            output,
        );
        let selected_label = format!("Выбранные ({})", model.selected_item_count);
        show_scope_branch(
            ui,
            &selected_label,
            PlaylistExportScopeIntent::SelectedEntries,
            scope_enabled(model, PlaylistExportScopeIntent::SelectedEntries),
            output,
        );
    });
}

/// Empty selection отключает только selected branch, а не весь export.
const fn scope_enabled(model: &PlaylistInteractionModel, scope: PlaylistExportScopeIntent) -> bool {
    match scope {
        PlaylistExportScopeIntent::FullPlaylist => model.item_count > 0,
        PlaylistExportScopeIntent::SelectedEntries => model.selected_item_count > 0,
    }
}

/// Ветка scope остаётся disabled независимо от доступности соседней ветки.
fn show_scope_branch(
    ui: &mut Ui,
    label: &str,
    scope: PlaylistExportScopeIntent,
    enabled: bool,
    output: &mut PlaylistUiOutput,
) {
    ui.add_enabled_ui(enabled, |ui| {
        ui.menu_button(label, |ui| {
            for (format, format_label) in EXPORT_FORMATS {
                if ui.button(format_label).clicked() {
                    output.push_action(PlaylistAction::StartExport(PlaylistExportRequest {
                        scope,
                        format,
                    }));
                    ui.close();
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_order_and_typed_scope_are_explicit() {
        assert_eq!(
            EXPORT_FORMATS,
            [
                (PlaylistExportFormat::M3u8, "M3U8"),
                (PlaylistExportFormat::Xspf, "XSPF"),
            ]
        );
        let selected = PlaylistExportRequest {
            scope: PlaylistExportScopeIntent::SelectedEntries,
            format: PlaylistExportFormat::Xspf,
        };
        assert_eq!(selected.scope, PlaylistExportScopeIntent::SelectedEntries);
        assert_eq!(selected.format, PlaylistExportFormat::Xspf);
    }

    #[test]
    fn empty_selection_disables_only_selected_branch() {
        let model = PlaylistInteractionModel {
            item_count: 4,
            selected_item_count: 0,
            ..PlaylistInteractionModel::default()
        };

        assert!(scope_enabled(
            &model,
            PlaylistExportScopeIntent::FullPlaylist
        ));
        assert!(!scope_enabled(
            &model,
            PlaylistExportScopeIntent::SelectedEntries
        ));
    }
}
