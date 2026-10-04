//! Pure winit 0.30 hotkey classification.
//!
//! Logical media keys имеют приоритет; physical media code используется только fallback.
//! Поэтому один `KeyEvent` всегда возвращает не более одного typed action.
//!
//! Какие хоткеи пропускать, решает владелец клавиатуры
//! (`ui::keyboard_focus::KeyboardFocusOwner`), а не общий флаг «egui занят»:
//! - фокуса нет — работают все хоткеи;
//! - фокус у кнопки/строки плейлиста — работают только буквенные хоткеи,
//!   а пробел, стрелки и Esc остаются виджету (иначе, например, пробел
//!   нажал бы кнопку play и тут же второй раз переключил playback хоткеем);
//! - фокус у текстового поля — только media keys.

use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

use crate::ui::keyboard_focus::KeyboardFocusOwner;
use crate::ui::player_controls::TransportControlAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShellHotkeyAction {
    /// Esc: закрыть то, что открыто поверх плеера, или выйти из фуллскрина.
    ///
    /// Что именно закрывается, решает `escape_dismissal` по текущему состоянию
    /// приложения; приложение по Esc не закрывается (решение владельца, сессия 01).
    DismissTopmost,
    Legacy(KeyCode),
    Transport(TransportControlAction),
}

pub(super) fn classify_key_event(
    event: &KeyEvent,
    keyboard_owner: KeyboardFocusOwner,
) -> Option<ShellHotkeyAction> {
    classify_key_parts(
        &event.logical_key,
        event.physical_key,
        event.state,
        event.repeat,
        keyboard_owner,
    )
}

fn classify_key_parts(
    logical_key: &Key,
    physical_key: PhysicalKey,
    state: ElementState,
    _repeat: bool,
    keyboard_owner: KeyboardFocusOwner,
) -> Option<ShellHotkeyAction> {
    if state != ElementState::Pressed {
        return None;
    }
    // Media keys не конфликтуют ни с текстом, ни с виджетами — работают всегда.
    if let Some(action) = media_key_action(logical_key, physical_key) {
        return Some(ShellHotkeyAction::Transport(action));
    }
    let PhysicalKey::Code(physical_code) = physical_key else {
        return None;
    };
    let allowed_for_owner = match keyboard_owner {
        KeyboardFocusOwner::Nobody => true,
        KeyboardFocusOwner::Widget => hotkey_coexists_with_focused_widget(physical_code),
        KeyboardFocusOwner::TextInput => false,
    };
    if !allowed_for_owner {
        return None;
    }
    player_hotkey_action(physical_code)
}

/// Logical media key важнее physical; physical используется как fallback.
fn media_key_action(
    logical_key: &Key,
    physical_key: PhysicalKey,
) -> Option<TransportControlAction> {
    let logical_media = match logical_key {
        Key::Named(NamedKey::MediaTrackPrevious) => Some(TransportControlAction::Previous),
        Key::Named(NamedKey::MediaTrackNext) => Some(TransportControlAction::Next),
        _ => None,
    };
    if logical_media.is_some() {
        return logical_media;
    }
    match physical_key {
        PhysicalKey::Code(KeyCode::MediaTrackPrevious) => Some(TransportControlAction::Previous),
        PhysicalKey::Code(KeyCode::MediaTrackNext) => Some(TransportControlAction::Next),
        _ => None,
    }
}

/// Хоткеи, которые не используют сфокусированные виджеты приложения.
///
/// Кнопки egui нажимаются пробелом/Enter, egui двигает фокус стрелками и
/// снимает его по Esc; строки плейлиста используют стрелки, Home/End, пробел,
/// Enter, Delete и Esc. Буквы F/M/J/L/P/N никто из них не использует.
/// PageUp/PageDown намеренно не пропускаются: это клавиши прокрутки.
fn hotkey_coexists_with_focused_widget(physical_code: KeyCode) -> bool {
    matches!(
        physical_code,
        KeyCode::KeyF
            | KeyCode::KeyM
            | KeyCode::KeyJ
            | KeyCode::KeyL
            | KeyCode::KeyP
            | KeyCode::KeyN
    )
}

