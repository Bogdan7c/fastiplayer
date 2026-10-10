//! Глобальные клавиши плеера, которые обрабатываются внутри кадра egui.
//!
//! UX edge cases 2, B3. Сюда входят клавиши, которым нужен `egui::InputState`
//! (учёт модификаторов, раскладочно-независимые `+`/`=`):
//! - `+` / `=` / `-` — шаг скорости, `0` — сброс скорости;
//! - `↑` / `↓` — громкость;
//! - `Ctrl+O` — открыть файл (тот же `ControlAction::OpenFile`, что и у кнопки Open).
//!
//! Кто может получить клавишу, решает владелец клавиатуры
//! (`ui::keyboard_focus::KeyboardFocusOwner`), а не отдельная логика:
//! - `Nobody` — доступно всё;
//! - `Widget` (кнопка, строка плейлиста): стрелки принадлежат виджету
//!   (навигация по плейлисту, перемещение фокуса), поэтому громкость по ним не
//!   меняется; скорость и `Ctrl+O` виджеты не используют и остаются доступными;
//! - `TextInput` — не срабатывает ничего: `-`, `+`, `0`, стрелки это ввод текста.
//!
//! Модуль только превращает ввод в намерения ([`ControlAction`]); команды в
//! player отправляет `AppState::handle_control_actions`.

use egui::{InputState, Key, Modifiers};

use crate::ui::keyboard_focus::{KeyboardFocusOwner, keyboard_focus_owner};
use crate::ui::player_controls::ControlAction;

/// Собирает глобальные действия клавиатуры за текущий кадр.
///
/// Вызывать после отрисовки всех панелей кадра: сфокусированная строка плейлиста
/// забирает свои стрелки раньше (и без них у нас всё равно владелец `Widget`).
#[must_use]
pub(crate) fn collect_actions(egui_ctx: &egui::Context) -> Vec<ControlAction> {
    let keyboard_owner = keyboard_focus_owner(egui_ctx);
    egui_ctx.input_mut(|input| actions_for_owner(input, keyboard_owner))
}

/// Чистая часть: ввод кадра + владелец клавиатуры → намерения.
fn actions_for_owner(
    input: &mut InputState,
    keyboard_owner: KeyboardFocusOwner,
) -> Vec<ControlAction> {
    let mut actions = Vec::new();
    if keyboard_owner == KeyboardFocusOwner::TextInput {
        return actions;
    }

    // Ctrl+O раньше остальных: egui сопоставляет модификаторы «логически»
    // (лишние Shift/Alt игнорируются), поэтому более конкретные сочетания идут первыми.
    if input.consume_key(Modifiers::COMMAND, Key::O) {
        actions.push(ControlAction::OpenFile);
    }

    // Скорость: `+` и `=` — шаг вверх (на обычной клавиатуре `+` это Shift+`=`).
    if input.consume_key(Modifiers::NONE, Key::Num0) {
        actions.push(ControlAction::ResetPlaybackRate);
    }
    let rate_steps = i32::try_from(
        input.count_and_consume_key(Modifiers::NONE, Key::Plus)
            + input.count_and_consume_key(Modifiers::NONE, Key::Equals),
    )
    .unwrap_or(i32::MAX)
        - i32::try_from(input.count_and_consume_key(Modifiers::NONE, Key::Minus))
            .unwrap_or(i32::MAX);
    if rate_steps != 0 {
        actions.push(ControlAction::AdjustPlaybackRateSteps(rate_steps));
    }

    // Громкость по стрелкам: только когда клавиатура свободна. При фокусе на
    // виджете стрелки принадлежат ему (навигация по плейлисту и т. п.).
    if keyboard_owner == KeyboardFocusOwner::Nobody {
        let volume_steps =
            i32::try_from(input.count_and_consume_key(Modifiers::NONE, Key::ArrowUp))
                .unwrap_or(i32::MAX)
                - i32::try_from(input.count_and_consume_key(Modifiers::NONE, Key::ArrowDown))
                    .unwrap_or(i32::MAX);
        if volume_steps != 0 {
            actions.push(ControlAction::AdjustVolumeSteps(volume_steps));
        }
    }
    actions
}

#[cfg(test)]
mod tests;
