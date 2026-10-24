//! Типизированные ошибки создания и работы контекста.
//!
//! Каждая причина отказа — свой вариант: вызывающий (каталог рендеров, сессия 09) по ним решает,
//! пробовать ли другой рендер и что сказать пользователю. Ничего не сливается в «ошибка EGL».

use crate::context::driver::{EglCallError, EglVersion};
use crate::context::platform::UnsupportedWindowSystem;
use crate::loss::ContextLoss;

/// Почему контекст GLES2 не создан.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CreateError {
    /// libEGL нет в системе или она не открылась.
    #[error("libEGL is unavailable: {message}")]
    EglUnavailable {
        /// Текст загрузчика библиотек.
        message: String,
    },
    /// libEGL или display старше EGL 1.5 (минимум, решение владельца 03).
    #[error("EGL {found} is older than the required 1.5")]
    EglVersionUnsupported {
        /// Что нашли.
        found: EglVersion,
    },
    /// Окно не отдало дескрипторы (`raw-window-handle`).
    #[error("window handle is unavailable: {message}")]
    WindowHandleUnavailable {
        /// Текст ошибки оконной системы.
        message: String,
    },
    /// Оконная система не поддерживается (не Wayland и не Xlib).
    #[error(transparent)]
    UnsupportedWindowSystem(#[from] UnsupportedWindowSystem),
    /// libEGL не умеет нужную платформу (`EGL_KHR_platform_wayland`, `_x11`,
    /// `EGL_MESA_platform_surfaceless`).
    #[error("EGL platform extension {extension} is missing")]
    PlatformExtensionMissing {
        /// Первое из равноценных расширений платформы.
        extension: &'static str,
    },
    /// `eglGetPlatformDisplay`/`eglInitialize`/`eglQueryString` display отказали.
    #[error("EGL display could not be opened: {0}")]
    DisplayOpen(EglCallError),
    /// `eglBindAPI(EGL_OPENGL_ES_API)` отказал: libEGL без OpenGL ES.
    #[error("OpenGL ES API is unavailable in EGL: {0}")]
    GlesApiUnavailable(EglCallError),
    /// Запрос конфигов отказал.
    #[error("EGL config query failed: {0}")]
    ConfigQuery(EglCallError),
    /// Ни одного конфига с OpenGL ES 2 и RGB 8 бит: драйвер не умеет GLES2.
    #[error("the driver offers no OpenGL ES 2 config")]
    NoGles2Config,
    /// Ни один конфиг не подходит к visual окна X11.
    #[error("no EGL config matches X11 window visual {visual_id:#x}")]
    NoConfigForWindowVisual {
        /// Visual окна.
        visual_id: i64,
    },
    /// `eglCreateContext` отказал.
    #[error("EGL context creation failed: {0}")]
    ContextCreation(EglCallError),
    /// Не создалась поверхность окна (`wl_egl_window` или `eglCreatePlatformWindowSurface`)
    /// или pbuffer для привязки offscreen-контекста.
    #[error("EGL surface creation failed: {0}")]
    SurfaceCreation(EglCallError),
    /// `eglMakeCurrent` отказал.
    #[error("EGL context could not be made current: {0}")]
    MakeCurrent(EglCallError),
    /// Начальные настройки показа (`eglSwapInterval`) не применились.
    #[error("EGL presentation settings could not be applied: {0}")]
    PresentationSetup(EglCallError),
    /// Контекст не дал функций GL или это не OpenGL ES 2.0+.
    #[error("OpenGL ES functions could not be loaded: {reason}")]
    GlLoad {
        /// Человекочитаемая причина.
        reason: String,
    },
    /// Offscreen-цель (FBO + текстура) не собралась.
    #[error("offscreen render target could not be created: {reason}")]
    OffscreenTarget {
        /// Что ответил GL.
        reason: String,
    },
    /// Контекст потерян прямо во время создания.
    #[error("GLES2 context lost during creation: {0}")]
    ContextLost(ContextLoss),
}

/// Отказ операции работающего контекста.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Gles2Error {
    /// Контекст потерян: все дальнейшие операции тоже ответят этим. Нужен новый контекст.
    #[error("GLES2 context lost: {0}")]
    ContextLost(ContextLoss),
    /// Отказ драйвера EGL.
    #[error("EGL call failed: {0}")]
    Driver(EglCallError),
    /// Отказ GL (`glGetError` или ответ функции).
    #[error("GL operation {operation} failed: {reason}")]
    Gl {
        /// Что делали.
        operation: &'static str,
        /// Что ответил GL.
        reason: String,
    },
}

impl Gles2Error {
    /// Ошибка EGL; `EGL_CONTEXT_LOST` превращается в [`Gles2Error::ContextLost`].
    pub(crate) fn from_egl(error: EglCallError) -> Self {
        if error.is_context_lost() {
            Self::ContextLost(ContextLoss::from_egl())
        } else {
            Self::Driver(error)
        }
    }
}

impl Gles2Error {
    /// Отказ доступа к GL в терминах операций контекста.
    pub(crate) fn from_gl_access(error: crate::GlAccessError) -> Self {
        match error {
            crate::GlAccessError::ContextLost(loss) => Self::ContextLost(loss),
            crate::GlAccessError::MakeCurrent(error) => Self::from_egl(error),
            crate::GlAccessError::DetachedWithoutSurfacelessContext => Self::Gl {
                operation: "make_current",
                reason: error.to_string(),
            },
        }
    }
}

impl From<ContextLoss> for Gles2Error {
    fn from(loss: ContextLoss) -> Self {
        Self::ContextLost(loss)
    }
}
