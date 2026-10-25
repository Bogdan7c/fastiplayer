//! Перевод ошибок создания контекста GLES2 в нейтральный словарь каталога рендеров.
//!
//! Каталог (`app-egui::render_backend`) знает только [`RendererCreateError`] со стадией и
//! сообщением и [`RendererUnusableReason`] для пробы. Перевод — таблица в одном месте, чтобы
//! стадия не зависела от того, кто создаёт рендер (проба, окно, рендер без окна).

use gles2_egui::{CreateError, MissingRequirement};
use render_shell_api::{
    RendererCreateError, RendererCreateStage, RendererUnusableReason, RequiredRendererFeature,
};

/// Стадия создания, на которой возникла ошибка контекста.
#[must_use]
pub(crate) fn create_stage_of(error: &CreateError) -> RendererCreateStage {
    match error {
        // Не нашли или не смогли открыть сам API: библиотека EGL, её версия, display, платформа,
        // загрузка функций GLES.
        CreateError::EglUnavailable { .. }
        | CreateError::EglVersionUnsupported { .. }
        | CreateError::PlatformExtensionMissing { .. }
        | CreateError::DisplayOpen(_)
        | CreateError::GlesApiUnavailable(_)
        | CreateError::GlLoad { .. } => RendererCreateStage::ApiConnection,
        // Выбор конфигурации EGL — аналог выбора адаптера.
        CreateError::ConfigQuery(_) | CreateError::NoGles2Config => {
            RendererCreateStage::AdapterSelection
        }
        // Цель показа: окно, его visual, offscreen-буфер.
        CreateError::WindowHandleUnavailable { .. }
        | CreateError::UnsupportedWindowSystem(_)
        | CreateError::NoConfigForWindowVisual { .. }
        | CreateError::SurfaceCreation(_)
        | CreateError::OffscreenTarget { .. } => RendererCreateStage::SurfaceCreation,
        // Контекст GLES2: создание, привязка, настройки показа, потеря в процессе.
        CreateError::ContextCreation(_)
        | CreateError::MakeCurrent(_)
        | CreateError::PresentationSetup(_)
        | CreateError::ContextLost(_) => RendererCreateStage::DeviceCreation,
    }
}

/// Ошибка создания рендера из ошибки контекста.
#[must_use]
pub(crate) fn renderer_create_error(error: &CreateError) -> RendererCreateError {
    RendererCreateError::new(create_stage_of(error), error.to_string())
}

/// Ошибка сборки программ и буферов рендера на уже созданном контексте.
#[must_use]
pub(crate) fn pipeline_creation_error(
    what: &str,
    error: &dyn std::fmt::Display,
) -> RendererCreateError {
    RendererCreateError::new(
        RendererCreateStage::PipelineCreation,
        format!("{what}: {error}"),
    )
}

/// Почему проба считает рендер негодным по ошибке контекста.
///
/// Нехватка версии EGL и отсутствие конфигурации GLES2 — это «нет требуемой возможности»
/// (каталог показывает её имя пользователю); остальное — отказ создания со стадией.
#[must_use]
pub(crate) fn unusable_reason_for(error: &CreateError) -> RendererUnusableReason {
    match error {
        CreateError::EglVersionUnsupported { .. } => {
            RendererUnusableReason::MissingRequiredFeature(RequiredRendererFeature::new("EGL 1.5"))
        }
        CreateError::NoGles2Config => RendererUnusableReason::MissingRequiredFeature(
            RequiredRendererFeature::new("EGL config with EGL_OPENGL_ES2_BIT"),
        ),
        other => RendererUnusableReason::CreationFailed(renderer_create_error(other)),
    }
}

/// Имя нехватающей возможности из спецификации для списка требований уровня.
#[must_use]
pub(crate) fn required_feature_for(missing: &MissingRequirement) -> RequiredRendererFeature {
    let name = match missing {
        MissingRequirement::GlesVersion { .. } => "OpenGL ES 2.0".to_owned(),
        MissingRequirement::EglVersion { .. } => "EGL 1.5".to_owned(),
        MissingRequirement::HighPrecisionFragmentFloat => {
            "highp float in fragment shaders (GLSL ES 1.00, GL_FRAGMENT_PRECISION_HIGH)".to_owned()
        }
        MissingRequirement::GlExtension(extension)
        | MissingRequirement::EglExtension(extension) => (*extension).to_owned(),
        MissingRequirement::DmaBufFormat { fourcc, .. } => {
            format!("DMA-BUF sampler2D import of {fourcc:?}")
        }
    };
    RequiredRendererFeature::new(name)
}

#[cfg(test)]
mod tests;
