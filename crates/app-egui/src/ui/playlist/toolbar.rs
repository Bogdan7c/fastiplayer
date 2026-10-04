//! Toolbar и URL form renderer без I/O и business mutations.

mod export_menu;
mod icon_bar;
mod import_menu;

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
    use super::{PlaylistAction, PlaylistInteractionModel, PlaylistUiOutput, SORT_KEYS};
    use crate::ui::skin::{MinimalSkin, PlayerSkin};

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

    /// Рисует toolbar в headless egui и отдаёт действия и полный output кадра.
    /// `after_toolbar` вызывается в том же кадре после toolbar — так видно,
    /// какие клавиши toolbar «съел», а какие дошли бы до глобальных hotkeys.
    fn render_toolbar(
        context: &egui::Context,
        model: &PlaylistInteractionModel,
        events: Vec<egui::Event>,
        mut after_toolbar: impl FnMut(&mut egui::Ui),
    ) -> (Vec<PlaylistAction>, egui::FullOutput) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(420.0, 240.0),
            )),
            focused: true,
            events,
            ..egui::RawInput::default()
        };
        let mut output = PlaylistUiOutput::default();
        let full_output = crate::ui::test_frame::run_ui_frame(context, input, |ui| {
            ui.set_width(420.0);
            super::show(ui, model, MinimalSkin.playlist_toolbar_style(), &mut output);
            after_toolbar(ui);
        });
        (output.take_actions(), full_output)
    }

    /// Нажатие и отпускание одной клавиши без модификаторов.
    fn key_press(key: egui::Key) -> Vec<egui::Event> {
        [true, false]
            .into_iter()
            .map(|pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .collect()
    }

    /// Enter в URL-поле отправляет URL и поглощается: он не должен дойти
    /// до глобальных hotkeys (например, play/pause) в том же кадре.
    #[test]
    fn inline_url_enter_submits_and_is_consumed_after_focus_loss() {
        let context = egui::Context::default();
        let mut model = PlaylistInteractionModel {
            url_editor_open: true,
            url_text: "https://media.invalid/clip.mp4".to_owned(),
            url_request_focus: true,
            ..PlaylistInteractionModel::default()
        };
        let (focus_actions, _) = render_toolbar(&context, &model, Vec::new(), |_| {});
        assert!(focus_actions.contains(&PlaylistAction::UrlFocusRestored));
        model.url_request_focus = false;

        let mut enter_leaked_to_hotkeys = None;
        let (enter_actions, _) =
            render_toolbar(&context, &model, key_press(egui::Key::Enter), |ui| {
                enter_leaked_to_hotkeys =
                    Some(ui.input(|input| input.key_pressed(egui::Key::Enter)));
            });

        assert!(enter_actions.contains(&PlaylistAction::SubmitUrl));
        assert_eq!(enter_leaked_to_hotkeys, Some(false));
    }

    /// Toolbar плейлиста содержит ровно свои кнопки; режимы очереди (повтор,
    /// перемешивание, «после текущего») и Undo живут в других местах и сюда не дублируются.
    #[test]
    fn playlist_toolbar_exposes_only_its_own_controls() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let model = PlaylistInteractionModel {
            item_count: 3,
            ..PlaylistInteractionModel::default()
        };

        let (_, full_output) = render_toolbar(&context, &model, Vec::new(), |_| {});

        let update = full_output
            .platform_output
            .accesskit_update
            .expect("AccessKit tree update");
        let mut button_labels: Vec<String> = update
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == egui::accesskit::Role::Button)
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect();
        button_labels.sort();
        let mut expected_labels = vec![
            "Добавить файлы",
            "Добавить URL",
            "Сортировать плейлист",
            "Перейти к текущему медиа",
            "Импортировать плейлист",
            "Экспортировать плейлист",
            "Очистить очередь",
        ];
        expected_labels.sort_unstable();
        assert_eq!(button_labels, expected_labels);
    }
}
