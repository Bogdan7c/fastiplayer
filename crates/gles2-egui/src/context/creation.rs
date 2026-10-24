//! Создание [`Gles2Context`]: libEGL, окно или offscreen, функции GL, возможности драйвера.

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::{Arc, OnceLock};

use glow::HasContext as _;

use super::display_registry::{DisplayLease, DisplayRegistry};
use super::driver::{EglDriver, NativeDisplay, SurfaceSize};
use super::egl_objects::{EglSession, ResetNotification, SessionRequest, SessionTarget};
use super::platform;
use super::safety::gl::CurrentGl;
use super::safety::{GlLoadError, LoadedGl, RobustnessExtension, SystemEgl, SystemEglLoadError};
use super::{ContextId, Gles2Context, PixelExtent, RenderTarget, WindowSource};
use crate::capabilities::{DriverReport, ExtensionSet, Gles2Capabilities, dma_buf};
use crate::error::CreateError;
use crate::fence::{FenceIssuer, FenceStrategy};
use crate::loss::{ContextHealth, ContextLoss};
use crate::offscreen::OffscreenTarget;
use crate::presentation::{
    OffscreenAlpha, PresentationSettings, PresentationState, VsyncMode, WindowAlphaMode,
    WindowAlphaRequest, WindowSystem,
};

/// libEGL процесса и реестр её display. Загружается один раз: повторная загрузка дала бы
/// второй реестр, и подсчёт пользователей display перестал бы работать.
struct SystemRuntime {
    egl: Arc<SystemEgl>,
    registry: Arc<DisplayRegistry>,
}

static SYSTEM_RUNTIME: OnceLock<Result<SystemRuntime, SystemEglLoadError>> = OnceLock::new();

fn system_runtime() -> Result<&'static SystemRuntime, CreateError> {
    let runtime = SYSTEM_RUNTIME.get_or_init(|| {
        SystemEgl::load().map(|egl| {
            let egl = Arc::new(egl);
            let driver: Arc<dyn EglDriver> = egl.clone();
            SystemRuntime {
                egl,
                registry: DisplayRegistry::new(driver),
            }
        })
    });
    runtime.as_ref().map_err(|error| match error {
        SystemEglLoadError::LibraryUnavailable { message } => CreateError::EglUnavailable {
            message: message.clone(),
        },
        SystemEglLoadError::VersionTooOld { provided } => {
            CreateError::EglVersionUnsupported { found: *provided }
        }
    })
}

impl Gles2Context {
    /// Контекст на окне. `extent` — текущий размер окна в физических пикселях.
    pub fn for_window(
        window: Arc<dyn WindowSource>,
        extent: PixelExtent,
        settings: PresentationSettings,
    ) -> Result<Self, CreateError> {
        let runtime = system_runtime()?;
        let display_handle = window
            .display_handle()
            .map_err(|error| CreateError::WindowHandleUnavailable {
                message: error.to_string(),
            })?
            .as_raw();
        let window_handle = window
            .window_handle()
            .map_err(|error| CreateError::WindowHandleUnavailable {
                message: error.to_string(),
            })?
            .as_raw();
        let binding = platform::window_binding(display_handle, window_handle)?;
        // `wl_egl_window` не бывает 0×0: свёрнутое окно стартует с 1×1, кадры пропускаются,
        // пока `resize` не сообщит настоящий размер.
        let initial_size = extent.surface_size().unwrap_or(SurfaceSize::ONE_PIXEL);
        let session = EglSession::open(
            &runtime.registry,
            SessionRequest {
                native_display: binding.native_display,
                target: SessionTarget::Window {
                    window: binding.window,
                    size: initial_size,
                },
                alpha: settings.alpha,
            },
        )?;
        let swap_interval = session
            .apply_vsync(settings.vsync)
            .map_err(|error| creation_egl_error(error, CreateError::PresentationSetup))?;
        let window_system = match binding.native_display {
            NativeDisplay::Wayland { .. } => WindowSystem::Wayland,
            NativeDisplay::X11 { .. } => WindowSystem::X11,
            NativeDisplay::Surfaceless => WindowSystem::Offscreen,
        };
        let presentation = PresentationState {
            window_system,
            settings,
            alpha: session.config().alpha,
            swap_interval,
            detached: false,
        };
        assemble(
            runtime,
            session,
            presentation,
            extent,
            RenderTarget::Window,
            Some(window),
        )
    }

