//! Единственное место crate-а с `unsafe`: настоящая libEGL, libwayland-egl и загрузка GL.
//!
//! Правила модуля:
//! - каждый `unsafe`-блок предварён комментарием `// SAFETY:` — почему вызов корректен;
//! - дескрипторы EGL снаружи — числа ([`DisplayId`] и т.п.), указателями они становятся только
//!   здесь и только на время вызова;
//! - владение (кто и когда разрушает объект) решают RAII-типы в соседних модулях, здесь только
//!   перевод вызова и кода ошибки;
//! - ни одного `unwrap/expect`: отказ драйвера — типизированная ошибка.
//!
//! Спецификации: EGL 1.5 (registry.khronos.org/EGL/specs/eglspec.1.5.pdf), `EGL_KHR_platform_x11`,
//! `EGL_KHR_platform_wayland`, `EGL_MESA_platform_surfaceless`, `EGL_EXT_image_dma_buf_import_modifiers`,
//! `EGL_EXT_create_context_robustness`, `GL_EXT_robustness` / `GL_KHR_robustness`.

use std::ffi::{CStr, c_void};
use std::os::raw::{c_uint, c_ulong};
use std::time::Duration;

use khronos_egl as egl;

use super::dma_buf_texture::DmaBufImageAttributes;

mod damage_swap;
mod dma_buf_query;
pub(crate) mod egl_image;
pub(crate) mod gl;

use super::driver::{
    ConfigDescription, ConfigId, ConfigQuery, ConfigSurfaceKind, ContextId, ContextRequest,
    CurrentBinding, DamageRect, DamageSwapExtension, DisplayId, DisplayString, DmaBufModifier,
    EglCallError, EglDriver, EglErrorCode, EglOperation, EglVersion, ImageId, NativeDisplay,
    PlatformWindowTarget, ResetNotificationRequest, SurfaceId, SurfaceSize, SyncId, SyncWaitStatus,
    WaylandEglWindowId,
};

/// Загруженная libEGL с интерфейсом EGL 1.5.
type Egl15Instance = egl::DynamicInstance<egl::EGL1_5>;

/// `EGL_PLATFORM_WAYLAND_KHR` (`EGL_KHR_platform_wayland`).
const EGL_PLATFORM_WAYLAND_KHR: egl::Enum = 0x31D8;
/// `EGL_PLATFORM_X11_KHR` (`EGL_KHR_platform_x11`).
const EGL_PLATFORM_X11_KHR: egl::Enum = 0x31D5;
/// `EGL_PLATFORM_SURFACELESS_MESA` (`EGL_MESA_platform_surfaceless`).
const EGL_PLATFORM_SURFACELESS_MESA: egl::Enum = 0x31DD;
/// `EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT` (`EGL_EXT_create_context_robustness`).
const EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT: egl::Int = 0x3138;
/// `EGL_LOSE_CONTEXT_ON_RESET_EXT`.
const EGL_LOSE_CONTEXT_ON_RESET_EXT: egl::Int = 0x31BF;
/// `GL_VERSION`.
const GL_VERSION: c_uint = 0x1F02;
/// `GL_EXTENSIONS`.
const GL_EXTENSIONS: c_uint = 0x1F03;

/// `glGetString`.
type GlGetStringFn = unsafe extern "system" fn(c_uint) -> *const u8;
/// `glGetGraphicsResetStatusEXT` / `glGetGraphicsResetStatusKHR`.
type GlGetGraphicsResetStatusFn = unsafe extern "system" fn() -> c_uint;

/// Почему libEGL не загрузилась.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SystemEglLoadError {
    /// Библиотеки `libEGL.so.1`/`libEGL.so` нет или она не открылась.
    LibraryUnavailable { message: String },
    /// Библиотека есть, но её интерфейс старше EGL 1.5.
    VersionTooOld { provided: EglVersion },
}

