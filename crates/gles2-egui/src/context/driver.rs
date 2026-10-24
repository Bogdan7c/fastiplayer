//! Контракт «что мы просим у EGL».
//!
//! Весь остальной crate работает с EGL только через [`EglDriver`]. Настоящая реализация живёт
//! в `safety.rs` (единственное место с `unsafe`), а тесты подставляют fake-драйвер с журналом
//! вызовов. Так порядок разрушения объектов, подсчёт пользователей display и ошибки создания
//! проверяются без GPU и без окна.
//!
//! Дескрипторы EGL здесь — непрозрачные числа (`usize`), а не сырые указатели. Указателями они
//! становятся только внутри `safety.rs`. Поэтому типы ниже `Send + Sync`, и fence можно ждать
//! из потока плеера.

use std::fmt;
use std::time::Duration;

/// Дескриптор `EGLDisplay`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct DisplayId(pub(crate) usize);

/// Дескриптор `EGLConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ConfigId(pub(crate) usize);

/// Дескриптор `EGLContext`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ContextId(pub(crate) usize);

/// Дескриптор `EGLSurface`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SurfaceId(pub(crate) usize);

/// Дескриптор `EGLSync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SyncId(pub(crate) usize);

/// Дескриптор `wl_egl_window` (обёртка libwayland-egl над `wl_surface`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct WaylandEglWindowId(pub(crate) usize);

/// Версия EGL (`eglInitialize`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EglVersion {
    /// Старшая часть версии.
    pub major: u32,
    /// Младшая часть версии.
    pub minor: u32,
}

impl fmt::Display for EglVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

/// Нативный display, на котором открывается `EGLDisplay`
/// (`eglGetPlatformDisplay`, EGL 1.5, раздел 3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum NativeDisplay {
    /// `EGL_PLATFORM_WAYLAND_KHR`: адрес `wl_display` окна.
    Wayland { display_address: usize },
    /// `EGL_PLATFORM_X11_KHR`: адрес Xlib `Display`; `None` — EGL открывает своё соединение
    /// (`EGL_DEFAULT_DISPLAY`).
    X11 { display_address: Option<usize> },
    /// `EGL_PLATFORM_SURFACELESS_MESA`: без оконной системы (стенд тестов и offscreen-цель).
    Surfaceless,
}

/// Во что EGL рисует на окне (`eglCreatePlatformWindowSurface`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlatformWindowTarget {
    /// Wayland: созданный нами `wl_egl_window`.
    Wayland(WaylandEglWindowId),
    /// X11: идентификатор окна (XID, тип `Window` из Xlib). По `EGL_KHR_platform_x11` EGL
    /// получает указатель на него — это делает `safety.rs`.
    X11 { window_xid: std::os::raw::c_ulong },
}

/// Размер поверхности в пикселях. Оба измерения не меньше 1 — EGL и `wl_egl_window` не
/// принимают пустые буферы.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurfaceSize {
    width: u32,
    height: u32,
}

impl SurfaceSize {
    /// 1×1: pbuffer только ради привязки offscreen-контекста.
    pub(crate) const ONE_PIXEL: Self = Self {
        width: 1,
        height: 1,
    };

    /// Размер, если оба измерения больше нуля. Свёрнутое окно (0×0) — `None`: такой кадр
    /// рисовать некуда.
    pub const fn new(width: u32, height: u32) -> Option<Self> {
        if width == 0 || height == 0 {
            None
        } else {
            Some(Self { width, height })
        }
    }

    /// Ширина в пикселях (≥ 1).
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Высота в пикселях (≥ 1).
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Ширина для FFI EGL/GL (`EGLint`/`GLsizei`). Значения больше `i32::MAX` прижимаются:
    /// такой размер драйвер всё равно отклонит своей ошибкой, а мы не получим переполнение.
    pub(crate) fn width_i32(self) -> i32 {
        i32::try_from(self.width).unwrap_or(i32::MAX)
    }