    /// Контекст без окна (EGL surfaceless): кадр рисуется в FBO и читается
    /// [`Gles2Context::read_offscreen_pixels`]. Стенд пиксельных тестов. Цель непрозрачная —
    /// как окно без альфы.
    pub fn offscreen(size: SurfaceSize) -> Result<Self, CreateError> {
        Self::offscreen_with_alpha(size, OffscreenAlpha::Opaque)
    }

    /// Контекст без окна с выбранным смыслом альфы цели.
    ///
    /// Цвет FBO — всегда RGBA8, альфа в нём есть. [`OffscreenAlpha::PremultipliedLayer`]
    /// говорит рисующим (маска углов), что цель — прозрачный premultiplied-слой, как окно с
    /// прозрачностью у композитора. Это и стенд маски углов, и честный режим для приложения,
    /// которое само композитит результат как текстуру.
    pub fn offscreen_with_alpha(
        size: SurfaceSize,
        target_alpha: OffscreenAlpha,
    ) -> Result<Self, CreateError> {
        let runtime = system_runtime()?;
        let session = EglSession::open(
            &runtime.registry,
            SessionRequest {
                native_display: NativeDisplay::Surfaceless,
                target: SessionTarget::Offscreen,
                alpha: WindowAlphaRequest::Opaque,
            },
        )?;
        let presentation = PresentationState {
            window_system: WindowSystem::Offscreen,
            settings: PresentationSettings {
                vsync: VsyncMode::Immediate,
                alpha: WindowAlphaRequest::Opaque,
            },
            alpha: match target_alpha {
                // Выбор конфига уже отметил offscreen-цель (`OpaqueWindowReason::OffscreenTarget`).
                OffscreenAlpha::Opaque => session.config().alpha,
                OffscreenAlpha::PremultipliedLayer => WindowAlphaMode::PremultipliedTransparent,
            },
            swap_interval: None,
            detached: false,
        };
        let extent = PixelExtent {
            width: size.width(),
            height: size.height(),
        };
        // Цель создаётся после загрузки GL (внутри `assemble`).
        assemble(
            runtime,
            session,
            presentation,
            extent,
            RenderTarget::Offscreen(None),
            None,
        )
    }
}

fn creation_egl_error(
    error: super::driver::EglCallError,
    other: fn(super::driver::EglCallError) -> CreateError,
) -> CreateError {
    if error.is_context_lost() {
        CreateError::ContextLost(ContextLoss::from_egl())
    } else {
        other(error)
    }
}