/// Настоящая libEGL (+ libwayland-egl по требованию).
pub(crate) struct SystemEgl {
    instance: Egl15Instance,
    /// Адреса функций показа с повреждениями (ищутся один раз, не на каждый кадр).
    damage_swap_functions: damage_swap::DamageSwapFunctions,
}

impl SystemEgl {
    /// Загружает системную libEGL (`libEGL.so.1`, затем `libEGL.so`) и требует интерфейс
    /// EGL 1.5 (решение владельца, сессия 03).
    pub(crate) fn load() -> Result<Self, SystemEglLoadError> {
        match Self::load_from_library_name("libEGL.so.1") {
            Err(SystemEglLoadError::LibraryUnavailable { .. }) => {
                Self::load_from_library_name("libEGL.so")
            }
            loaded => loaded,
        }
    }

    /// Загружает libEGL по имени файла (для теста «библиотеки нет»).
    pub(crate) fn load_from_library_name(library_name: &str) -> Result<Self, SystemEglLoadError> {
        // SAFETY: `load_required_from_filename` открывает библиотеку и берёт символы по именам
        // спецификации EGL 1.5. Корректность ABI библиотеки — обязанность драйвера Mesa/вендора;
        // при загрузке вызывается только `eglQueryString` версии.
        let loaded = unsafe { Egl15Instance::load_required_from_filename(library_name) };
        match loaded {
            Ok(instance) => Ok(Self {
                instance,
                damage_swap_functions: damage_swap::DamageSwapFunctions::default(),
            }),
            Err(egl::LoadError::InvalidVersion { provided, .. }) => {
                Err(SystemEglLoadError::VersionTooOld {
                    provided: egl_version_from_library(provided),
                })
            }
            Err(egl::LoadError::Library(error)) => Err(SystemEglLoadError::LibraryUnavailable {
                message: error.to_string(),
            }),
        }
    }

    /// `eglGetProcAddress` как нетипизированный указатель (для загрузчика glow).
    fn proc_address(&self, name: &str) -> *const c_void {
        // EGL 1.5 (раздел 3.10) гарантирует адреса и core-функций GL, и функций расширений.
        match self.instance.get_proc_address(name) {
            Some(function) => function as *const c_void,
            None => std::ptr::null(),
        }
    }
}

fn egl_version_from_library(version: egl::Version) -> EglVersion {
    let (major, minor) = match version {
        egl::Version::EGL1_0 => (1, 0),
        egl::Version::EGL1_1 => (1, 1),
        egl::Version::EGL1_2 => (1, 2),
        egl::Version::EGL1_3 => (1, 3),
        egl::Version::EGL1_4 => (1, 4),
        egl::Version::EGL1_5 => (1, 5),
    };
    EglVersion { major, minor }
}

fn error_code(error: egl::Error) -> EglErrorCode {
    match error {
        egl::Error::ContextLost => EglErrorCode::ContextLost,
        egl::Error::BadAlloc => EglErrorCode::BadAlloc,
        egl::Error::BadMatch => EglErrorCode::BadMatch,
        egl::Error::BadConfig => EglErrorCode::BadConfig,
        egl::Error::BadAttribute => EglErrorCode::BadAttribute,
        egl::Error::BadNativeWindow => EglErrorCode::BadNativeWindow,
        egl::Error::BadSurface => EglErrorCode::BadSurface,
        egl::Error::NotInitialized => EglErrorCode::NotInitialized,
        other => EglErrorCode::Other(other.native()),
    }
}

fn call_error(operation: EglOperation) -> impl Fn(egl::Error) -> EglCallError {
    move |error| EglCallError::new(operation, error_code(error))
}

fn display_handle(display: DisplayId) -> egl::Display {
    // SAFETY: `DisplayId` создаётся только из ответа `eglGetPlatformDisplay` (см.
    // `get_platform_display`), значит это настоящий `EGLDisplay` этой библиотеки. Владелец
    // (`display_registry`) не отдаёт дескриптор после `eglTerminate`.
    unsafe { egl::Display::from_ptr(display.0 as egl::EGLDisplay) }
}

