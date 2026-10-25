//! Таблица перевода ошибок создания: стадия и причина «негоден» для каждого варианта.

use gles2_egui::{
    ContextLoss, ContextLossDetection, ContextLossReason, EglCallError, EglErrorCode, EglOperation,
    EglVersion, UnsupportedWindowSystem,
};

use super::*;

fn driver_error() -> EglCallError {
    EglCallError {
        operation: EglOperation::MakeCurrent,
        code: EglErrorCode::BadAlloc,
    }
}

fn context_loss() -> ContextLoss {
    ContextLoss {
        reason: ContextLossReason::EglContextLost,
        detection: ContextLossDetection::EglCall,
    }
}

/// Каждый вариант ошибки создания попадает в свою стадию каталога.
#[test]
fn every_create_error_lands_on_its_catalog_stage() {
    use RendererCreateStage::{AdapterSelection, ApiConnection, DeviceCreation, SurfaceCreation};
    let cases: Vec<(CreateError, RendererCreateStage)> = vec![
        (
            CreateError::EglUnavailable {
                message: "no libEGL".into(),
            },
            ApiConnection,
        ),
        (
            CreateError::EglVersionUnsupported {
                found: EglVersion { major: 1, minor: 4 },
            },
            ApiConnection,
        ),
        (
            CreateError::PlatformExtensionMissing {
                extension: "EGL_KHR_platform_wayland",
            },
            ApiConnection,
        ),
        (CreateError::DisplayOpen(driver_error()), ApiConnection),
        (
            CreateError::GlesApiUnavailable(driver_error()),
            ApiConnection,
        ),
        (
            CreateError::GlLoad {
                reason: "missing glGetString".into(),
            },
            ApiConnection,
        ),
        (CreateError::ConfigQuery(driver_error()), AdapterSelection),
        (CreateError::NoGles2Config, AdapterSelection),
        (
            CreateError::WindowHandleUnavailable {
                message: "no handle".into(),
            },
            SurfaceCreation,
        ),
        (
            CreateError::NoConfigForWindowVisual { visual_id: 0x21 },
            SurfaceCreation,
        ),
        (
            CreateError::SurfaceCreation(driver_error()),
            SurfaceCreation,
        ),
        (
            CreateError::OffscreenTarget {
                reason: "fbo incomplete".into(),
            },
            SurfaceCreation,
        ),
        (CreateError::ContextCreation(driver_error()), DeviceCreation),
        (CreateError::MakeCurrent(driver_error()), DeviceCreation),
        (
            CreateError::PresentationSetup(driver_error()),
            DeviceCreation,
        ),
        (CreateError::ContextLost(context_loss()), DeviceCreation),
    ];
    for (error, expected_stage) in cases {
        let translated = renderer_create_error(&error);
        assert_eq!(translated.stage, expected_stage, "{error:?}");
        assert_eq!(
            translated.message,
            error.to_string(),
            "the message of {error:?} must be kept for the user-facing report"
        );
    }
}

/// Оконная система, которой нет в таблице (не Wayland/X11), — отказ на стадии цели показа.
#[test]
fn unsupported_window_system_is_a_surface_stage_error() {
    let error = CreateError::UnsupportedWindowSystem(UnsupportedWindowSystem {
        display: "Windows",
        window: "Win32",
    });
    assert_eq!(
        create_stage_of(&error),
        RendererCreateStage::SurfaceCreation
    );
}

/// Проба называет нехватку версии EGL и конфигурации GLES2 возможностью, остальное — отказом
/// создания с той же стадией.
#[test]
fn probe_names_missing_features_and_keeps_other_failures_typed() {
    let old_egl = CreateError::EglVersionUnsupported {
        found: EglVersion { major: 1, minor: 4 },
    };
    match unusable_reason_for(&old_egl) {
        RendererUnusableReason::MissingRequiredFeature(feature) => {
            assert_eq!(feature.specification_name(), "EGL 1.5");
        }
        other => panic!("old EGL must be a missing feature, got {other:?}"),
    }
    match unusable_reason_for(&CreateError::NoGles2Config) {
        RendererUnusableReason::MissingRequiredFeature(feature) => {
            assert!(feature.specification_name().contains("EGL_OPENGL_ES2_BIT"));
        }
        other => panic!("no GLES2 config must be a missing feature, got {other:?}"),
    }
    let no_library = CreateError::EglUnavailable {
        message: "no libEGL".into(),
    };
    match unusable_reason_for(&no_library) {
        RendererUnusableReason::CreationFailed(failure) => {
            assert_eq!(failure.stage, RendererCreateStage::ApiConnection);
            assert!(failure.message.contains("no libEGL"), "{failure:?}");
        }
        other => panic!("a missing library is a creation failure, got {other:?}"),
    }
}

/// Нехватка требования уровня получает имя из спецификации, а не внутренний `Debug`.
#[test]
fn missing_requirement_names_follow_the_specifications() {
    let cases = [
        (
            MissingRequirement::GlExtension("GL_OES_EGL_image"),
            "GL_OES_EGL_image",
        ),
        (
            MissingRequirement::EglExtension("EGL_KHR_fence_sync"),
            "EGL_KHR_fence_sync",
        ),
        (
            MissingRequirement::EglVersion {
                found: EglVersion { major: 1, minor: 4 },
            },
            "EGL 1.5",
        ),
    ];
    for (missing, expected) in cases {
        assert_eq!(
            required_feature_for(&missing).specification_name(),
            expected
        );
    }
    assert!(
        required_feature_for(&MissingRequirement::HighPrecisionFragmentFloat)
            .specification_name()
            .contains("highp")
    );
}