/// Общая часть создания: GL, возможности, fence, offscreen-цель.
fn assemble(
    runtime: &SystemRuntime,
    session: EglSession,
    presentation: PresentationState,
    extent: PixelExtent,
    target: RenderTarget,
    window: Option<Arc<dyn WindowSource>>,
) -> Result<Gles2Context, CreateError> {
    let loaded = runtime
        .egl
        .load_gl_for_current_context()
        .map_err(gl_load_error)?;
    let gl_extensions = loaded.gl.supported_extensions();
    let robustness_extension = if gl_extensions.contains("GL_KHR_robustness") {
        Some(RobustnessExtension::Khr)
    } else if gl_extensions.contains("GL_EXT_robustness") {
        Some(RobustnessExtension::Ext)
    } else {
        None
    };
    // Статус сброса имеет смысл только у контекста со стратегией LOSE_CONTEXT_ON_RESET:
    // без неё драйвер не обязан сообщать о сбросе (`GL_EXT_robustness`).
    let reset_status = match (session.reset_notification(), robustness_extension) {
        (ResetNotification::LoseContextOnReset, Some(extension)) => {
            runtime.egl.load_reset_status_query(extension)
        }
        _ => None,
    };

    let report = driver_report(&loaded, session.lease(), runtime.registry.driver().as_ref());
    let capabilities = Gles2Capabilities::from_report(report);
    let fence_strategy = if capabilities.tiers().conveniences.egl_fence_sync {
        FenceStrategy::EglFenceSync
    } else {
        FenceStrategy::FinishFallback
    };

    let mut context = Gles2Context {
        id: ContextId::next(),
        gl: loaded.gl,
        reset_status,
        capabilities,
        fences: FenceIssuer::new(fence_strategy),
        presentation,
        health: Cell::new(ContextHealth::Healthy),
        extent,
        target,
        session,
        _window_keepalive: window,
        _thread_bound: PhantomData,
    };
    if matches!(context.target, RenderTarget::Offscreen(None)) {
        attach_offscreen_target(&mut context)?;
    }
    log::info!(
        "GLES2 context ready: {} / {} ({}), EGL {} ({}), fence {:?}, alpha {:?}",
        context.capabilities.gl_renderer(),
        context.capabilities.gl_version_string(),
        context.capabilities.gl_vendor(),
        context.capabilities.egl_version(),
        context.capabilities.egl_vendor(),
        fence_strategy,
        context.presentation.alpha,
    );
    Ok(context)
}

fn attach_offscreen_target(context: &mut Gles2Context) -> Result<(), CreateError> {
    let Some(size) = context.extent.surface_size() else {
        return Err(CreateError::OffscreenTarget {
            reason: "offscreen target size is empty".to_owned(),
        });
    };
    let gl = CurrentGl::after_make_current(&context.gl);
    let target = OffscreenTarget::create(gl, size)
        .map_err(|reason| CreateError::OffscreenTarget { reason })?;
    context.target = RenderTarget::Offscreen(Some(target));
    Ok(())
}

fn gl_load_error(error: GlLoadError) -> CreateError {
    let reason = match error {
        GlLoadError::MissingGetString => "glGetString is unavailable".to_owned(),
        GlLoadError::VersionUnavailable => "GL_VERSION is NULL (no current context)".to_owned(),
        GlLoadError::NotGles { version } => {
            format!("context is not OpenGL ES 2.0+ (GL_VERSION {version:?})")
        }
        GlLoadError::ExtensionsUnavailable => "GL_EXTENSIONS is NULL".to_owned(),
    };
    CreateError::GlLoad { reason }
}

/// Собирает ответы драйвера для [`Gles2Capabilities`]. Контекст текущий.
fn driver_report(loaded: &LoadedGl, lease: &DisplayLease, driver: &dyn EglDriver) -> DriverReport {
    let gl = CurrentGl::after_make_current(&loaded.gl);
    let display_extensions = lease.info().extensions.clone();
    DriverReport {
        gl_version_string: loaded.version_string.clone(),
        reported_gles_version: loaded.version,
        gl_vendor: gl.string(glow::VENDOR),
        gl_renderer: gl.string(glow::RENDERER),
        shading_language_version: gl.string(glow::SHADING_LANGUAGE_VERSION),
        gl_extensions: ExtensionSet::from_names(
            loaded.gl.supported_extensions().iter().map(String::as_str),
        ),
        egl_version: lease.info().version,
        egl_vendor: lease.info().vendor.clone(),
        egl_client_extensions: ExtensionSet::from_space_separated(&driver.client_extensions()),
        dma_buf: dma_buf::query_support(driver, lease.display(), &display_extensions),
        egl_display_extensions: display_extensions,
        max_texture_size: u32::try_from(gl.integer(glow::MAX_TEXTURE_SIZE)).unwrap_or(0),
        fragment_high_float_precision_bits: gl.fragment_high_float_precision_bits(),
    }
}