fn config_handle(config: ConfigId) -> egl::Config {
    // SAFETY: `ConfigId` получен из `eglChooseConfig` на том же display; конфиги живут, пока
    // display инициализирован, а display переживает всех своих пользователей (реестр).
    unsafe { egl::Config::from_ptr(config.0 as egl::EGLConfig) }
}

fn context_handle(context: ContextId) -> egl::Context {
    // SAFETY: `ContextId` получен из `eglCreateContext`; RAII-владелец разрушает его ровно
    // один раз и после этого дескриптор не используется.
    unsafe { egl::Context::from_ptr(context.0 as egl::EGLContext) }
}

fn surface_handle(surface: SurfaceId) -> egl::Surface {
    // SAFETY: `SurfaceId` получен из `eglCreate*Surface`; RAII-владелец разрушает его ровно
    // один раз и после этого дескриптор не используется.
    unsafe { egl::Surface::from_ptr(surface.0 as egl::EGLSurface) }
}

fn sync_handle(sync: SyncId) -> egl::Sync {
    // SAFETY: `SyncId` получен из `eglCreateSync`; `GpuFence` разрушает его ровно один раз.
    unsafe { egl::Sync::from_ptr(sync.0 as egl::EGLSync) }
}

fn wayland_egl_library() -> Option<&'static wayland_sys::egl::WaylandEgl> {
    // libwayland-egl открывается один раз на процесс (ленивая статическая в wayland-sys).
    // `None` — библиотеки нет: это типизированная ошибка создания окна, а не паника.
    wayland_sys::egl::wayland_egl_option()
}

fn timeout_nanoseconds(timeout: Duration) -> egl::Time {
    // Таймаут больше ~584 лет прижимается к «ждать вечно» — смысл тот же.
    u64::try_from(timeout.as_nanos()).unwrap_or(egl::FOREVER)
}

