//! RAII-владельцы объектов EGL и сессия «display + конфиг + контекст + surface».
//!
//! Порядок разрушения закреплён порядком полей [`EglSession`] (Rust разрушает поля сверху вниз
//! после `Drop::drop`): сначала контекст перестаёт быть текущим, затем surface (и под ней
//! `wl_egl_window`), затем контекст, последней — аренда display. Каждый `destroy_*` вызывается
//! ровно один раз: дескриптор живёт только внутри своего владельца.

use std::sync::Arc;

use super::config_select::{self, ConfigSelectionError, ConfigSelectionRequest, SelectedConfig};
use super::display_registry::{DisplayLease, DisplayOpenError, DisplayRegistry};
use super::driver::{
    ConfigQuery, ConfigSurfaceKind, ContextId, ContextRequest, CurrentBinding, DamageRect,
    DamageSwapExtension, EglCallError, NativeDisplay, PlatformWindowTarget,
    ResetNotificationRequest, SurfaceId, SurfaceSize, WaylandEglWindowId,
};
use super::platform::{NativeWindow, required_platform_extensions};
use crate::capabilities::ExtensionSet;
use crate::damage::BufferAge;
use crate::error::CreateError;
use crate::frame::PresentKind;
use crate::loss::ContextLoss;
use crate::presentation::{DetachOutcome, ReattachOutcome, SwapIntervalOutcome, VsyncMode};

/// Владелец `EGLContext`.
struct ContextOwner {
    lease: Arc<DisplayLease>,
    context: ContextId,
}

impl Drop for ContextOwner {
    fn drop(&mut self) {
        let driver = self.lease.driver();
        if let Err(error) = driver.destroy_context(self.lease.display(), self.context) {
            log::warn!("eglDestroyContext failed: {error}");
        }
    }
}

/// Владелец `EGLSurface` (и `wl_egl_window` под ней на Wayland).
struct SurfaceOwner {
    lease: Arc<DisplayLease>,
    surface: SurfaceId,
    wayland_window: Option<WaylandEglWindowId>,
}

impl Drop for SurfaceOwner {
    fn drop(&mut self) {
        let driver = self.lease.driver();
        if let Err(error) = driver.destroy_surface(self.lease.display(), self.surface) {
            log::warn!("eglDestroySurface failed: {error}");
        }
        // `wl_egl_window` — только после EGLSurface поверх него (libwayland-egl: буферы
        // surface ссылаются на окно).
        if let Some(window) = self.wayland_window {
            driver.destroy_wayland_egl_window(window);
        }
    }
}

/// Во что сейчас рисует контекст.
enum SurfaceBinding {
    /// Окно. `None` — окно отпущено (`detach`), EGLSurface на нём нет.
    Window {
        window: NativeWindow,
        surface: Option<SurfaceOwner>,
    },
    /// Offscreen-цель. `None` — контекст без surface (`EGL_KHR_surfaceless_context`), иначе
    /// pbuffer 1×1 только ради привязки (рисуем всё равно в свой FBO).
    Offscreen { pbuffer: Option<SurfaceOwner> },
}

/// Что создавать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionTarget {
    /// Окно и его начальный размер.
    Window {
        window: NativeWindow,
        size: SurfaceSize,
    },
    /// Без окна.
    Offscreen,
}

/// Запрос на сессию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionRequest {
    /// Display.
    pub(crate) native_display: NativeDisplay,
    /// Цель.
    pub(crate) target: SessionTarget,
}

/// Удалось ли включить уведомление о сбросе GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResetNotification {
    /// Контекст создан со стратегией `EGL_LOSE_CONTEXT_ON_RESET_EXT`.
    LoseContextOnReset,
    /// Без неё (нет расширения или драйвер отказал): о потере узнаём только из ошибок EGL.
    Unavailable,
}

/// Почему окно не вернулось.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReattachWindowError {
    /// Новая surface не создалась.
    SurfaceCreation(EglCallError),
    /// Surface создана, но контекст не стал текущим с ней.
    MakeCurrent(EglCallError),
}

