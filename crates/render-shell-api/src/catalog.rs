//! Словарь каталога рендеров платформы.
//!
//! Сам каталог (упорядоченный список кандидатов, чистая функция выбора, фабрики) живёт в
//! composition root приложения (`app-egui::render_backend`, решение владельца в сессии 01):
//! он знает конкретные рендеры. Здесь — только общие слова: «какое семейство», «годен ли и
//! почему нет», «почему не создался». На Windows словарь не меняется, добавляются семейства.

use std::fmt;

use render_core::RenderBackendKind;

/// Семейство рендера: одна реализация контракта под один GPU-API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RendererFamily {
    /// wgpu поверх Vulkan (Linux).
    WgpuVulkan,

    /// Свой рендер OpenGL ES 2.0 через EGL (старое железо без Vulkan, issue #6).
    Gles2,
}

impl RendererFamily {
    /// Стабильный идентификатор для конфигурации, логов и метки «запуск не завершён».
    #[must_use]
    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::WgpuVulkan => "wgpu-vulkan",
            Self::Gles2 => "gles2",
        }
    }

    /// Класс рендера в нейтральных возможностях (`RenderCapabilities::backend`).
    #[must_use]
    pub const fn backend_kind(self) -> RenderBackendKind {
        match self {
            Self::WgpuVulkan => RenderBackendKind::Wgpu,
            Self::Gles2 => RenderBackendKind::OpenGles,
        }
    }
}

/// Итог проверки кандидата каталога до создания.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendererProbe {
    /// Кандидат годен: его можно создавать.
    Usable,

    /// Кандидат не годен; причину видит пользователь в уведомлении о фолбэке.
    Unusable {
        /// Почему не годен.
        reason: RendererUnusableReason,
    },
}

/// Почему кандидат каталога не годен.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendererUnusableReason {
    /// В системе нет аппаратного адаптера под этот GPU-API (например, нет Vulkan-устройств).
    NoHardwareAdapter,

    /// Есть только программный адаптер (lavapipe/llvmpipe): играть через CPU-растеризатор
    /// дороже, чем через другой рендер на аппаратном GPU.
    SoftwareAdapterOnly,

    /// Драйверу не хватает обязательной возможности (расширения, формата, точности).
    MissingRequiredFeature(RequiredRendererFeature),

    /// Прошлый запуск с этим рендером не дошёл до первого показанного кадра.
    PreviousStartupCrashed,

    /// Кандидат прошёл проверку, но создание упало.
    CreationFailed(RendererCreateError),
}

/// Обязательная возможность драйвера, которой не нашлось.
///
/// Имя — строка из спецификации (например, `EGL_KHR_surfaceless_context` или
/// `VK_EXT_external_memory_dma_buf`): по ней пользователь и разработчик ищут причину.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredRendererFeature(String);

impl RequiredRendererFeature {
    /// Возможность по её имени из спецификации API.
    #[must_use]
    pub fn new(specification_name: impl Into<String>) -> Self {
        Self(specification_name.into())
    }

    /// Имя возможности из спецификации.
    #[must_use]
    pub fn specification_name(&self) -> &str {
        &self.0
    }
}

/// Ошибка создания рендера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererCreateError {
    /// На каком шаге создание упало.
    pub stage: RendererCreateStage,

    /// Сообщение рендера/драйвера для логов и уведомления.
    pub message: String,
}

impl RendererCreateError {
    /// Ошибка на шаге создания с сообщением драйвера.
    #[must_use]
    pub fn new(stage: RendererCreateStage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
        }
    }
}

impl fmt::Display for RendererCreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.stage, self.message)
    }
}

impl std::error::Error for RendererCreateError {}

/// Шаг создания рендера, на котором случилась ошибка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererCreateStage {
    /// Подключение к API (Vulkan instance, EGL display).
    ApiConnection,

    /// Выбор адаптера/устройства.
    AdapterSelection,

    /// Surface окна (swapchain, EGL window surface).
    SurfaceCreation,

    /// Логическое устройство или GL-контекст.
    DeviceCreation,

    /// Шейдеры и пайплайны рендера.
    PipelineCreation,
}