impl EglDriver for SystemEgl {
    fn client_extensions(&self) -> String {
        // Без `EGL_EXT_client_extensions` запрос с `EGL_NO_DISPLAY` вернёт ошибку — это значит
        // «клиентских расширений нет», а не сбой.
        self.instance
            .query_string(None, egl::EXTENSIONS)
            .map(|extensions| extensions.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn get_platform_display(&self, native: NativeDisplay) -> Result<DisplayId, EglCallError> {
        let (platform, native_display) = match native {
            NativeDisplay::Wayland { display_address } => {
                (EGL_PLATFORM_WAYLAND_KHR, display_address as *mut c_void)
            }
            NativeDisplay::X11 { display_address } => (
                EGL_PLATFORM_X11_KHR,
                display_address.map_or(egl::DEFAULT_DISPLAY, |address| address as *mut c_void),
            ),
            NativeDisplay::Surfaceless => (EGL_PLATFORM_SURFACELESS_MESA, egl::DEFAULT_DISPLAY),
        };
        // SAFETY: `native_display` — адрес живого `wl_display`/Xlib `Display` из
        // `RawDisplayHandle` окна, которое переживает контекст (его держит `Gles2Context`), либо
        // `EGL_DEFAULT_DISPLAY`, допустимый для X11 и surfaceless по их расширениям. Список
        // атрибутов пуст и завершён `EGL_NONE`.
        let display = unsafe {
            self.instance
                .get_platform_display(platform, native_display, &[egl::ATTRIB_NONE])
        }
        .map_err(call_error(EglOperation::GetPlatformDisplay))?;
        Ok(DisplayId(display.as_ptr() as usize))
    }

    fn initialize(&self, display: DisplayId) -> Result<EglVersion, EglCallError> {
        let (major, minor) = self
            .instance
            .initialize(display_handle(display))
            .map_err(call_error(EglOperation::Initialize))?;
        Ok(EglVersion {
            major: u32::try_from(major).unwrap_or_default(),
            minor: u32::try_from(minor).unwrap_or_default(),
        })
    }

    fn terminate(&self, display: DisplayId) -> Result<(), EglCallError> {
        self.instance
            .terminate(display_handle(display))
            .map_err(call_error(EglOperation::Terminate))
    }

    fn query_display_string(
        &self,
        display: DisplayId,
        which: DisplayString,
    ) -> Result<String, EglCallError> {
        let name = match which {
            DisplayString::Vendor => egl::VENDOR,
            DisplayString::Extensions => egl::EXTENSIONS,
        };
        self.instance
            .query_string(Some(display_handle(display)), name)
            .map(|text| text.to_string_lossy().into_owned())
            .map_err(call_error(EglOperation::QueryString))
    }

    fn bind_gles_api(&self) -> Result<(), EglCallError> {
        self.instance
            .bind_api(egl::OPENGL_ES_API)
            .map_err(call_error(EglOperation::BindApi))
    }

    fn matching_configs(
        &self,
        display: DisplayId,
        query: ConfigQuery,
    ) -> Result<Vec<ConfigDescription>, EglCallError> {
        let display = display_handle(display);
        let mut attributes = vec![
            egl::RENDERABLE_TYPE,
            egl::OPENGL_ES2_BIT,
            egl::COLOR_BUFFER_TYPE,
            egl::RGB_BUFFER,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            query.minimum_alpha_bits,
        ];
        match query.surface_kind {
            ConfigSurfaceKind::Window => attributes.extend([egl::SURFACE_TYPE, egl::WINDOW_BIT]),
            ConfigSurfaceKind::Pbuffer => attributes.extend([egl::SURFACE_TYPE, egl::PBUFFER_BIT]),
            // `EGL_SURFACE_TYPE` по умолчанию — `EGL_WINDOW_BIT` (EGL 1.5, табл. 3.4); без окна
            // снимаем требование явно, иначе surfaceless-платформа не отдаст ни одного конфига.
            ConfigSurfaceKind::Surfaceless => attributes.extend([egl::SURFACE_TYPE, 0]),
        }
        attributes.push(egl::NONE);

        let choose_error = call_error(EglOperation::ChooseConfig);
        let count = self
            .instance
            .matching_config_count(display, &attributes)
            .map_err(&choose_error)?;
        let mut configs = Vec::with_capacity(count);
        if count > 0 {
            self.instance
                .choose_config(display, &attributes, &mut configs)
                .map_err(&choose_error)?;
        }

        configs
            .into_iter()
            .map(|config| {
                let attribute = |name| {
                    self.instance
                        .get_config_attrib(display, config, name)
                        .map_err(&choose_error)
                };
                Ok(ConfigDescription {
                    id: ConfigId(config.as_ptr() as usize),
                    red_bits: attribute(egl::RED_SIZE)?,
                    green_bits: attribute(egl::GREEN_SIZE)?,
                    blue_bits: attribute(egl::BLUE_SIZE)?,
                    alpha_bits: attribute(egl::ALPHA_SIZE)?,
                    native_visual_id: i64::from(attribute(egl::NATIVE_VISUAL_ID)?),
                    min_swap_interval: attribute(egl::MIN_SWAP_INTERVAL)?,
                    max_swap_interval: attribute(egl::MAX_SWAP_INTERVAL)?,
                })
            })
            .collect()
    }

    fn create_context(
        &self,
        display: DisplayId,
        config: ConfigId,
        request: ContextRequest,
    ) -> Result<ContextId, EglCallError> {
        let mut attributes = vec![egl::CONTEXT_CLIENT_VERSION, 2];
        if request.reset_notification == ResetNotificationRequest::LoseContextOnReset {
            attributes.extend([
                EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT,
                EGL_LOSE_CONTEXT_ON_RESET_EXT,
            ]);
        }
        attributes.push(egl::NONE);
        let context = self
            .instance
            .create_context(
                display_handle(display),
                config_handle(config),
                None,
                &attributes,
            )
            .map_err(call_error(EglOperation::CreateContext))?;
        Ok(ContextId(context.as_ptr() as usize))
    }

    fn destroy_context(&self, display: DisplayId, context: ContextId) -> Result<(), EglCallError> {
        self.instance
            .destroy_context(display_handle(display), context_handle(context))
            .map_err(call_error(EglOperation::DestroyContext))
    }

    fn create_wayland_egl_window(
        &self,
        wl_surface_address: usize,
        size: SurfaceSize,
    ) -> Result<WaylandEglWindowId, EglCallError> {
        let unavailable = EglCallError::new(
            EglOperation::CreateWaylandEglWindow,
            EglErrorCode::Unreported,
        );
        let library = wayland_egl_library().ok_or(unavailable)?;
        // SAFETY: `wl_surface_address` — адрес живого `wl_surface` из `RawWindowHandle::Wayland`
        // окна, которое переживает и `wl_egl_window`, и EGLSurface (окно держит `Gles2Context`).
        // Размер ≥ 1×1 гарантирует `SurfaceSize`.
        let window = unsafe {
            (library.wl_egl_window_create)(
                wl_surface_address as *mut wayland_sys::client::wl_proxy,
                size.width_i32(),
                size.height_i32(),
            )
        };
        if window.is_null() {
            Err(unavailable)
        } else {
            Ok(WaylandEglWindowId(window as usize))
        }
    }

    fn resize_wayland_egl_window(&self, window: WaylandEglWindowId, size: SurfaceSize) {
        // Окно создано этой же библиотекой, значит она загружена; `None` невозможен, но и
        // паники здесь нет — без библиотеки просто нечего менять.
        let Some(library) = wayland_egl_library() else {
            return;
        };
        // SAFETY: `window` получен из `wl_egl_window_create` и ещё не разрушен (владелец —
        // `egl_objects::WindowSurfaceOwner`). Смещение (dx, dy) нулевое: окно не сдвигается.
        unsafe {
            (library.wl_egl_window_resize)(
                window.0 as *mut wayland_sys::egl::wl_egl_window,
                size.width_i32(),
                size.height_i32(),
                0,
                0,
            );
        }
    }

    fn destroy_wayland_egl_window(&self, window: WaylandEglWindowId) {
        let Some(library) = wayland_egl_library() else {
            return;
        };
        // SAFETY: `window` получен из `wl_egl_window_create`, EGLSurface поверх него уже
        // разрушена (порядок держит `WindowSurfaceOwner::drop`), повторного вызова нет.
        unsafe {
            (library.wl_egl_window_destroy)(window.0 as *mut wayland_sys::egl::wl_egl_window);
        }
    }

    fn create_window_surface(
        &self,
        display: DisplayId,
        config: ConfigId,
        target: PlatformWindowTarget,
    ) -> Result<SurfaceId, EglCallError> {
        // По `EGL_KHR_platform_x11` platform-вариант принимает УКАЗАТЕЛЬ на `Window` (XID), а не
        // сам XID; переменная живёт до конца вызова.
        let mut x11_window_xid: c_ulong = match target {
            PlatformWindowTarget::X11 { window_xid } => window_xid,
            PlatformWindowTarget::Wayland(_) => 0,
        };
        let native_window: egl::NativeWindowType = match target {
            PlatformWindowTarget::Wayland(window) => window.0 as egl::NativeWindowType,
            PlatformWindowTarget::X11 { .. } => {
                std::ptr::from_mut(&mut x11_window_xid).cast::<c_void>()
            }
        };
        // SAFETY: `native_window` — либо живой `wl_egl_window` этого же окна (Wayland), либо
        // указатель на локальную копию XID живого окна (X11), принадлежащего тому же display
        // (platform совпадает: display открыт по тому же `RawDisplayHandle`). Атрибуты пусты.
        let surface = unsafe {
            self.instance.create_platform_window_surface(
                display_handle(display),
                config_handle(config),
                native_window,
                &[egl::ATTRIB_NONE],
            )
        }
        .map_err(call_error(EglOperation::CreateWindowSurface))?;
        Ok(SurfaceId(surface.as_ptr() as usize))
    }

    fn create_pbuffer_surface(
        &self,
        display: DisplayId,
        config: ConfigId,
        size: SurfaceSize,
    ) -> Result<SurfaceId, EglCallError> {
        let attributes = [
            egl::WIDTH,
            size.width_i32(),
            egl::HEIGHT,
            size.height_i32(),
            egl::NONE,
        ];
        let surface = self
            .instance
            .create_pbuffer_surface(display_handle(display), config_handle(config), &attributes)
            .map_err(call_error(EglOperation::CreatePbufferSurface))?;
        Ok(SurfaceId(surface.as_ptr() as usize))
    }

    fn destroy_surface(&self, display: DisplayId, surface: SurfaceId) -> Result<(), EglCallError> {
        self.instance
            .destroy_surface(display_handle(display), surface_handle(surface))
            .map_err(call_error(EglOperation::DestroySurface))
    }

    fn make_current(
        &self,
        display: DisplayId,
        binding: CurrentBinding,
    ) -> Result<(), EglCallError> {
        let (surface, context) = match binding {
            CurrentBinding::Surface { context, surface } => {
                (Some(surface_handle(surface)), Some(context_handle(context)))
            }
            CurrentBinding::Surfaceless { context } => (None, Some(context_handle(context))),
            CurrentBinding::Nothing => (None, None),
        };
        self.instance
            .make_current(display_handle(display), surface, surface, context)
            .map_err(call_error(EglOperation::MakeCurrent))
    }

    fn swap_buffers(&self, display: DisplayId, surface: SurfaceId) -> Result<(), EglCallError> {
        self.instance
            .swap_buffers(display_handle(display), surface_handle(surface))
            .map_err(call_error(EglOperation::SwapBuffers))
    }

    fn query_buffer_age(
        &self,
        display: DisplayId,
        surface: SurfaceId,
    ) -> Result<u32, EglCallError> {
        self.query_buffer_age_of_surface(display, surface)
    }

    fn swap_buffers_with_damage(
        &self,
        display: DisplayId,
        surface: SurfaceId,
        extension: DamageSwapExtension,
        rects: &[DamageRect],
    ) -> Result<(), EglCallError> {
        self.swap_surface_buffers_with_damage(display, surface, extension, rects)
    }

    fn swap_interval(&self, display: DisplayId, interval: i32) -> Result<(), EglCallError> {
        self.instance
            .swap_interval(display_handle(display), interval)
            .map_err(call_error(EglOperation::SwapInterval))
    }

    fn create_fence_sync(&self, display: DisplayId) -> Result<SyncId, EglCallError> {
        // SAFETY: тип `EGL_SYNC_FENCE` не принимает внешних дескрипторов (в отличие от OpenCL
        // event sync), атрибуты пусты и завершены `EGL_NONE`. Fence ставится в командный поток
        // текущего контекста — вызывающий делает свой контекст текущим.
        let sync = unsafe {
            self.instance.create_sync(
                display_handle(display),
                egl::SYNC_FENCE as egl::Enum,
                &[egl::ATTRIB_NONE],
            )
        }
        .map_err(call_error(EglOperation::CreateSync))?;
        Ok(SyncId(sync.as_ptr() as usize))
    }

    fn client_wait_sync(
        &self,
        display: DisplayId,
        sync: SyncId,
        timeout: Duration,
    ) -> Result<SyncWaitStatus, EglCallError> {
        // SAFETY: `sync` создан на этом же `display` (fence хранит пару вместе), ещё не
        // разрушен. Флаг сброса команд не ставим: ждать можно из потока без текущего контекста.
        let status = unsafe {
            self.instance.client_wait_sync(
                display_handle(display),
                sync_handle(sync),
                0,
                timeout_nanoseconds(timeout),
            )
        }
        .map_err(call_error(EglOperation::ClientWaitSync))?;
        match status {
            egl::CONDITION_SATISFIED => Ok(SyncWaitStatus::Signaled),
            egl::TIMEOUT_EXPIRED => Ok(SyncWaitStatus::TimedOut),
            other => Err(EglCallError::new(
                EglOperation::ClientWaitSync,
                EglErrorCode::Other(other),
            )),
        }
    }

    fn destroy_sync(&self, display: DisplayId, sync: SyncId) -> Result<(), EglCallError> {
        // SAFETY: `sync` создан на этом `display` и разрушается ровно один раз (владелец —
        // `GpuFence`). Ожидающие в других потоках отпускаются драйвером (EGL 1.5, 3.8.1).
        unsafe {
            self.instance
                .destroy_sync(display_handle(display), sync_handle(sync))
        }
        .map_err(call_error(EglOperation::DestroySync))
    }

    fn query_dma_buf_formats(&self, display: DisplayId) -> Result<Vec<u32>, EglCallError> {
        self.query_dma_buf_formats_impl(display)
    }

    fn query_dma_buf_modifiers(
        &self,
        display: DisplayId,
        fourcc: u32,
    ) -> Result<Vec<DmaBufModifier>, EglCallError> {
        self.query_dma_buf_modifiers_impl(display, fourcc)
    }

    fn create_dma_buf_image(
        &self,
        display: DisplayId,
        attributes: &DmaBufImageAttributes,
    ) -> Result<ImageId, EglCallError> {
        self.create_dma_buf_image_checked(display, attributes)
    }

    fn destroy_image(&self, display: DisplayId, image: ImageId) -> Result<(), EglCallError> {
        self.destroy_image_checked(display, image)
    }
}

/// Почему не удалось загрузить функции GL для текущего контекста.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GlLoadError {
    /// Нет `glGetString` — контекст не текущий или драйвер сломан.
    MissingGetString,
    /// `glGetString(GL_VERSION)` вернул NULL: текущего контекста нет.
    VersionUnavailable,
    /// Строка версии не «OpenGL ES <major>.<minor>…».
    NotGles { version: String },
    /// `glGetString(GL_EXTENSIONS)` вернул NULL у контекста ES 2.x.
    ExtensionsUnavailable,
}

/// Функции GL текущего контекста.
pub(crate) struct LoadedGl {
    /// Таблица функций glow.
    pub(crate) gl: glow::Context,
    /// Строка `GL_VERSION`, как её отдал драйвер.
    pub(crate) version_string: String,
    /// Разобранная версия ES.
    pub(crate) version: super::gles_version::GlesVersion,
}

/// Расширение GL, дающее `glGetGraphicsResetStatus*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RobustnessExtension {
    /// `GL_KHR_robustness` → `glGetGraphicsResetStatusKHR`.
    Khr,
    /// `GL_EXT_robustness` → `glGetGraphicsResetStatusEXT`.
    Ext,
}

