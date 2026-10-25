//! Контекст OpenGL ES 2.0 на окне или без окна: [`Gles2Context`].
//!
//! Модули:
//! - `driver` — контракт «что мы просим у EGL»;
//! - `safety` — единственное место `unsafe` (libEGL, libwayland-egl, GL);
//! - `display_registry` — один `EGLDisplay` на процесс и подсчёт пользователей;
//! - `egl_objects` — RAII surface/context/display и порядок разрушения;
//! - `config_select`, `platform`, `gles_version` — чистые правила без драйвера;
//! - `creation` — сборка контекста и возможностей драйвера;
//! - `presentation_ops` — vsync, отпуск и возврат окна.
//!
//! Поток: контекст EGL текущий в одном потоке, поэтому [`Gles2Context`] не `Send`. Ждать
//! [`crate::GpuFence`] можно из любого потока.

mod config_select;
mod creation;
pub(crate) mod display_registry;
pub(crate) mod dma_buf_texture;
pub(crate) mod driver;
mod egl_objects;
pub(crate) mod gles_version;
pub(crate) mod platform;
mod presentation_ops;
#[allow(
    unsafe_code,
    reason = "единственный модуль FFI EGL/GL/libwayland-egl; каждый блок с // SAFETY:"
)]
pub(crate) mod safety;

#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

use self::driver::{EglCallError, SurfaceSize};
use self::egl_objects::EglSession;
use self::safety::GraphicsResetStatusQuery;
use self::safety::egl_image::ImageTargetTexture;
use self::safety::gl::CurrentGl;
use crate::capabilities::Gles2Capabilities;
use crate::error::Gles2Error;
use crate::fence::{FenceIssuer, FenceStats, FenceStrategy, GpuFence};
use crate::frame::{ActiveFrame, FrameSkipReason, FrameStart};
use crate::loss::{ContextHealth, ContextLoss};
use crate::offscreen::{OffscreenPixels, OffscreenTarget};
use crate::presentation::PresentationState;
use crate::shader::{Es100ProgramSources, ShaderError};

/// Окно, к которому привязывается контекст: любое, что отдаёт `raw-window-handle` 0.6
/// (окно winit и т.п.). `Arc` держит окно живым дольше EGLSurface поверх него.
pub trait WindowSource: HasWindowHandle + HasDisplayHandle + Send + Sync {}

impl<T> WindowSource for T where T: HasWindowHandle + HasDisplayHandle + Send + Sync + ?Sized {}

/// Размер окна в физических пикселях. В отличие от [`SurfaceSize`] может быть 0×0
/// (свёрнутое окно).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PixelExtent {
    /// Ширина.
    pub width: u32,
    /// Высота.
    pub height: u32,
}

impl PixelExtent {
    /// Размер цели или `None` для 0×0.
    pub fn surface_size(self) -> Option<SurfaceSize> {
        SurfaceSize::new(self.width, self.height)
    }
}

/// Почему GL сейчас недоступен вне кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GlAccessError {
    /// Контекст потерян.
    #[error("GLES2 context lost: {0}")]
    ContextLost(ContextLoss),
    /// Окно отпущено, а драйвер не умеет контекст без surface
    /// (`EGL_KHR_surfaceless_context`): сделать контекст текущим не на чем.
    #[error("presentation is detached and the driver has no surfaceless contexts")]
    DetachedWithoutSurfacelessContext,
    /// `eglMakeCurrent` отказал.
    #[error("eglMakeCurrent failed: {0}")]
    MakeCurrent(EglCallError),
}

/// Почему не удалось прочитать пиксели.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadbackError {
    /// Чтение есть только у offscreen-цели.
    #[error("pixel readback is only available for offscreen targets")]
    NotOffscreen,
    /// GL недоступен.
    #[error(transparent)]
    GlUnavailable(#[from] GlAccessError),
}

/// Куда рисует контекст.
enum RenderTarget {
    /// Окно; `Arc` окна живёт в `Gles2Context::window`.
    Window,
    /// FBO без окна. `Option` — чтобы `Drop` контекста забрал цель и удалил её GL-объекты.
    Offscreen(Option<OffscreenTarget>),
}

