//! Перевод исходов сборщика кадра `gles2-egui` в словарь контракта рендера.
//!
//! Таблица (из `results/03.md`–`04.md`):
//!
//! | сборщик кадра                                   | контракт                                   |
//! |--------------------------------------------------|--------------------------------------------|
//! | `Dropped(RetainedUiUnavailable)`                 | `Dropped(RetainedUiUnavailable)`           |
//! | `Dropped(UiTexturesOutOfSync / NotApplied)`      | `Dropped(UiTexturesOutOfSync)`             |
//! | `Skipped(PresentationDetached)`                  | `Dropped(PresentationDetached)`            |
//! | `Skipped(ZeroSize)` (окно свёрнуто)              | `Dropped(SurfaceOccluded)`                 |
//! | ошибка кадра (потеря контекста, отказ слоя)      | `Failed(VideoRender)` с текстом причины    |

use gles2_egui::{FrameSkipReason, UiFrameDropReason, UiFrameError};
use render_shell_api::{RenderFrameDropReason, RenderFrameFailure};

/// Кадр отброшен до рисования.
#[must_use]
pub(super) fn drop_reason_of(reason: UiFrameDropReason) -> RenderFrameDropReason {
    match reason {
        UiFrameDropReason::RetainedUiUnavailable(_) => RenderFrameDropReason::RetainedUiUnavailable,
        UiFrameDropReason::UiTexturesOutOfSync { .. } | UiFrameDropReason::UiTexturesNotApplied => {
            RenderFrameDropReason::UiTexturesOutOfSync
        }
    }
}

/// Кадра нет: окно отпущено или свёрнуто.
#[must_use]
pub(super) const fn skip_reason_of(reason: FrameSkipReason) -> RenderFrameDropReason {
    match reason {
        FrameSkipReason::PresentationDetached => RenderFrameDropReason::PresentationDetached,
        FrameSkipReason::ZeroSize => RenderFrameDropReason::SurfaceOccluded,
    }
}

/// Ошибка кадра. Потеря контекста тоже `Failed`: приложение превращает его в «устройство
/// потеряно» и откатывается (так же, как отказ рендера wgpu).
#[must_use]
pub(super) fn failure_of(error: &UiFrameError) -> RenderFrameFailure {
    RenderFrameFailure::new(format!("GLES2 frame failed: {error}"))
}

#[cfg(test)]
mod tests;