/// Вызов `glGetGraphicsResetStatus*` текущего контекста.
pub(crate) struct GraphicsResetStatusQuery {
    function: GlGetGraphicsResetStatusFn,
}

impl GraphicsResetStatusQuery {
    /// Код статуса сброса (`GL_NO_ERROR` — сброса не было).
    pub(crate) fn query(&self) -> u32 {
        // SAFETY: адрес выдан `eglGetProcAddress` для функции с сигнатурой из
        // `GL_EXT_robustness`/`GL_KHR_robustness`; вызов без аргументов. Контекст текущий в
        // этом потоке: `Gles2Context` не `Send` и опрашивает статус сразу после своего
        // `eglMakeCurrent`/`eglSwapBuffers`.
        unsafe { (self.function)() }
    }
}

impl SystemEgl {
    /// Загружает функции GL для контекста, ТЕКУЩЕГО в этом потоке.
    ///
    /// `glow::Context::from_loader_function*` паникует, если `GL_VERSION` пуст, не UTF-8 или не
    /// разбирается. Поэтому версию и расширения проверяем сами тем же правилом разбора
    /// (`gles_version::parse_like_glow`) и только потом строим glow.
    pub(crate) fn load_gl_for_current_context(&self) -> Result<LoadedGl, GlLoadError> {
        let get_string_address = self.proc_address("glGetString");
        if get_string_address.is_null() {
            return Err(GlLoadError::MissingGetString);
        }
        // SAFETY: адрес `glGetString` из `eglGetProcAddress`; сигнатура — GLES 2.0, раздел 6.1.5.
        let get_string: GlGetStringFn = unsafe { std::mem::transmute(get_string_address) };
        let version_string =
            gl_string(get_string, GL_VERSION).ok_or(GlLoadError::VersionUnavailable)?;
        let version = super::gles_version::parse_like_glow(&version_string).ok_or_else(|| {
            GlLoadError::NotGles {
                version: version_string.clone(),
            }
        })?;
        // До ES 3.0 glow читает расширения одной строкой `GL_EXTENSIONS` и упал бы на NULL.
        if version.major < 3 && gl_string(get_string, GL_EXTENSIONS).is_none() {
            return Err(GlLoadError::ExtensionsUnavailable);
        }

        // SAFETY: контекст текущий (проверено непустым `GL_VERSION`), версия разбирается тем же
        // правилом, что внутри glow, а `GL_EXTENSIONS` для ES 2.x не NULL — условия паники
        // конструктора glow исключены. Загрузчик возвращает адреса той же libEGL.
        let gl = unsafe {
            glow::Context::from_loader_function_cstr(|name| {
                name.to_str()
                    .map_or(std::ptr::null(), |name| self.proc_address(name))
            })
        };
        Ok(LoadedGl {
            gl,
            version_string,
            version,
        })
    }