/// Контекст OpenGL ES 2.0 с целью показа.
///
/// Порядок разрушения: в `Drop::drop` удаляются GL-объекты (контекст ещё жив), затем поля по
/// порядку объявления — `session` (surface → context → display), и только потом `window`:
/// окно переживает EGLSurface поверх него.
pub struct Gles2Context {
    id: ContextId,
    gl: glow::Context,
    reset_status: Option<GraphicsResetStatusQuery>,
    /// `glEGLImageTargetTexture2DOES` (`None` — расширения нет или драйвер не дал адрес).
    image_target_texture: Option<ImageTargetTexture>,
    /// Счётчики импорта DMA-BUF (см. `dma_buf_texture`).
    dma_buf_stats: Cell<dma_buf_texture::DmaBufTextureStats>,
    capabilities: Gles2Capabilities,
    fences: FenceIssuer,
    presentation: PresentationState,
    health: Cell<ContextHealth>,
    extent: PixelExtent,
    target: RenderTarget,
    session: EglSession,
    /// Окно только удерживается живым дольше `session` (поверх него EGLSurface), поэтому
    /// не читается.
    _window_keepalive: Option<Arc<dyn WindowSource>>,
    /// Контекст EGL привязан к потоку: тип не `Send` и не `Sync`.
    _thread_bound: PhantomData<*const ()>,
}

/// Идентичность контекста в процессе. GL-объекты (текстуры, буферы, программы) живут в своём
/// контексте: имя текстуры одного контекста в другом означает «ничего» или чужой объект.
/// Владельцы GL-объектов (художник egui, маска углов) запоминают идентичность и отказывают
/// чужому контексту типизированной ошибкой, а не рисуют чужими именами.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContextId(u64);

