//! Toolbar/forms/progress renderer без I/O и business mutations.

use playlist_core::{PlaylistSortKey, SortCanonicalQueue, SortDirection};

use crate::playlist_runtime::{PlaylistInteractionModel, PlaylistWaitDirection};

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
    output: &mut PlaylistUiOutput,
) {
    ui.horizontal_wrapped(|ui| {
        let add_files = ui
            .add_enabled(
                model.structural_actions_enabled && !model.file_dialog_open,
                egui::Button::new("Добавить файлы"),
            )
            .on_hover_text("Выбрать несколько файлов и добавить их в конец плейлиста");
        if add_files.clicked() {
            output.push_action(PlaylistAction::AddFiles);
        }

        if ui
            .add_enabled(
                model.structural_actions_enabled,
                egui::Button::new("Добавить URL"),
            )
            .on_hover_text("Открыть встроенную форму добавления URL")
            .clicked()
        {
            output.push_action(PlaylistAction::OpenUrlEditor);
        }

        if ui
            .add_enabled(
                model.structural_actions_enabled && model.item_count > 0,
                egui::Button::new("Очистить"),
            )
            .on_hover_text("Очистить очередь; текущее воспроизведение не останавливается")
            .clicked()
        {
            output.push_action(PlaylistAction::Clear);
        }

        let mut stop_after_current = model.stop_after_current;
        let stop_response = ui
            .add_enabled(
                model.structural_actions_enabled && model.stop_after_current_available,
                egui::Checkbox::new(&mut stop_after_current, "После текущего"),
            )
            .on_hover_text(if stop_after_current {
                "Выключение не возобновит ранее отменённый переход"
            } else {
                "Остановиться после текущего; ожидающий переход будет отменён"
            });
        if stop_response.changed() {
            output.push_action(PlaylistAction::SetStopAfterCurrent(stop_after_current));
        }

        sort_menu(ui, model, output);

        if let Some(target) = model.go_current_target {
            if ui
                .button("Перейти к текущему")
                .on_hover_text(match target {
                    crate::playlist_runtime::PlaylistGoCurrentTarget::Row(_) => {
                        "Сфокусировать и показать текущую строку"
                    }
                    crate::playlist_runtime::PlaylistGoCurrentTarget::Tombstone => {
                        "Показать продолжающее воспроизводиться удалённое медиа"
                    }
                })
                .clicked()
            {
                output.push_action(PlaylistAction::GoCurrent(target));
            }
        } else {
            ui.add_enabled(false, egui::Button::new("Перейти к текущему"))
                .on_disabled_hover_text("Сейчас нет активного медиа");
        }
    });

    if model.url_editor_open {
        show_url_editor(ui, model, output);
    }
    show_operation_status(ui, model, output);
}

fn sort_menu(ui: &mut egui::Ui, model: &PlaylistInteractionModel, output: &mut PlaylistUiOutput) {
    let sort_enabled =
        model.structural_actions_enabled && model.item_count > 1 && model.progress.is_none();
    ui.add_enabled_ui(sort_enabled, |ui| {
        ui.menu_button("Сортировка", |ui| {
            for (key, label) in SORT_KEYS {
                ui.menu_button(label, |ui| {
                    if ui.button("По возрастанию ↑").clicked() {
                        output.push_action(PlaylistAction::Sort(SortCanonicalQueue::new(
                            key,
                            SortDirection::Ascending,
                        )));
                        ui.close();
                    }
                    if ui.button("По убыванию ↓").clicked() {
                        output.push_action(PlaylistAction::Sort(SortCanonicalQueue::new(
                            key,
                            SortDirection::Descending,
                        )));
                        ui.close();
                    }
                });
            }
        })
        .response
        .on_hover_text("Однократно изменить canonical порядок");
    });
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

fn show_operation_status(
    ui: &mut egui::Ui,
    model: &PlaylistInteractionModel,
    output: &mut PlaylistUiOutput,
) {
    if let Some(progress) = &model.progress {
        ui.horizontal_wrapped(|ui| {
            ui.label(progress_text(progress));
            if ui.button("Отмена").clicked() {
                output.push_action(PlaylistAction::CancelProgress(progress.cancel_scope));
            }
        });
    }
    if let Some(summary) = &model.completion_summary {
        ui.label(summary.as_ref());
    }
    if let Some(details) = &model.completion_details {
        ui.small(details.as_ref());
    }
    if let Some(feedback) = &model.safe_feedback {
        ui.colored_label(ui.visuals().warn_fg_color, feedback.as_ref());
    }
    if model.save_retry_available {
        ui.horizontal(|ui| {
            ui.colored_label(ui.visuals().warn_fg_color, "Не удалось сохранить плейлист");
            if ui.button("Повторить").clicked() {
                output.push_action(PlaylistAction::RetrySave);
            }
        });
    }
    if let Some(direction) = model.wait_direction {
        ui.label(wait_message(direction));
    }
    if model.navigation_cancel_available {
        let tooltip = navigation_cancel_tooltip(model.awaiting_failure_origin_ended);
        if ui
            .button("Отменить переход")
            .on_hover_text(tooltip)
            .clicked()
        {
            output.push_action(PlaylistAction::CancelNavigation);
        }
    }
}

fn progress_text(progress: &crate::playlist_runtime::PlaylistProgressModel) -> String {
    progress.total.map_or_else(
        || format!("{}: {}", progress.stage, progress.processed),
        |total| format!("{}: {} из {total}", progress.stage, progress.processed),
    )
}

const fn wait_message(direction: PlaylistWaitDirection) -> &'static str {
    match direction {
        PlaylistWaitDirection::Next => "Ищу следующий трек…",
        PlaylistWaitDirection::Previous => "Ищу предыдущий трек…",
    }
}

const fn navigation_cancel_tooltip(origin_already_ended: bool) -> &'static str {
    if origin_already_ended {
        "Отменить переход; завершившееся воспроизведение останется остановленным"
    } else {
        "Отменить только ожидающий переход"
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::playlist_runtime::{
        PlaylistProgressCancelScope, PlaylistProgressModel, PlaylistWaitDirection,
    };

    use super::{SORT_KEYS, navigation_cancel_tooltip, progress_text, wait_message};

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
    fn progress_and_direction_text_keep_exact_scopes_distinct() {
        let progress = PlaylistProgressModel {
            stage: Arc::from("Проверка файлов"),
            processed: 3,
            total: Some(7),
            cancel_scope: PlaylistProgressCancelScope::ManualAdd,
        };

        assert_eq!(progress_text(&progress), "Проверка файлов: 3 из 7");
        assert_eq!(
            wait_message(PlaylistWaitDirection::Next),
            "Ищу следующий трек…"
        );
        assert_eq!(
            wait_message(PlaylistWaitDirection::Previous),
            "Ищу предыдущий трек…"
        );
    }

    #[test]
    fn ended_origin_cancel_tooltip_promises_stop_not_resume() {
        assert!(navigation_cancel_tooltip(true).contains("останется остановленным"));
        assert!(!navigation_cancel_tooltip(false).contains("останется остановленным"));
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
        let production_source = include_str!("toolbar.rs")
            .split_once("#[cfg(test)]")
            .expect("toolbar tests must stay after production code")
            .0;

        assert!(!production_source.contains("SetRepeatMode"));
        assert!(!production_source.contains("SetShuffle"));
        assert!(!production_source.contains("Перемешать"));
        assert!(!production_source.contains("Повтор:"));
        assert!(production_source.contains("После текущего"));
        assert!(production_source.contains("Сортировка"));
    }
}