/// Display + конфиг + контекст + surface.
///
/// ВНИМАНИЕ: порядок полей — это порядок разрушения. `binding` (surface) раньше `context`,
/// `lease` (display) последней.
pub(crate) struct EglSession {
    binding: SurfaceBinding,
    context: ContextOwner,
    config: SelectedConfig,
    reset_notification: ResetNotification,
    surfaceless_context: bool,
    /// Дисплей заявляет `EGL_EXT_buffer_age`.
    buffer_age_supported: bool,
    /// Какую функцию показа с повреждениями вызывать (`None` — расширений нет).
    damage_swap: Option<DamageSwapExtension>,
    lease: Arc<DisplayLease>,
}

/// Какое расширение показа с повреждениями выбрать по набору расширений дисплея. Оба имеют
/// одну сигнатуру и семантику; KHR — основное, EXT — запасное.
fn damage_swap_extension(extensions: &ExtensionSet) -> Option<DamageSwapExtension> {
    if extensions.contains("EGL_KHR_swap_buffers_with_damage") {
        Some(DamageSwapExtension::Khr)
    } else if extensions.contains("EGL_EXT_swap_buffers_with_damage") {
        Some(DamageSwapExtension::Ext)
    } else {
        None
    }
}

impl EglSession {
    /// Открывает сессию и делает контекст текущим в этом потоке.
    pub(crate) fn open(
        registry: &Arc<DisplayRegistry>,
        request: SessionRequest,
    ) -> Result<Self, CreateError> {
        let driver = registry.driver();
        let client_extensions = ExtensionSet::from_space_separated(&driver.client_extensions());
        let platform_extensions = required_platform_extensions(request.native_display);
        if !client_extensions.contains_any(platform_extensions) {
            return Err(CreateError::PlatformExtensionMissing {
                extension: platform_extensions
                    .first()
                    .copied()
                    .unwrap_or("EGL platform"),
            });
        }

        let lease = Arc::new(registry.acquire(request.native_display).map_err(
            |error| match error {
                DisplayOpenError::Driver(error) => CreateError::DisplayOpen(error),
                DisplayOpenError::VersionTooOld { found } => {
                    CreateError::EglVersionUnsupported { found }
                }
            },
        )?);
        driver
            .bind_gles_api()
            .map_err(CreateError::GlesApiUnavailable)?;

        let surfaceless_context = lease
            .info()
            .extensions
            .contains("EGL_KHR_surfaceless_context");
        let config = select_session_config(&lease, request, surfaceless_context)?;
        let (context, reset_notification) = create_context(&lease, config)?;
        let binding = match request.target {
            SessionTarget::Window { window, size } => SurfaceBinding::Window {
                window,
                surface: Some(
                    create_window_surface(&lease, config, window, size)
                        .map_err(CreateError::SurfaceCreation)?,
                ),
            },
            SessionTarget::Offscreen if surfaceless_context => {
                SurfaceBinding::Offscreen { pbuffer: None }
            }
            SessionTarget::Offscreen => SurfaceBinding::Offscreen {
                pbuffer: Some(create_binding_pbuffer(&lease, config)?),
            },
        };
        let session = Self {
            binding,
            context,
            config,
            reset_notification,
            surfaceless_context,
            buffer_age_supported: lease.info().extensions.contains("EGL_EXT_buffer_age"),
            damage_swap: damage_swap_extension(&lease.info().extensions),
            lease,
        };
        // Отказ здесь разрушит уже созданное в правильном порядке (Drop сессии).
        session.make_current().map_err(|error| {
            if error.is_context_lost() {
                CreateError::ContextLost(ContextLoss::from_egl())
            } else {
                CreateError::MakeCurrent(error)
            }
        })?;
        Ok(session)
    }

    /// Аренда display (fence-ы держат её клон).
    pub(crate) fn lease(&self) -> &Arc<DisplayLease> {
        &self.lease
    }

    /// Выбранный конфиг.
    pub(crate) fn config(&self) -> SelectedConfig {
        self.config
    }

    /// Создан ли контекст со стратегией сброса.
    pub(crate) fn reset_notification(&self) -> ResetNotification {
        self.reset_notification
    }

    /// Есть ли сейчас surface окна (не отпущено и это окно).
    pub(crate) fn has_window_surface(&self) -> bool {
        matches!(
            self.binding,
            SurfaceBinding::Window {
                surface: Some(_),
                ..
            }
        )
    }

    /// Цель — окно (отпущенное или нет).
    pub(crate) fn is_window_target(&self) -> bool {
        matches!(self.binding, SurfaceBinding::Window { .. })
    }