impl ContextId {
    /// Новая уникальная идентичность (монотонный счётчик процесса; переполнение `u64` на
    /// практике недостижимо).
    fn next() -> Self {
        static NEXT_CONTEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self(NEXT_CONTEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

impl Gles2Context {
    /// Идентичность этого контекста (см. [`ContextId`]).
    pub fn id(&self) -> ContextId {
        self.id
    }

    /// Что умеет драйвер.
    pub fn capabilities(&self) -> &Gles2Capabilities {
        &self.capabilities
    }

    /// Состояние показа: настройки, прозрачность, интервал, отпущено ли окно.
    pub fn presentation(&self) -> PresentationState {
        self.presentation
    }

    /// Текущий размер цели (может быть 0×0 у свёрнутого окна).
    pub fn extent(&self) -> PixelExtent {
        self.extent
    }

    /// Стратегия «GPU закончил».
    pub fn fence_strategy(&self) -> FenceStrategy {
        self.fences.strategy()
    }

    /// Счётчики fence.
    pub fn fence_stats(&self) -> FenceStats {
        self.fences.stats()
    }

    /// Потерян ли контекст.
    pub fn context_loss(&self) -> Option<ContextLoss> {
        match self.health.get() {
            ContextHealth::Healthy => None,
            ContextHealth::Lost(loss) => Some(loss),
        }
    }

    /// Таблица функций GL вне кадра (загрузка текстур и т.п.). Делает контекст текущим в
    /// этом потоке.
    pub fn gl(&self) -> Result<&glow::Context, GlAccessError> {
        self.current_gl().map(|_| &self.gl)
    }

    /// Начинает кадр: контекст текущий, цель (окно или FBO) привязана, viewport на весь размер.
    pub fn begin_frame(&mut self) -> Result<FrameStart<'_>, Gles2Error> {
        self.health.get().ensure_healthy()?;
        if self.presentation.detached {
            return Ok(FrameStart::Skipped(FrameSkipReason::PresentationDetached));
        }
        let Some(size) = self.extent.surface_size() else {
            return Ok(FrameStart::Skipped(FrameSkipReason::ZeroSize));
        };
        let acquire_started_at = Instant::now();
        let gl = self.current_gl().map_err(Gles2Error::from_gl_access)?;
        let framebuffer = match &self.target {
            RenderTarget::Window => None,
            RenderTarget::Offscreen(target) => target.as_ref().map(OffscreenTarget::framebuffer),
        };
        gl.bind_framebuffer(framebuffer);
        gl.viewport(size);
        Ok(FrameStart::Ready(ActiveFrame::new(
            self,
            size,
            acquire_started_at,
        )))
    }

    /// Новый размер цели. 0×0 запоминается: кадры пропускаются, пока окно не развернут.
    pub fn resize(&mut self, extent: PixelExtent) -> Result<(), Gles2Error> {
        self.health.get().ensure_healthy()?;
        self.extent = extent;
        let Some(size) = extent.surface_size() else {
            return Ok(());
        };
        if matches!(self.target, RenderTarget::Window) {
            self.session.resize_window(size);
            return Ok(());
        }
        self.ensure_current_for_mutation()?;
        // Поля `gl` и `target` заимствуются раздельно: обёртка GL не держит весь `self`.
        let gl = CurrentGl::after_make_current(&self.gl);
        if let RenderTarget::Offscreen(Some(target)) = &mut self.target {
            target.resize(gl, size);
        }
        Ok(())
    }

    /// Ставит fence после уже отправленных команд. Без `EGL_KHR_fence_sync` — `glFinish` и
    /// счётчик [`FenceStats::fence_fallback_finishes`].
    pub fn insert_fence(&mut self) -> Result<GpuFence, Gles2Error> {
        self.ensure_current_for_mutation()?;
        let gl = CurrentGl::after_make_current(&self.gl);
        let issued = self.fences.issue(self.session.lease(), &gl);
        issued.map_err(|error| self.observe_egl_error(error))
    }

    /// Синхронно дожидается GPU (`glFinish`): запасной путь, когда fence поставить не удалось, а
    /// отправленные команды читают чужой ресурс (кадр декодера). Дорого: CPU ждёт GPU.
    ///
    /// # Errors
    /// Контекст потерян или не может стать текущим.
    pub fn finish_submitted_commands(&mut self) -> Result<(), Gles2Error> {
        self.ensure_current_for_mutation()?;
        let gl = CurrentGl::after_make_current(&self.gl);
        crate::fence::GpuCommandSubmission::finish(&gl);
        Ok(())
    }

    /// Опрос сброса GPU (`glGetGraphicsResetStatus*`). Вызывается и сам — после каждого показа.
    pub fn check_context_health(&mut self) -> Result<(), ContextLoss> {
        self.health.get().ensure_healthy()?;
        self.poll_reset_status()
    }

    /// Пиксели offscreen-цели (последний нарисованный кадр).
    pub fn read_offscreen_pixels(&self) -> Result<OffscreenPixels, ReadbackError> {
        let RenderTarget::Offscreen(Some(target)) = &self.target else {
            return Err(ReadbackError::NotOffscreen);
        };
        let gl = self.current_gl()?;
        Ok(target.read_pixels(gl))
    }

    /// Собирает программу из шейдеров GLSL ES 1.00. Удалять — [`Self::delete_program`].
    pub fn compile_es100_program(
        &self,
        sources: Es100ProgramSources<'_>,
    ) -> Result<glow::NativeProgram, ShaderError> {
        let gl = self.current_gl().map_err(ShaderError::ContextUnavailable)?;
        crate::shader::build_program(gl, sources)
    }

    /// Удаляет программу, собранную [`Self::compile_es100_program`].
    pub fn delete_program(&self, program: glow::NativeProgram) -> Result<(), GlAccessError> {
        let gl = self.current_gl()?;
        gl.delete_program(program);
        Ok(())
    }

    /// Контекст текущий и здоров → обёртка GL.
    fn current_gl(&self) -> Result<CurrentGl<'_>, GlAccessError> {
        self.ensure_current()?;
        Ok(CurrentGl::after_make_current(&self.gl))
    }

    /// Делает контекст текущим в этом потоке; потерю запоминает навсегда.
    fn ensure_current(&self) -> Result<(), GlAccessError> {
        if let ContextHealth::Lost(loss) = self.health.get() {
            return Err(GlAccessError::ContextLost(loss));
        }
        if !self.session.can_be_current_without_surface() {
            return Err(GlAccessError::DetachedWithoutSurfacelessContext);
        }
        match self.session.make_current() {
            Ok(()) => Ok(()),
            Err(error) if error.is_context_lost() => {
                let loss = ContextLoss::from_egl();
                self.health.set(ContextHealth::Lost(loss));
                Err(GlAccessError::ContextLost(loss))
            }
            Err(error) => Err(GlAccessError::MakeCurrent(error)),
        }
    }

    /// То же для операций, которые возвращают [`Gles2Error`].
    fn ensure_current_for_mutation(&self) -> Result<(), Gles2Error> {
        self.ensure_current().map_err(Gles2Error::from_gl_access)
    }

    /// Ошибка EGL; потеря контекста запоминается навсегда.
    fn observe_egl_error(&self, error: EglCallError) -> Gles2Error {
        let converted = Gles2Error::from_egl(error);
        if let Gles2Error::ContextLost(loss) = converted {
            self.health.set(ContextHealth::Lost(loss));
        }
        converted
    }

    fn poll_reset_status(&self) -> Result<(), ContextLoss> {
        let Some(query) = &self.reset_status else {
            return Ok(());
        };
        match ContextLoss::from_reset_status(query.query()) {
            None => Ok(()),
            Some(loss) => {
                log::error!("GLES2 context reset detected: {loss}");
                self.health.set(ContextHealth::Lost(loss));
                Err(loss)
            }
        }
    }

    /// Пометить контекст потерянным (тест: настоящий сброс GPU вызвать нельзя). Публичен под
    /// feature `test-support` для тестов потребителей (`render-gles2-shell`: путь потери
    /// устройства рендера); в production-сборку feature не входит.
    #[cfg(any(test, feature = "test-support"))]
    pub fn simulate_context_loss(&self, loss: ContextLoss) {
        self.health.set(ContextHealth::Lost(loss));
    }

    /// GL для начатого кадра: `begin_frame` уже сделал контекст текущим.
    pub(crate) fn gl_for_active_frame(&self) -> &glow::Context {
        &self.gl
    }

    /// Заливка для начатого кадра.
    pub(crate) fn clear_for_active_frame(&self, rgba: [f32; 4]) {
        CurrentGl::after_make_current(&self.gl).clear_color(rgba);
    }

    /// Показ начатого кадра; возвращает время `eglSwapBuffers` (`glFlush` без окна).
    pub(crate) fn present_active_frame(&mut self) -> Result<Duration, Gles2Error> {
        let present_started_at = Instant::now();
        match self.target {
            RenderTarget::Window => {
                self.session
                    .swap_buffers()
                    .map_err(|error| self.observe_egl_error(error))?;
            }
            RenderTarget::Offscreen(_) => CurrentGl::after_make_current(&self.gl).flush(),
        }
        let elapsed = present_started_at.elapsed();
        self.poll_reset_status()?;
        Ok(elapsed)
    }
}

impl Drop for Gles2Context {
    fn drop(&mut self) {
        let target = match &mut self.target {
            RenderTarget::Offscreen(target) => target.take(),
            RenderTarget::Window => None,
        };
        // GL-объекты удаляются, пока контекст жив и текущий. После потери контекста драйвер
        // освободит их сам вместе с контекстом — трогать GL нельзя.
        if let Some(target) = target {
            match self.current_gl() {
                Ok(gl) => target.destroy(gl),
                Err(error) => log::debug!("offscreen target left to the driver: {error}"),
            }
        }
        // Дальше поля: session (surface → context → display), затем окно.
    }
}

impl std::fmt::Debug for Gles2Context {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Gles2Context")
            .field("presentation", &self.presentation)
            .field("extent", &self.extent)
            .field("health", &self.health.get())
            .field("fence_strategy", &self.fences.strategy())
            .finish_non_exhaustive()
    }
}