/// Полная таблица хоткеев плеера без учёта владельца клавиатуры.
fn player_hotkey_action(physical_code: KeyCode) -> Option<ShellHotkeyAction> {
    match physical_code {
        KeyCode::KeyP => Some(ShellHotkeyAction::Transport(
            TransportControlAction::Previous,
        )),
        KeyCode::KeyN => Some(ShellHotkeyAction::Transport(TransportControlAction::Next)),
        KeyCode::Space => Some(ShellHotkeyAction::Transport(
            TransportControlAction::TogglePlayback,
        )),
        KeyCode::Escape => Some(ShellHotkeyAction::DismissTopmost),
        other @ (KeyCode::KeyF
        | KeyCode::KeyM
        | KeyCode::ArrowLeft
        | KeyCode::KeyJ
        | KeyCode::ArrowRight
        | KeyCode::KeyL
        | KeyCode::PageUp
        | KeyCode::PageDown) => Some(ShellHotkeyAction::Legacy(other)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{NativeKey, NativeKeyCode};

    fn classified(
        logical: Key,
        physical: PhysicalKey,
        state: ElementState,
        repeat: bool,
        keyboard_owner: KeyboardFocusOwner,
    ) -> Option<ShellHotkeyAction> {
        classify_key_parts(&logical, physical, state, repeat, keyboard_owner)
    }

    #[test]
    fn p_and_n_are_suppressed_while_text_input_owns_keyboard() {
        assert_eq!(
            classified(
                Key::Character("p".into()),
                PhysicalKey::Code(KeyCode::KeyP),
                ElementState::Pressed,
                false,
                KeyboardFocusOwner::Nobody,
            ),
            Some(ShellHotkeyAction::Transport(
                TransportControlAction::Previous
            ))
        );
        assert_eq!(
            classified(
                Key::Character("n".into()),
                PhysicalKey::Code(KeyCode::KeyN),
                ElementState::Pressed,
                false,
                KeyboardFocusOwner::TextInput,
            ),
            None
        );
        assert_eq!(
            classified(
                Key::Character("x".into()),
                PhysicalKey::Code(KeyCode::KeyX),
                ElementState::Pressed,
                false,
                KeyboardFocusOwner::Nobody,
            ),
            None
        );
    }

    #[test]
    fn logical_media_wins_when_logical_and_physical_both_match() {
        assert_eq!(
            classified(
                Key::Named(NamedKey::MediaTrackPrevious),
                PhysicalKey::Code(KeyCode::MediaTrackNext),
                ElementState::Pressed,
                false,
                KeyboardFocusOwner::TextInput,
            ),
            Some(ShellHotkeyAction::Transport(
                TransportControlAction::Previous
            ))
        );
    }

    #[test]
    fn unidentified_logical_key_uses_one_physical_media_fallback() {
        assert_eq!(
            classified(
                Key::Unidentified(NativeKey::Unidentified),
                PhysicalKey::Code(KeyCode::MediaTrackNext),
                ElementState::Pressed,
                false,
                KeyboardFocusOwner::TextInput,
            ),
            Some(ShellHotkeyAction::Transport(TransportControlAction::Next))
        );
    }

    #[test]
    fn unrelated_and_released_events_do_not_create_transport_actions() {
        assert_eq!(
            classified(
                Key::Character("x".into()),
                PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                ElementState::Pressed,
                false,
                KeyboardFocusOwner::Nobody,
            ),
            None
        );
        assert_eq!(
            classified(
                Key::Named(NamedKey::MediaTrackNext),
                PhysicalKey::Code(KeyCode::MediaTrackNext),
                ElementState::Released,
                false,
                KeyboardFocusOwner::Nobody,
            ),
            None
        );
    }

    #[test]
    fn repeated_pressed_event_preserves_current_policy() {
        assert_eq!(
            classified(
                Key::Character("n".into()),
                PhysicalKey::Code(KeyCode::KeyN),
                ElementState::Pressed,
                true,
                KeyboardFocusOwner::Nobody,
            ),
            Some(ShellHotkeyAction::Transport(TransportControlAction::Next))
        );
    }

    // --- Владелец клавиатуры (UX edge cases, сессия 01) ---

    use crate::ui::keyboard_focus::keyboard_focus_owner;
    use crate::ui::player_controls::playback_button_test_harness::PlaybackButtonHarness;

    /// Нажатие обычной (не media) клавиши: логическая клавиша на решение не влияет.
    fn pressed(code: KeyCode, keyboard_owner: KeyboardFocusOwner) -> Option<ShellHotkeyAction> {
        classified(
            Key::Unidentified(NativeKey::Unidentified),
            PhysicalKey::Code(code),
            ElementState::Pressed,
            false,
            keyboard_owner,
        )
    }

    const LETTER_HOTKEYS: [KeyCode; 6] = [
        KeyCode::KeyF,
        KeyCode::KeyM,
        KeyCode::KeyJ,
        KeyCode::KeyL,
        KeyCode::KeyP,
        KeyCode::KeyN,
    ];

    /// Клавиши, которые сфокусированный виджет обрабатывает сам.
    const WIDGET_OWNED_KEYS: [KeyCode; 6] = [
        KeyCode::Space,
        KeyCode::ArrowLeft,
        KeyCode::ArrowRight,
        KeyCode::Escape,
        KeyCode::PageUp,
        KeyCode::PageDown,
    ];

    #[test]
    fn hotkeys_work_after_mouse_click_on_play_button() {
        let play_button = PlaybackButtonHarness::new();
        assert_eq!(
            play_button.click_with_mouse(),
            1,
            "клик переключает playback"
        );

        let keyboard_owner = keyboard_focus_owner(play_button.egui_ctx());

        assert_eq!(keyboard_owner, KeyboardFocusOwner::Nobody);
        for code in [
            KeyCode::KeyF,
            KeyCode::KeyM,
            KeyCode::KeyJ,
            KeyCode::KeyL,
            KeyCode::ArrowLeft,
            KeyCode::ArrowRight,
        ] {
            assert_eq!(
                pressed(code, keyboard_owner),
                Some(ShellHotkeyAction::Legacy(code)),
                "{code:?} после клика по play"
            );
        }
        assert_eq!(
            pressed(KeyCode::KeyN, keyboard_owner),
            Some(ShellHotkeyAction::Transport(TransportControlAction::Next))
        );
    }

    #[test]
    fn space_after_mouse_click_on_play_toggles_playback_exactly_once() {
        let play_button = PlaybackButtonHarness::new();
        let _click_toggles = play_button.click_with_mouse();

        // Порядок как в живом окне: shell классифицирует клавишу до кадра egui.
        let keyboard_owner = keyboard_focus_owner(play_button.egui_ctx());
        let hotkey_toggles = usize::from(
            pressed(KeyCode::Space, keyboard_owner)
                == Some(ShellHotkeyAction::Transport(
                    TransportControlAction::TogglePlayback,
                )),
        );
        let button_toggles = play_button.press_key(egui::Key::Space);

        assert_eq!(hotkey_toggles + button_toggles, 1);
    }

    #[test]
    fn space_on_tab_focused_play_button_toggles_playback_exactly_once() {
        let play_button = PlaybackButtonHarness::new();
        play_button.focus_with_tab();

        let keyboard_owner = keyboard_focus_owner(play_button.egui_ctx());
        assert_eq!(keyboard_owner, KeyboardFocusOwner::Widget);
        let hotkey_action = pressed(KeyCode::Space, keyboard_owner);
        let button_toggles = play_button.press_key(egui::Key::Space);

        // Пробел нажимает кнопку, хоткей молчит — ровно одно переключение.
        assert_eq!(hotkey_action, None);
        assert_eq!(button_toggles, 1);
        // Буквенные хоткеи при этом продолжают работать.
        assert_eq!(
            pressed(KeyCode::KeyF, keyboard_owner),
            Some(ShellHotkeyAction::Legacy(KeyCode::KeyF))
        );
    }

    #[test]
    fn focused_widget_keeps_its_keys_and_lets_letter_hotkeys_through() {
        for code in LETTER_HOTKEYS {
            assert!(
                pressed(code, KeyboardFocusOwner::Widget).is_some(),
                "{code:?} должен работать при фокусе на виджете"
            );
        }
        for code in WIDGET_OWNED_KEYS {
            assert_eq!(
                pressed(code, KeyboardFocusOwner::Widget),
                None,
                "{code:?} принадлежит сфокусированному виджету"
            );
        }
    }

    #[test]
    fn text_input_blocks_every_player_hotkey() {
        for code in LETTER_HOTKEYS.into_iter().chain(WIDGET_OWNED_KEYS) {
            assert_eq!(
                pressed(code, KeyboardFocusOwner::TextInput),
                None,
                "{code:?}"
            );
        }
    }

    #[test]
    fn without_focus_every_player_hotkey_is_available() {
        for code in LETTER_HOTKEYS.into_iter().chain(WIDGET_OWNED_KEYS) {
            assert!(
                pressed(code, KeyboardFocusOwner::Nobody).is_some(),
                "{code:?}"
            );
        }
    }

    #[test]
    fn escape_requests_dismissal_instead_of_closing_application() {
        assert_eq!(
            pressed(KeyCode::Escape, KeyboardFocusOwner::Nobody),
            Some(ShellHotkeyAction::DismissTopmost)
        );
    }

    #[test]
    fn media_keys_work_for_every_keyboard_owner() {
        for keyboard_owner in [
            KeyboardFocusOwner::Nobody,
            KeyboardFocusOwner::Widget,
            KeyboardFocusOwner::TextInput,
        ] {
            assert_eq!(
                classified(
                    Key::Named(NamedKey::MediaTrackNext),
                    PhysicalKey::Code(KeyCode::MediaTrackNext),
                    ElementState::Pressed,
                    false,
                    keyboard_owner,
                ),
                Some(ShellHotkeyAction::Transport(TransportControlAction::Next)),
                "{keyboard_owner:?}"
            );
        }
    }
}
