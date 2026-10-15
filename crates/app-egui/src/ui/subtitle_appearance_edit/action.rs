//! Намерения режима правки, которые UI отдаёт наружу.

use crate::state::{AppearanceEdit, AppearanceEditFinish};

/// Что хочет пользователь; применяет `AppState` после прохода egui.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SubtitleAppearanceEditAction {
    /// Изменить черновик (жест на видео или элемент панели).
    Edit(AppearanceEdit),
    /// Пользователь схватил рамку: перенос начинается от места, где слой сейчас виден.
    StartMove,
    /// «Reset»: черновик становится стилем по умолчанию, режим остаётся открытым.
    Reset,
    /// «Done» (сохранить) или «Cancel» (вернуть прежний стиль): режим закрывается.
    Finish(AppearanceEditFinish),
}