    /// Можно ли работать с GL без surface: окно отпущено, а контекст всё равно текущий.
    pub(crate) fn can_be_current_without_surface(&self) -> bool {
        self.surfaceless_context || !self.is_window_target() || self.has_window_surface()
    }

    /// Делает контекст текущим: с surface, если она есть; без неё — через
    /// `EGL_KHR_surfaceless_context`; если и его нет (окно отпущено) — отвязывает всё.
    pub(crate) fn make_current(&self) -> Result<(), EglCallError> {
        let context = self.context.context;
        let binding = match &self.binding {
            SurfaceBinding::Window {
                surface: Some(surface),
                ..
            }
            | SurfaceBinding::Offscreen {
                pbuffer: Some(surface),
            } => CurrentBinding::Surface {
                context,
                surface: surface.surface,
            },
            SurfaceBinding::Window { surface: None, .. }
            | SurfaceBinding::Offscreen { pbuffer: None }
                if self.surfaceless_context =>
            {
                CurrentBinding::Surfaceless { context }
            }
            SurfaceBinding::Window { surface: None, .. }
            | SurfaceBinding::Offscreen { pbuffer: None } => CurrentBinding::Nothing,
        };
        self.lease
            .driver()
            .make_current(self.lease.display(), binding)
    }

    /// `eglSwapBuffers` surface окна. `Ok(false)` — показывать некуда (окно отпущено или
    /// цель offscreen).
    pub(crate) fn swap_buffers(&self) -> Result<bool, EglCallError> {
        match &self.binding {
            SurfaceBinding::Window {
                surface: Some(surface),
                ..
            } => {
                self.lease
                    .driver()
                    .swap_buffers(self.lease.display(), surface.surface)?;
                Ok(true)
            }
            SurfaceBinding::Window { surface: None, .. } | SurfaceBinding::Offscreen { .. } => {
                Ok(false)
            }
        }
    }

    /// Возраст заднего буфера surface окна (`EGL_EXT_buffer_age`). Вызывать после
    /// `make_current` и до рисования кадра. [`BufferAge::Unsupported`] — расширения нет или
    /// surface окна нет (окно отпущено, offscreen-цель).
    pub(crate) fn query_buffer_age(&self) -> Result<BufferAge, EglCallError> {
        if !self.buffer_age_supported {
            return Ok(BufferAge::Unsupported);
        }
        let SurfaceBinding::Window {
            surface: Some(surface),
            ..
        } = &self.binding
        else {
            return Ok(BufferAge::Unsupported);
        };
        let age = self
            .lease
            .driver()
            .query_buffer_age(self.lease.display(), surface.surface)?;
        Ok(BufferAge::from_egl_value(age))
    }

    /// Показ с подсказкой о повреждениях (`damage` — прямоугольники в координатах EGL, начало
    /// снизу слева). Без расширения или с пустым списком (по спецификации 0 прямоугольников —
    /// «повреждена вся surface») это обычный `eglSwapBuffers`. `Ok(None)` — показывать некуда.
    pub(crate) fn swap_buffers_with_damage(
        &self,
        damage: &[DamageRect],
    ) -> Result<Option<PresentKind>, EglCallError> {
        let Some(extension) = self.damage_swap.filter(|_| !damage.is_empty()) else {
            return Ok(self.swap_buffers()?.then_some(PresentKind::Plain));
        };
        match &self.binding {
            SurfaceBinding::Window {
                surface: Some(surface),
                ..
            } => {
                self.lease.driver().swap_buffers_with_damage(
                    self.lease.display(),
                    surface.surface,
                    extension,
                    damage,
                )?;
                Ok(Some(PresentKind::WithDamage))
            }
            SurfaceBinding::Window { surface: None, .. } | SurfaceBinding::Offscreen { .. } => {
                Ok(None)
            }
        }
    }

    /// `eglSwapInterval` для surface окна (контекст должен быть текущим с ней).
    /// `Ok(None)` — surface окна нет.
    pub(crate) fn apply_vsync(
        &self,
        vsync: VsyncMode,
    ) -> Result<Option<SwapIntervalOutcome>, EglCallError> {
        if !self.has_window_surface() {
            return Ok(None);
        }
        self.make_current()?;
        let requested = vsync.swap_interval();
        self.lease
            .driver()
            .swap_interval(self.lease.display(), requested)?;
        Ok(Some(SwapIntervalOutcome::for_config(
            requested,
            self.config.description.min_swap_interval,
            self.config.description.max_swap_interval,
        )))
    }

