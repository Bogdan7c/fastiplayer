//! Политика «кому сейчас принадлежит клавиатура» для shell-хоткеев.
//!
//! egui 0.36 считает клавиатуру занятой (`Context::egui_wants_keyboard_input`),
//! как только фокус есть у **любого** виджета — даже у обычной кнопки. Если
//! напрямую использовать этот сигнал, хоткеи плеера глохнут после клика по
//! кнопке или строке плейлиста. Поэтому здесь различаются три случая, и
//! вызывающий код (`app_shell::hotkeys`) решает, какие клавиши пропустить.
//!
//! Модуль только читает память egui и ничего не меняет.

/// Кто владеет клавиатурой на момент прихода события клавиши.
///
/// Значение вычисляется по памяти egui после последнего завершённого кадра:
/// winit доставляет клавишу до того, как egui прогонит следующий кадр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyboardFocusOwner {
    /// Фокуса нет: все хоткеи плеера доступны.
    Nobody,
    /// Фокус у нетекстового виджета (кнопка, строка плейлиста).
    ///
    /// Такой виджет сам обрабатывает пробел, Enter, стрелки и Esc
    /// (нажатие кнопки, навигация, снятие фокуса), поэтому хоткеи на эти
    /// клавиши не должны срабатывать второй раз.
    Widget,
    /// Фокус у поля ввода текста: любые буквы и пробел — это ввод текста.
    TextInput,
}

/// Определяет владельца клавиатуры по памяти egui.
pub(crate) fn keyboard_focus_owner(egui_ctx: &egui::Context) -> KeyboardFocusOwner {
    // `text_edit_focused` проверяет, что у сфокусированного id есть состояние
    // TextEdit, — это единственный надёжный признак текстового ввода в egui.
    if egui_ctx.text_edit_focused() {
        return KeyboardFocusOwner::TextInput;
    }
    let any_widget_focused = egui_ctx.memory(|memory| memory.focused().is_some());
    if any_widget_focused {
        KeyboardFocusOwner::Widget
    } else {
        KeyboardFocusOwner::Nobody
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_frame::{app_behavior_context, run_ui_frame};

    fn screen_input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            events,
            ..Default::default()
        }
    }

    #[test]
    fn nobody_owns_keyboard_without_focus() {
        let egui_ctx = app_behavior_context();
        let _ = run_ui_frame(&egui_ctx, screen_input(Vec::new()), |ui| {
            let _ = ui.button("Кнопка без фокуса");
        });

        assert_eq!(keyboard_focus_owner(&egui_ctx), KeyboardFocusOwner::Nobody);
    }

    #[test]
    fn focused_button_is_widget_owner_not_text_input() {
        let egui_ctx = app_behavior_context();
        // Первый кадр запрашивает фокус, второй применяет его, как в живом приложении.
        for _ in 0..2 {
            let _ = run_ui_frame(&egui_ctx, screen_input(Vec::new()), |ui| {
                ui.button("Кнопка").request_focus();
            });
        }

        assert_eq!(keyboard_focus_owner(&egui_ctx), KeyboardFocusOwner::Widget);
    }

    #[test]
    fn focused_text_edit_is_text_input_owner() {
        let egui_ctx = app_behavior_context();
        let mut url_text = String::new();
        for _ in 0..2 {
            let _ = run_ui_frame(&egui_ctx, screen_input(Vec::new()), |ui| {
                ui.text_edit_singleline(&mut url_text).request_focus();
            });
        }

        assert_eq!(
            keyboard_focus_owner(&egui_ctx),
            KeyboardFocusOwner::TextInput
        );
    }

    #[test]
    fn focused_text_edit_receives_typed_letter_and_space() {
        // Функциональная проверка: при владельце TextInput текст действительно печатается.
        let egui_ctx = app_behavior_context();
        let mut url_text = String::new();
        for _ in 0..2 {
            let _ = run_ui_frame(&egui_ctx, screen_input(Vec::new()), |ui| {
                ui.text_edit_singleline(&mut url_text).request_focus();
            });
        }
        let typed = vec![
            egui::Event::Text("f".to_owned()),
            egui::Event::Text(" ".to_owned()),
        ];
        let _ = run_ui_frame(&egui_ctx, screen_input(typed), |ui| {
            ui.text_edit_singleline(&mut url_text);
        });

        assert_eq!(url_text, "f ");
        assert_eq!(
            keyboard_focus_owner(&egui_ctx),
            KeyboardFocusOwner::TextInput
        );
    }
}
