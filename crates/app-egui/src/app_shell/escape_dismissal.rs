//! Семантика Esc на уровне окна: «закрыть то, что сверху».
//!
//! Решение владельца (UX edge cases, сессия 01, вариант A): Esc по приоритету
//! 1) закрывает открытое меню/popup egui;
//! 2) отменяет подтверждение замены очереди;
//! 3) отменяет preview импорта плейлиста;
//! 4) выходит из полноэкранного режима;
//! 5) иначе ничего не делает. Приложение по Esc **не** закрывается.
//!
//! Настройки по Esc намеренно не закрываются: закрытие настроек отменяет
//! несохранённые правки, и одно случайное нажатие не должно их стирать.
//!
//! Модуль разделён на три шага, чтобы каждый был проверяем отдельно:
//! сбор фактов (`EscapeContext::collect`) → чистое решение
//! (`resolve_escape_target`) → исполнение теми же путями, что и кнопки
//! «Отмена» (`dismiss_topmost`).

use render_wgpu_shell::Renderer;
use tracing::debug;
use winit::window::Window;

use crate::playlist_runtime::{
    PlaylistConfirmationAction, PlaylistImportPreviewId, PlaylistRuntime,
    QueueReplacementConfirmationDecision,
};
use crate::state::AppState;
use crate::ui::playlist::PlaylistAction;

/// Открыто ли сейчас меню/popup egui (ComboBox, контекстное меню, меню тулбара).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EguiPopupState {
    Open,
    Closed,
}

/// Режим окна, от которого зависит, есть ли из чего «выходить» по Esc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WindowPresentation {
    Fullscreen,
    Windowed,
}

/// Снимок фактов, по которым выбирается действие Esc.
///
/// Снимок только читает владельцев состояния; изменение происходит позже,
/// в `dismiss_topmost`, через их собственные методы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EscapeContext {
    pub(super) egui_popup: EguiPopupState,
    /// Готовое действие «Отмена» для открытого подтверждения, если оно есть.
    pub(super) playlist_confirmation_cancel: Option<PlaylistConfirmationAction>,
    /// Id открытого preview импорта, если оно есть.
    pub(super) playlist_import_preview: Option<PlaylistImportPreviewId>,
    pub(super) window_presentation: WindowPresentation,
}

impl EscapeContext {
    /// Собирает факты у их владельцев: память egui, playlist runtime и окно.
    pub(super) fn collect(
        egui_ctx: &egui::Context,
        playlist_runtime: &PlaylistRuntime,
        window_presentation: WindowPresentation,
    ) -> Self {
        // Память egui хранит открытые popup-ы между кадрами, поэтому её можно
        // читать на winit-событии, до прогона следующего кадра.
        let egui_popup = if egui::Popup::is_any_open(egui_ctx) {
            EguiPopupState::Open
        } else {
            EguiPopupState::Closed
        };
        let playlist_confirmation_cancel =
            playlist_runtime
                .pending_playlist_confirmation()
                .map(|confirmation| PlaylistConfirmationAction {
                    intent_id: confirmation.intent_id(),
                    decision: QueueReplacementConfirmationDecision::Cancel,
                });
        let playlist_import_preview = playlist_runtime
            .pending_playlist_import_preview()
            .map(|preview| preview.preview_id());
        Self {
            egui_popup,
            playlist_confirmation_cancel,
            playlist_import_preview,
            window_presentation,
        }
    }
}

/// Что именно закрывает текущее нажатие Esc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EscapeTarget {
    /// Popup закроет сам egui в ближайшем кадре (он видит тот же Esc).
    EguiPopup,
    /// Отменить подтверждение замены очереди (как кнопка «Отмена»).
    PlaylistConfirmation(PlaylistConfirmationAction),
    /// Отменить preview импорта (как кнопка «Отмена» в preview).
    PlaylistImportPreview(PlaylistImportPreviewId),
    /// Выйти из полноэкранного режима.
    Fullscreen,
    /// Закрывать нечего: Esc ничего не делает.
    Nothing,
}

/// Чистое решение по приоритету «сверху вниз» без побочных эффектов.
///
/// Порядок подтверждение → preview совпадает с порядком отрисовки
/// центрального overlay: подтверждение рисуется поверх preview.
pub(super) fn resolve_escape_target(context: &EscapeContext) -> EscapeTarget {
    if context.egui_popup == EguiPopupState::Open {
        return EscapeTarget::EguiPopup;
    }
    if let Some(cancel_action) = context.playlist_confirmation_cancel {
        return EscapeTarget::PlaylistConfirmation(cancel_action);
    }
    if let Some(preview_id) = context.playlist_import_preview {
        return EscapeTarget::PlaylistImportPreview(preview_id);
    }
    match context.window_presentation {
        WindowPresentation::Fullscreen => EscapeTarget::Fullscreen,
        WindowPresentation::Windowed => EscapeTarget::Nothing,
    }
}

/// Определяет режим окна; источник истины — само окно winit.
pub(super) fn window_presentation(window: &Window) -> WindowPresentation {
    if window.fullscreen().is_some() {
        WindowPresentation::Fullscreen
    } else {
        WindowPresentation::Windowed
    }
}

/// Выполняет действие Esc и возвращает, что было закрыто.
pub(super) fn dismiss_topmost(
    window: &Window,
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
) -> EscapeTarget {
    let context = EscapeContext::collect(
        &app_state.egui_ctx,
        playlist_runtime,
        window_presentation(window),
    );
    let target = resolve_escape_target(&context);
    match target {
        // egui сам закроет popup: событие Esc уже в его очереди ввода.
        EscapeTarget::EguiPopup | EscapeTarget::Nothing => {}
        EscapeTarget::PlaylistConfirmation(cancel_action) => {
            // Тот же путь, что у кнопки «Отмена» в диалоге подтверждения.
            app_state.apply_playlist_confirmation_action(cancel_action, playlist_runtime, renderer);
        }
        EscapeTarget::PlaylistImportPreview(preview_id) => {
            // Тот же путь, что у кнопки «Отмена» в preview импорта. Флаг
            // «видимое изменение» не нужен: event loop перерисует окно после
            // любого хоткея.
            let _visible_change = crate::playlist_action_runtime::apply_playlist_actions(
                window,
                app_state,
                playlist_runtime,
                renderer,
                vec![PlaylistAction::CancelImport(preview_id)],
            );
        }
        EscapeTarget::Fullscreen => {
            // Ровно ветка выхода из `AppState::toggle_fullscreen`: окно уже
            // в фуллскрине, поэтому выходим без переключения «туда-обратно».
            window.set_fullscreen(None);
        }
    }
    debug!(?target, "Esc обработан");
    target
}

#[cfg(test)]
mod tests;
