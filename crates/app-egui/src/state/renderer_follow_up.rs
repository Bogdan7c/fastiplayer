//! Следующий кадр по просьбе рендера: рендер досчитывает картинку сам, приложение спит.
//!
//! Рендер по изменению (`mem:app-egui/render-on-change-s04-2026-10-20`) не перерисовывает
//! окно без причины. Но у рендера бывает фоновая работа, результат которой виден только на
//! следующем кадре (GLES2 строит таблицу HDR → SDR в фоне, решение владельца 38). Рендер
//! говорит об этом через `RenderShell::needs_follow_up_frame`, а здесь решение превращается в
//! обычный follow-up redraw — тот же механизм, что у отказа повторить интерфейс.

use render_shell_api::FollowUpFrame;

use super::AppState;

/// Что приложение сделало с просьбой рендера о следующем кадре.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RendererFollowUpRequest {
    /// Следующий redraw запрошен.
    Requested,
    /// Рендеру кадр не нужен: цикл событий не будится.
    NotRequested,
}

impl RendererFollowUpRequest {
    /// Просьба рендера → решение приложения. Чистая функция: единственное правило проверяется
    /// отдельно от `AppState`.
    #[must_use]
    pub(crate) const fn for_renderer_answer(answer: FollowUpFrame) -> Self {
        match answer {
            FollowUpFrame::Soon => Self::Requested,
            FollowUpFrame::NotNeeded => Self::NotRequested,
        }
    }
}

impl AppState {
    /// Запрашивает следующий redraw, если рендер просит ещё один кадр.
    ///
    /// Вызывать после показанного кадра (`render_frame` → `Presented`): у потерянной surface
    /// или свёрнутого окна кадр не показывается, и просьба крутила бы redraw вхолостую.
    /// При `NotNeeded` ничего не меняется: пустых пробуждений нет.
    pub(crate) fn request_follow_up_redraw_for_renderer(
        &mut self,
        renderer_answer: FollowUpFrame,
    ) -> RendererFollowUpRequest {
        let request = RendererFollowUpRequest::for_renderer_answer(renderer_answer);
        if request == RendererFollowUpRequest::Requested {
            self.mark_pending_worker_redraw();
        }
        request
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// «Нужен» → redraw запрошен; «не нужен» → нет (иначе приложение крутилось бы вхолостую).
    #[test]
    fn only_a_soon_answer_requests_a_redraw() {
        assert_eq!(
            RendererFollowUpRequest::for_renderer_answer(FollowUpFrame::Soon),
            RendererFollowUpRequest::Requested
        );
        assert_eq!(
            RendererFollowUpRequest::for_renderer_answer(FollowUpFrame::NotNeeded),
            RendererFollowUpRequest::NotRequested
        );
    }
}