    /// Функция статуса сброса для текущего контекста; `None` — драйвер её не дал.
    pub(crate) fn load_reset_status_query(
        &self,
        extension: RobustnessExtension,
    ) -> Option<GraphicsResetStatusQuery> {
        let function_name = match extension {
            RobustnessExtension::Khr => "glGetGraphicsResetStatusKHR",
            RobustnessExtension::Ext => "glGetGraphicsResetStatusEXT",
        };
        let address = self.proc_address(function_name);
        if address.is_null() {
            return None;
        }
        // SAFETY: адрес функции из `GL_EXT_robustness`/`GL_KHR_robustness` с сигнатурой
        // `GLenum (void)`, выдан той же libEGL.
        let function: GlGetGraphicsResetStatusFn = unsafe { std::mem::transmute(address) };
        Some(GraphicsResetStatusQuery { function })
    }
}

fn gl_string(get_string: GlGetStringFn, name: c_uint) -> Option<String> {
    // SAFETY: `glGetString` с допустимым именем строки возвращает NULL или указатель на
    // статическую строку драйвера с завершающим нулём (GLES 2.0, раздел 6.1.5).
    let raw = unsafe { get_string(name) };
    if raw.is_null() {
        return None;
    }
    // SAFETY: см. выше — строка с нулём в конце, живёт столько же, сколько контекст.
    let text = unsafe { CStr::from_ptr(raw.cast()) };
    // Не-UTF-8 строку glow не переживёт (`to_str().unwrap()` в конструкторе) — отказываем сами.
    text.to_str().ok().map(str::to_owned)
}
