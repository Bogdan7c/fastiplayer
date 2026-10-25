//! Перевод настроек показа и модель отпуска окна рендера без окна.

use gles2_egui::{
    ContextLoss, ContextLossDetection, ContextLossReason, EglCallError, EglErrorCode, EglOperation,
};

use super::*;

fn driver_error() -> EglCallError {
    EglCallError {
        operation: EglOperation::MakeCurrent,
        code: EglErrorCode::BadSurface,
    }
}

fn lost() -> ContextLoss {
    ContextLoss {
        reason: ContextLossReason::EglContextLost,
        detection: ContextLossDetection::EglCall,
    }
}

/// Режимы показа контракта → вертикальная развёртка EGL; `Mailbox` заменяется честно.
#[test]
fn present_modes_map_to_vsync_and_mailbox_is_substituted() {
    assert_eq!(
        vsync_for(ShellPresentMode::Auto),
        (VsyncMode::WaitForVblank, PresentModeFidelity::AsRequested)
    );
    assert_eq!(
        vsync_for(ShellPresentMode::Fifo),
        (VsyncMode::WaitForVblank, PresentModeFidelity::AsRequested)
    );
    assert_eq!(
        vsync_for(ShellPresentMode::Immediate),
        (VsyncMode::Immediate, PresentModeFidelity::AsRequested)
    );
    assert_eq!(
        vsync_for(ShellPresentMode::Mailbox),
        (VsyncMode::WaitForVblank, PresentModeFidelity::Substituted)
    );
}

/// Прозрачность и задержка: прозрачность уходит в запрос окна, задержка в настройки контекста
/// не попадает вовсе (EGL её не управляет).
#[test]
fn context_settings_carry_vsync_and_alpha_but_never_the_frame_latency() {
    let shallow = SurfacePresentSettings {
        present_mode: ShellPresentMode::Immediate,
        max_frame_latency: 1,
        alpha_preference: SurfaceAlphaPreference::TransparentPreferred,
    };
    let deep = SurfacePresentSettings {
        max_frame_latency: 7,
        ..shallow
    };
    let settings = context_settings_for(shallow);
    assert_eq!(settings.vsync, VsyncMode::Immediate);
    assert_eq!(settings.alpha, WindowAlphaRequest::TransparentPreferred);
    assert_eq!(settings, context_settings_for(deep));
    assert_eq!(
        context_settings_for(SurfacePresentSettings::default()).alpha,
        WindowAlphaRequest::Opaque
    );
}

/// Отказы контекста при перенастройке различаются: нет окна, прозрачность, прежние настройки
/// сохранены, показ потерян.
#[test]
fn reconfigure_errors_keep_their_distinct_meaning() {
    assert_eq!(
        reconfigure_error(PresentationError::Detached),
        PresentationReconfigureError::PresentationDetached
    );
    assert_eq!(
        reconfigure_error(PresentationError::NotAWindowTarget),
        PresentationReconfigureError::PresentationDetached
    );
    assert_eq!(
        reconfigure_error(PresentationError::AlphaChangeRequiresRecreation),
        PresentationReconfigureError::AlphaPreferenceChangeUnsupported
    );
    assert!(matches!(
        reconfigure_error(PresentationError::Driver(driver_error())),
        PresentationReconfigureError::RejectedPreviousRestored { .. }
    ));
    assert!(matches!(
        reconfigure_error(PresentationError::SurfaceCreation(driver_error())),
        PresentationReconfigureError::PresentationLost { .. }
    ));
    assert!(matches!(
        reconfigure_error(PresentationError::ContextLost(lost())),
        PresentationReconfigureError::PresentationLost { .. }
    ));
}

/// Отказы при возврате окна: нет surface — одно, не приняты настройки — другое.
#[test]
fn reattach_errors_separate_surface_creation_from_rejected_configuration() {
    for error in [
        PresentationError::SurfaceCreation(driver_error()),
        PresentationError::ContextLost(lost()),
        PresentationError::NotAWindowTarget,
        PresentationError::Detached,
    ] {
        assert!(
            matches!(
                reattach_error(error.clone()),
                PresentationReattachError::SurfaceCreation { .. }
            ),
            "{error:?}"
        );
    }
    for error in [
        PresentationError::Driver(driver_error()),
        PresentationError::AlphaChangeRequiresRecreation,
    ] {
        assert!(
            matches!(
                reattach_error(error.clone()),
                PresentationReattachError::ConfigureRejected { .. }
            ),
            "{error:?}"
        );
    }
}