    /// Новый размер окна. На Wayland — `wl_egl_window_resize` (размер буферов задаёт клиент),
    /// на X11 EGL сам следует за размером окна.
    pub(crate) fn resize_window(&self, size: SurfaceSize) {
        if let SurfaceBinding::Window {
            surface: Some(surface),
            ..
        } = &self.binding
            && let Some(window) = surface.wayland_window
        {
            self.lease.driver().resize_wayland_egl_window(window, size);
        }
    }

    /// Отпускает окно: контекст перестаёт рисовать в surface, surface разрушается.
    /// Конфиг и контекст остаются (GL-объекты живы).
    pub(crate) fn detach_window(&mut self) -> Result<DetachOutcome, EglCallError> {
        let SurfaceBinding::Window { surface, .. } = &mut self.binding else {
            return Ok(DetachOutcome::AlreadyDetached);
        };
        let Some(owner) = surface.take() else {
            return Ok(DetachOutcome::AlreadyDetached);
        };
        // Сначала снять surface с контекста, потом разрушать: так EGL не откладывает удаление
        // «текущей» surface (EGL 1.5, 3.7.3).
        let release = self.make_current();
        drop(owner);
        release.map(|()| DetachOutcome::Detached)
    }

    /// Отпускает окно у ПОТЕРЯННОГО контекста. Surface разрушается всё равно: после сброса GPU
    /// рендер пересоздаётся, и новый обязан создать свою surface на том же окне (одно окно —
    /// одна surface). Контекст отвязывается полностью: делать текущим потерянный контекст
    /// бессмысленно, а отказ отвязки не мешает разрушить surface.
    pub(crate) fn detach_window_after_context_loss(&mut self) -> DetachOutcome {
        let SurfaceBinding::Window { surface, .. } = &mut self.binding else {
            return DetachOutcome::AlreadyDetached;
        };
        let Some(owner) = surface.take() else {
            return DetachOutcome::AlreadyDetached;
        };
        if let Err(error) = self
            .lease
            .driver()
            .make_current(self.lease.display(), CurrentBinding::Nothing)
        {
            log::warn!("eglMakeCurrent(EGL_NO_CONTEXT) after context loss failed: {error}");
        }
        drop(owner);
        DetachOutcome::Detached
    }

    /// Возвращает окно: новая surface на прежних display и конфиге.
    pub(crate) fn reattach_window(
        &mut self,
        size: SurfaceSize,
    ) -> Result<ReattachOutcome, ReattachWindowError> {
        let config = self.config;
        let lease = Arc::clone(&self.lease);
        let SurfaceBinding::Window { window, surface } = &mut self.binding else {
            return Ok(ReattachOutcome::AlreadyAttached);
        };
        if surface.is_some() {
            return Ok(ReattachOutcome::AlreadyAttached);
        }
        let created = create_window_surface(&lease, config, *window, size)
            .map_err(ReattachWindowError::SurfaceCreation)?;
        *surface = Some(created);
        self.make_current()
            .map_err(ReattachWindowError::MakeCurrent)?;
        Ok(ReattachOutcome::Reattached)
    }
}

impl Drop for EglSession {
    fn drop(&mut self) {
        // Контекст не должен оставаться текущим, когда его и surface разрушают.
        let driver = self.lease.driver();
        if let Err(error) = driver.make_current(self.lease.display(), CurrentBinding::Nothing) {
            log::warn!("eglMakeCurrent(EGL_NO_CONTEXT) before teardown failed: {error}");
        }
        // Дальше поля разрушаются в порядке объявления: binding → context → lease.
    }
}