    /// Высота для FFI EGL/GL, правило как у [`Self::width_i32`].
    pub(crate) fn height_i32(self) -> i32 {
        i32::try_from(self.height).unwrap_or(i32::MAX)
    }
}

/// Что сделать текущим в потоке (`eglMakeCurrent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CurrentBinding {
    /// Контекст рисует в surface (окно или pbuffer).
    Surface {
        context: ContextId,
        surface: SurfaceId,
    },
    /// Контекст без surface (`EGL_KHR_surfaceless_context`): загрузки текстур и FBO работают,
    /// показывать нечего.
    Surfaceless { context: ContextId },
    /// Отвязать всё (`EGL_NO_CONTEXT`).
    Nothing,
}

/// Строка display (`eglQueryString`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisplayString {
    /// `EGL_VENDOR`.
    Vendor,
    /// `EGL_EXTENSIONS`.
    Extensions,
}

/// Атрибуты EGL-конфига, которые нужны выбору (`eglGetConfigAttrib`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfigDescription {
    /// Сам конфиг.
    pub(crate) id: ConfigId,
    /// `EGL_RED_SIZE`.
    pub(crate) red_bits: i32,
    /// `EGL_GREEN_SIZE`.
    pub(crate) green_bits: i32,
    /// `EGL_BLUE_SIZE`.
    pub(crate) blue_bits: i32,
    /// `EGL_ALPHA_SIZE`: 0 — окно без прозрачности.
    pub(crate) alpha_bits: i32,
    /// `EGL_NATIVE_VISUAL_ID`: на X11 конфиг обязан совпадать с visual окна.
    pub(crate) native_visual_id: i64,
    /// `EGL_MIN_SWAP_INTERVAL`.
    pub(crate) min_swap_interval: i32,
    /// `EGL_MAX_SWAP_INTERVAL`.
    pub(crate) max_swap_interval: i32,
}

/// Какой конфиг просим у `eglChooseConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigSurfaceKind {
    /// `EGL_WINDOW_BIT`: показ в окно.
    Window,
    /// Без требований к типу surface: offscreen-контекст рисует в свой FBO и привязывается
    /// без surface (`EGL_KHR_surfaceless_context`).
    Surfaceless,
    /// `EGL_PBUFFER_BIT`: offscreen-контекст без `EGL_KHR_surfaceless_context` привязывается
    /// к pbuffer 1×1.
    Pbuffer,
}

/// Запрос к `eglChooseConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfigQuery {
    /// Тип surface.
    pub(crate) surface_kind: ConfigSurfaceKind,
    /// Минимум бит альфы: 8 — нужна прозрачность окна, 0 — любая.
    pub(crate) minimum_alpha_bits: i32,
}

/// Атрибуты создания контекста.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContextRequest {
    /// Просить `EGL_LOSE_CONTEXT_ON_RESET_EXT` (`EGL_EXT_create_context_robustness`): после сброса
    /// GPU драйвер сообщает о потере контекста, а не продолжает молча.
    pub(crate) reset_notification: ResetNotificationRequest,
}

/// Нужна ли стратегия уведомления о сбросе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResetNotificationRequest {
    /// `EGL_LOSE_CONTEXT_ON_RESET_EXT`.
    LoseContextOnReset,
    /// Без атрибута (драйвер без `EGL_EXT_create_context_robustness`).
    NotRequested,
}

/// Исход `eglClientWaitSync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncWaitStatus {
    /// `EGL_CONDITION_SATISFIED`: GPU прошёл точку fence.
    Signaled,
    /// `EGL_TIMEOUT_EXPIRED`: ещё не прошёл.
    TimedOut,
}

/// Один модификатор формата DMA-BUF (`eglQueryDmaBufModifiersEXT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmaBufModifier {
    /// Модификатор DRM (`DRM_FORMAT_MOD_*`), 0 — `LINEAR`.
    pub modifier: u64,
    /// `true` — импорт только как `GL_TEXTURE_EXTERNAL_OES` (не обычный `sampler2D`).
    pub external_only: bool,
}

