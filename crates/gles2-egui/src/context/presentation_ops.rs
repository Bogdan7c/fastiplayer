//! Операции показа [`Gles2Context`]: vsync на месте, отпуск и возврат окна.
//!
//! Контракт тот же, что у wgpu-рендера после сессии 02a (`RenderShell::reconfigure_presentation`,
//! `detach_presentation`, `reattach_presentation`): вызывать только между кадрами; пока окно
//! отпущено, кадры пропускаются ([`crate::FrameSkipReason::PresentationDetached`]).

use super::driver::SurfaceSize;
use super::egl_objects::ReattachWindowError;
use super::{Gles2Context, RenderTarget};
use crate::loss::{ContextHealth, ContextLoss};
use crate::presentation::{
    DetachOutcome, PresentationError, PresentationReconfigured, PresentationSettings,
    ReattachOutcome,
};

impl Gles2Context {
    /// Применяет настройки показа на месте (`eglSwapInterval`), без нового контекста.
    /// Те же настройки — [`PresentationReconfigured::AlreadyActive`], драйвер не трогаем.
    pub fn reconfigure_presentation(
        &mut self,
        settings: PresentationSettings,
    ) -> Result<PresentationReconfigured, PresentationError> {
        self.ensure_window_presentation()?;
        if self.presentation.detached {
            return Err(PresentationError::Detached);
        }
        if settings == self.presentation.settings {
            return Ok(PresentationReconfigured::AlreadyActive);
        }
        let applied = self
            .session
            .apply_vsync(settings.vsync)
            .map_err(|error| self.presentation_egl_error(error))?;
        let Some(outcome) = applied else {
            return Err(PresentationError::Detached);
        };
        self.presentation.settings = settings;
        self.presentation.swap_interval = Some(outcome);
        Ok(PresentationReconfigured::Applied(outcome))
    }

    /// Отпускает окно: EGLSurface разрушается, контекст, конфиг и GL-объекты остаются.
    /// Нужно, чтобы новый рендер мог создать свою surface на том же окне (решение
    /// владельца 22: на одном окне одна surface).
    ///
    /// Работает и у потерянного контекста: после сброса GPU окно обязано освободиться для
    /// нового рендера.
    pub fn detach_presentation(&mut self) -> Result<DetachOutcome, PresentationError> {
        if matches!(self.target, RenderTarget::Offscreen(_)) {
            return Err(PresentationError::NotAWindowTarget);
        }
        if self.health.get() != ContextHealth::Healthy {
            let outcome = self.session.detach_window_after_context_loss();
            self.presentation.detached = true;
            return Ok(outcome);
        }
        let outcome = self
            .session
            .detach_window()
            .map_err(|error| self.presentation_egl_error(error))?;
        self.presentation.detached = true;
        Ok(outcome)
    }

    /// Возвращает окно: новая EGLSurface на прежних display и конфиге, настройки vsync
    /// применяются заново (у новой surface интервал по умолчанию — 1).
    pub fn reattach_presentation(&mut self) -> Result<ReattachOutcome, PresentationError> {
        self.ensure_window_presentation()?;
        let size = self.extent.surface_size().unwrap_or(SurfaceSize::ONE_PIXEL);
        let outcome = self
            .session
            .reattach_window(size)
            .map_err(|error| self.presentation_reattach_error(error))?;
        self.presentation.detached = false;
        if outcome == ReattachOutcome::Reattached {
            let applied = self
                .session
                .apply_vsync(self.presentation.settings.vsync)
                .map_err(|error| self.presentation_egl_error(error))?;
            self.presentation.swap_interval = applied;
        }
        Ok(outcome)
    }

    fn ensure_window_presentation(&self) -> Result<(), PresentationError> {
        if let ContextHealth::Lost(loss) = self.health.get() {
            return Err(PresentationError::ContextLost(loss));
        }
        match self.target {
            RenderTarget::Window => Ok(()),
            RenderTarget::Offscreen(_) => Err(PresentationError::NotAWindowTarget),
        }
    }

    fn presentation_egl_error(&self, error: super::driver::EglCallError) -> PresentationError {
        if error.is_context_lost() {
            let loss = ContextLoss::from_egl();
            self.health.set(ContextHealth::Lost(loss));
            PresentationError::ContextLost(loss)
        } else {
            PresentationError::Driver(error)
        }
    }

    fn presentation_reattach_error(&self, error: ReattachWindowError) -> PresentationError {
        match error {
            ReattachWindowError::SurfaceCreation(error) if !error.is_context_lost() => {
                PresentationError::SurfaceCreation(error)
            }
            ReattachWindowError::SurfaceCreation(error)
            | ReattachWindowError::MakeCurrent(error) => self.presentation_egl_error(error),
        }
    }
}
