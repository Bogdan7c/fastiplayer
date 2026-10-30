//! Исполнение хоткеев оболочки (пробел, стрелки, Esc, F, M, P/N, медиаклавиши) для
//! любого окна плеера.
//!
//! Классификатор клавиш — [`super::hotkeys`]; здесь — исполнители, общие для обычного
//! окна (`AppShell`) и окна браузерного режима (`browser_session::window`). Окно, где
//! транспорт ведёт страница, получает те же клавиши тем же путём: пробел и стрелки
//! уходят в единую точку транспорта `AppState` (`send_transport_command` /
//! `route_page_led_transport_action`), и там становятся намерениями зрителя, а не
//! командами плееру напрямую.

use render_shell_api::RenderShell;
use winit::event::KeyEvent;
use winit::window::Window;

use super::escape_dismissal;
use super::hotkeys::{self, ShellHotkeyAction};
use crate::playlist_runtime::PlaylistRuntime;
use crate::state::AppState;
use crate::ui::keyboard_focus::keyboard_focus_owner;

/// Что стало с клавишей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellKeyOutcome {
    /// Это хоткей оболочки: исполнен, окну нужен кадр.
    Handled,
    /// Не хоткей оболочки (или клавишу забрал виджет): решает egui-кадр.
    NotAHotkey,
}

/// Классифицирует клавишу и исполняет хоткей оболочки.
///
/// `egui_response.consumed` для клавиш здесь не используется: egui-winit ставит его по
/// `egui_wants_keyboard_input()`, то есть при фокусе на любой кнопке, и хоткеи глохли
/// бы после клика мышью. Какие клавиши отдать виджету, решает политика владельца
/// клавиатуры.
pub(crate) fn handle_shell_key_event(
    window: &Window,
    key_event: &KeyEvent,
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &mut dyn RenderShell,
) -> ShellKeyOutcome {
    let keyboard_owner = keyboard_focus_owner(&app_state.egui_ctx);
    let Some(action) = hotkeys::classify_key_event(key_event, keyboard_owner) else {
        return ShellKeyOutcome::NotAHotkey;
    };
    match action {
        ShellHotkeyAction::DismissTopmost => {
            let _dismissed =
                escape_dismissal::dismiss_topmost(window, app_state, playlist_runtime, renderer);
        }
        ShellHotkeyAction::Legacy(key_code) => {
            app_state.handle_hotkeys(window, key_code, false);
        }
        ShellHotkeyAction::Transport(action) => {
            let snapshot = app_state.refresh_player_snapshot();
            playlist_runtime.publish_desktop_snapshot(&snapshot);
            crate::transport_runtime::apply_transport_action(
                app_state,
                playlist_runtime,
                renderer,
                &snapshot,
                action,
            );
        }
    }
    window.request_redraw();
    ShellKeyOutcome::Handled
}