/// Операция EGL — для текста ошибок и журнала fake-драйвера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EglOperation {
    /// `eglGetPlatformDisplay`.
    GetPlatformDisplay,
    /// `eglInitialize`.
    Initialize,
    /// `eglTerminate`.
    Terminate,
    /// `eglQueryString`.
    QueryString,
    /// `eglBindAPI(EGL_OPENGL_ES_API)`.
    BindApi,
    /// `eglChooseConfig` + `eglGetConfigAttrib`.
    ChooseConfig,
    /// `eglCreateContext`.
    CreateContext,
    /// `eglDestroyContext`.
    DestroyContext,
    /// `wl_egl_window_create`.
    CreateWaylandEglWindow,
    /// `eglCreatePlatformWindowSurface`.
    CreateWindowSurface,
    /// `eglCreatePbufferSurface`.
    CreatePbufferSurface,
    /// `eglDestroySurface`.
    DestroySurface,
    /// `eglMakeCurrent`.
    MakeCurrent,
    /// `eglSwapBuffers`.
    SwapBuffers,
    /// `eglSwapInterval`.
    SwapInterval,
    /// `eglCreateSync(EGL_SYNC_FENCE)`.
    CreateSync,
    /// `eglClientWaitSync`.
    ClientWaitSync,
    /// `eglDestroySync`.
    DestroySync,
    /// `eglQueryDmaBufFormatsEXT` / `eglQueryDmaBufModifiersEXT`.
    QueryDmaBuf,
}

/// Код ошибки EGL (`eglGetError`, EGL 1.5, раздел 3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EglErrorCode {
    /// `EGL_CONTEXT_LOST`: сброс GPU или событие питания, контекст надо пересоздать.
    ContextLost,
    /// `EGL_BAD_ALLOC`.
    BadAlloc,
    /// `EGL_BAD_MATCH`.
    BadMatch,
    /// `EGL_BAD_CONFIG`.
    BadConfig,
    /// `EGL_BAD_ATTRIBUTE`.
    BadAttribute,
    /// `EGL_BAD_NATIVE_WINDOW`.
    BadNativeWindow,
    /// `EGL_BAD_SURFACE`.
    BadSurface,
    /// `EGL_NOT_INITIALIZED`.
    NotInitialized,
    /// Другой код (число из спецификации).
    Other(i32),
    /// Драйвер вернул отказ, но `eglGetError` ничего не сообщил, либо нужной функции
    /// расширения нет в библиотеке.
    Unreported,
}

/// Отказ одного вызова EGL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{operation:?} failed: {code:?}")]
pub struct EglCallError {
    /// Что вызывали.
    pub operation: EglOperation,
    /// Что ответил драйвер.
    pub code: EglErrorCode,
}

impl EglCallError {
    /// Ошибка с кодом драйвера.
    pub(crate) const fn new(operation: EglOperation, code: EglErrorCode) -> Self {
        Self { operation, code }
    }

    /// `true`, если драйвер сообщил о потере контекста.
    pub(crate) const fn is_context_lost(self) -> bool {
        matches!(self.code, EglErrorCode::ContextLost)
    }
}