fn select_session_config(
    lease: &DisplayLease,
    request: SessionRequest,
    surfaceless_context: bool,
) -> Result<SelectedConfig, CreateError> {
    let offscreen_kind = if surfaceless_context {
        ConfigSurfaceKind::Surfaceless
    } else {
        ConfigSurfaceKind::Pbuffer
    };
    let (surface_kind, required_native_visual, offscreen) = match request.target {
        SessionTarget::Window {
            window: NativeWindow::X11 { visual_id, .. },
            ..
        } => (ConfigSurfaceKind::Window, visual_id, false),
        SessionTarget::Window { .. } => (ConfigSurfaceKind::Window, None, false),
        SessionTarget::Offscreen => (offscreen_kind, None, true),
    };
    let candidates = lease
        .driver()
        .matching_configs(
            lease.display(),
            ConfigQuery {
                surface_kind,
                minimum_alpha_bits: 0,
            },
        )
        .map_err(CreateError::ConfigQuery)?;
    config_select::select_config(
        &candidates,
        ConfigSelectionRequest {
            required_native_visual,
            offscreen,
        },
    )
    .map_err(|error| match error {
        ConfigSelectionError::NoGles2Config => CreateError::NoGles2Config,
        ConfigSelectionError::NoConfigForWindowVisual { visual_id } => {
            CreateError::NoConfigForWindowVisual { visual_id }
        }
    })
}

fn create_context(
    lease: &Arc<DisplayLease>,
    config: SelectedConfig,
) -> Result<(ContextOwner, ResetNotification), CreateError> {
    let driver = lease.driver();
    let display = lease.display();
    let config_id = config.description.id;
    let robustness_available = lease
        .info()
        .extensions
        .contains("EGL_EXT_create_context_robustness");
    if robustness_available {
        let robust = driver.create_context(
            display,
            config_id,
            ContextRequest {
                reset_notification: ResetNotificationRequest::LoseContextOnReset,
            },
        );
        match robust {
            Ok(context) => {
                return Ok((
                    ContextOwner {
                        lease: Arc::clone(lease),
                        context,
                    },
                    ResetNotification::LoseContextOnReset,
                ));
            }
            Err(error) if error.is_context_lost() => {
                return Err(CreateError::ContextLost(ContextLoss::from_egl()));
            }
            Err(error) => {
                // Расширение есть, но сочетание с GLES2 драйвер не принял: работаем без
                // уведомления о сбросе, о потере узнаем из ошибок EGL.
                log::info!("robust GLES2 context rejected ({error}), retrying without it");
            }
        }
    }
    let context = driver
        .create_context(
            display,
            config_id,
            ContextRequest {
                reset_notification: ResetNotificationRequest::NotRequested,
            },
        )
        .map_err(CreateError::ContextCreation)?;
    Ok((
        ContextOwner {
            lease: Arc::clone(lease),
            context,
        },
        ResetNotification::Unavailable,
    ))
}

fn create_window_surface(
    lease: &Arc<DisplayLease>,
    config: SelectedConfig,
    window: NativeWindow,
    size: SurfaceSize,
) -> Result<SurfaceOwner, EglCallError> {
    let driver = lease.driver();
    let display = lease.display();
    let config_id = config.description.id;
    match window {
        NativeWindow::Wayland { surface_address } => {
            let wayland_window = driver.create_wayland_egl_window(surface_address, size)?;
            match driver.create_window_surface(
                display,
                config_id,
                PlatformWindowTarget::Wayland(wayland_window),
            ) {
                Ok(surface) => Ok(SurfaceOwner {
                    lease: Arc::clone(lease),
                    surface,
                    wayland_window: Some(wayland_window),
                }),
                Err(error) => {
                    // EGLSurface не создалась — `wl_egl_window` больше никому не нужен.
                    driver.destroy_wayland_egl_window(wayland_window);
                    Err(error)
                }
            }
        }
        NativeWindow::X11 { window_xid, .. } => {
            let surface = driver.create_window_surface(
                display,
                config_id,
                PlatformWindowTarget::X11 { window_xid },
            )?;
            Ok(SurfaceOwner {
                lease: Arc::clone(lease),
                surface,
                wayland_window: None,
            })
        }
    }
}

/// Pbuffer 1×1 — только чтобы сделать offscreen-контекст текущим без
/// `EGL_KHR_surfaceless_context`.
fn create_binding_pbuffer(
    lease: &Arc<DisplayLease>,
    config: SelectedConfig,
) -> Result<SurfaceOwner, CreateError> {
    let surface = lease
        .driver()
        .create_pbuffer_surface(
            lease.display(),
            config.description.id,
            SurfaceSize::ONE_PIXEL,
        )
        .map_err(CreateError::SurfaceCreation)?;
    Ok(SurfaceOwner {
        lease: Arc::clone(lease),
        surface,
        wayland_window: None,
    })
}
