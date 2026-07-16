//! Pure winit 0.30 hotkey classification.
//!
//! Logical media keys имеют приоритет; physical media code используется только fallback.
//! Поэтому один `KeyEvent` всегда возвращает не более одного typed action.

use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

use crate::ui::player_controls::TransportControlAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShellHotkeyAction {
    Close,
    Legacy(KeyCode),
    Transport(TransportControlAction),
}

pub(super) fn classify_key_event(
    event: &KeyEvent,
    egui_keyboard_captured: bool,
) -> Option<ShellHotkeyAction> {
    classify_key_parts(
        &event.logical_key,
        event.physical_key,
        event.state,
        event.repeat,
        egui_keyboard_captured,
    )
}

fn classify_key_parts(
    logical_key: &Key,
    physical_key: PhysicalKey,
    state: ElementState,
    _repeat: bool,
    egui_keyboard_captured: bool,
) -> Option<ShellHotkeyAction> {
    if state != ElementState::Pressed {
        return None;
    }
    let logical_media = match logical_key {
        Key::Named(NamedKey::MediaTrackPrevious) => Some(TransportControlAction::Previous),
        Key::Named(NamedKey::MediaTrackNext) => Some(TransportControlAction::Next),
        _ => None,
    };
    if let Some(action) = logical_media {
        return Some(ShellHotkeyAction::Transport(action));
    }
    let physical_code = match physical_key {
        PhysicalKey::Code(code) => code,
        PhysicalKey::Unidentified(_) => return None,
    };
    let physical_media = match physical_code {
        KeyCode::MediaTrackPrevious => Some(TransportControlAction::Previous),
        KeyCode::MediaTrackNext => Some(TransportControlAction::Next),
        _ => None,
    };
    if let Some(action) = physical_media {
        return Some(ShellHotkeyAction::Transport(action));
    }
    if egui_keyboard_captured {
        return None;
    }
    match physical_code {
        KeyCode::KeyP => Some(ShellHotkeyAction::Transport(
            TransportControlAction::Previous,
        )),
        KeyCode::KeyN => Some(ShellHotkeyAction::Transport(TransportControlAction::Next)),
        KeyCode::Space => Some(ShellHotkeyAction::Transport(
            TransportControlAction::TogglePlayback,
        )),
        KeyCode::Escape => Some(ShellHotkeyAction::Close),
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
        captured: bool,
    ) -> Option<ShellHotkeyAction> {
        classify_key_parts(&logical, physical, state, repeat, captured)
    }

    #[test]
    fn p_and_n_are_suppressed_by_egui_keyboard_capture() {
        assert_eq!(
            classified(
                Key::Character("p".into()),
                PhysicalKey::Code(KeyCode::KeyP),
                ElementState::Pressed,
                false,
                false,
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
                true,
            ),
            None
        );
        assert_eq!(
            classified(
                Key::Character("x".into()),
                PhysicalKey::Code(KeyCode::KeyX),
                ElementState::Pressed,
                false,
                false,
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
                true,
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
                true,
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
                false,
            ),
            None
        );
        assert_eq!(
            classified(
                Key::Named(NamedKey::MediaTrackNext),
                PhysicalKey::Code(KeyCode::MediaTrackNext),
                ElementState::Released,
                false,
                false,
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
                false,
            ),
            Some(ShellHotkeyAction::Transport(TransportControlAction::Next))
        );
    }
}