/// Всё, что crate просит у EGL. Реализации: `safety::SystemEgl` (настоящая libEGL) и fake-драйвер
/// тестов.
///
/// Инвариант реализаций: метод `destroy_*`/`terminate` для одного дескриптора вызывается не
/// больше одного раза — это держат RAII-владельцы (`egl_objects.rs`, `display_registry.rs`,
/// `fence.rs`), а fake-драйвер тестов падает на повторном разрушении.
pub(crate) trait EglDriver: Send + Sync {
    /// Клиентские расширения (`eglQueryString(EGL_NO_DISPLAY, EGL_EXTENSIONS)`); пусто, если
    /// библиотека их не сообщает.
    fn client_extensions(&self) -> String;
    /// `eglGetPlatformDisplay`.
    fn get_platform_display(&self, native: NativeDisplay) -> Result<DisplayId, EglCallError>;
    /// `eglInitialize`.
    fn initialize(&self, display: DisplayId) -> Result<EglVersion, EglCallError>;
    /// `eglTerminate`.
    fn terminate(&self, display: DisplayId) -> Result<(), EglCallError>;
    /// `eglQueryString` для display.
    fn query_display_string(
        &self,
        display: DisplayId,
        which: DisplayString,
    ) -> Result<String, EglCallError>;
    /// `eglBindAPI(EGL_OPENGL_ES_API)` для текущего потока.
    fn bind_gles_api(&self) -> Result<(), EglCallError>;
    /// Конфиги с GLES2, RGB 8 бит и запрошенной альфой, с атрибутами для выбора.
    fn matching_configs(
        &self,
        display: DisplayId,
        query: ConfigQuery,
    ) -> Result<Vec<ConfigDescription>, EglCallError>;
    /// `eglCreateContext` с `EGL_CONTEXT_CLIENT_VERSION = 2`.
    fn create_context(
        &self,
        display: DisplayId,
        config: ConfigId,
        request: ContextRequest,
    ) -> Result<ContextId, EglCallError>;
    /// `eglDestroyContext`.
    fn destroy_context(&self, display: DisplayId, context: ContextId) -> Result<(), EglCallError>;
    /// `wl_egl_window_create` поверх `wl_surface` по адресу.
    fn create_wayland_egl_window(
        &self,
        wl_surface_address: usize,
        size: SurfaceSize,
    ) -> Result<WaylandEglWindowId, EglCallError>;
    /// `wl_egl_window_resize` (dx = dy = 0).
    fn resize_wayland_egl_window(&self, window: WaylandEglWindowId, size: SurfaceSize);
    /// `wl_egl_window_destroy`. Вызывается только после уничтожения EGLSurface поверх окна.
    fn destroy_wayland_egl_window(&self, window: WaylandEglWindowId);
    /// `eglCreatePlatformWindowSurface`.
    fn create_window_surface(
        &self,
        display: DisplayId,
        config: ConfigId,
        target: PlatformWindowTarget,
    ) -> Result<SurfaceId, EglCallError>;
    /// `eglCreatePbufferSurface` (запасная привязка контекста без `EGL_KHR_surfaceless_context`).
    fn create_pbuffer_surface(
        &self,
        display: DisplayId,
        config: ConfigId,
        size: SurfaceSize,
    ) -> Result<SurfaceId, EglCallError>;
    /// `eglDestroySurface`.
    fn destroy_surface(&self, display: DisplayId, surface: SurfaceId) -> Result<(), EglCallError>;
    /// `eglMakeCurrent`.
    fn make_current(&self, display: DisplayId, binding: CurrentBinding)
    -> Result<(), EglCallError>;
    /// `eglSwapBuffers`.
    fn swap_buffers(&self, display: DisplayId, surface: SurfaceId) -> Result<(), EglCallError>;
    /// `eglSwapInterval` для surface, текущей в этом потоке.
    fn swap_interval(&self, display: DisplayId, interval: i32) -> Result<(), EglCallError>;
    /// `eglCreateSync(EGL_SYNC_FENCE)` в командном потоке текущего контекста.
    fn create_fence_sync(&self, display: DisplayId) -> Result<SyncId, EglCallError>;
    /// `eglClientWaitSync` без флага сброса команд (их сбрасывает создатель fence).
    fn client_wait_sync(
        &self,
        display: DisplayId,
        sync: SyncId,
        timeout: Duration,
    ) -> Result<SyncWaitStatus, EglCallError>;
    /// `eglDestroySync`.
    fn destroy_sync(&self, display: DisplayId, sync: SyncId) -> Result<(), EglCallError>;
    /// `eglQueryDmaBufFormatsEXT`: fourcc форматов импорта.
    fn query_dma_buf_formats(&self, display: DisplayId) -> Result<Vec<u32>, EglCallError>;
    /// `eglQueryDmaBufModifiersEXT` для одного формата.
    fn query_dma_buf_modifiers(
        &self,
        display: DisplayId,
        fourcc: u32,
    ) -> Result<Vec<DmaBufModifier>, EglCallError>;
}
