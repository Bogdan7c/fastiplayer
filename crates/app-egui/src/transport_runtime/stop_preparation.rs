//! Stop отменяет подготовку источника до coordinator-request-а (UX edge cases 2, D3).
//!
//! Stop-intent адресован player-у и controller-у плейлиста, но подготовку, у которой ещё нет
//! coordinator-request-а, ведут другие владельцы: `StartupMediaController` (CLI-ссылка,
//! restore) и `AppState` (кнопка Open, drag & drop, forwarding, «Повторить» для файла).
//! Эта граница лишь спрашивает у каждого владельца intent-метод отмены; состояние и токены
//! остаются у владельцев. Подготовка, у которой coordinator-request уже есть (элемент
//! очереди, установка из Open), отменяется прежним guard-ом (`neutral_stop` → release).

use crate::media_open::{PreparationCancelCause, PreparationCancelOutcome};
use crate::startup_media::StartupMediaController;
use crate::state::AppState;

/// Отменяет все pending-подготовки, у которых нет coordinator-request-а.
///
/// Тихая отмена (семантика C5): ошибок и уведомлений нет, очередь не перебирается, элемент
/// не помечается битым. Возвращает итог по всем владельцам.
pub(crate) fn cancel_pending_preparations_for_stop(
    app_state: &mut AppState,
    startup_media: &mut StartupMediaController,
) -> PreparationCancelOutcome {
    let startup = startup_media.cancel_pending_preparation(PreparationCancelCause::UserStop);
    if startup.cancelled_anything() {
        // Строка «Подготовка…» принадлежит AppState; владелец job-ов её сам не видит.
        app_state.withdraw_cancelled_startup_preparation();
    }
    let local = app_state.cancel_pending_local_file_preparation(PreparationCancelCause::UserStop);
    startup.or(local)
}
