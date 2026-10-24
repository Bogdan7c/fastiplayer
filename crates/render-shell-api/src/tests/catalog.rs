use render_core::RenderBackendKind;

use crate::{
    RendererCreateError, RendererCreateStage, RendererFamily, RendererProbe,
    RendererUnusableReason, RequiredRendererFeature,
};

/// Стабильные id семейств различны и совпадают с классом рендера в возможностях.
#[test]
fn renderer_families_have_distinct_stable_ids_and_backend_kinds() {
    assert_eq!(RendererFamily::WgpuVulkan.stable_id(), "wgpu-vulkan");
    assert_eq!(RendererFamily::Gles2.stable_id(), "gles2");
    assert_eq!(
        RendererFamily::WgpuVulkan.backend_kind(),
        RenderBackendKind::Wgpu
    );
    assert_eq!(
        RendererFamily::Gles2.backend_kind(),
        RenderBackendKind::OpenGles
    );
}

/// Причины «не годен» не схлопываются: программный адаптер ≠ нет адаптера ≠ упал при старте.
#[test]
fn unusable_reasons_stay_distinct() {
    let reasons = [
        RendererUnusableReason::NoHardwareAdapter,
        RendererUnusableReason::SoftwareAdapterOnly,
        RendererUnusableReason::MissingRequiredFeature(RequiredRendererFeature::new(
            "EGL_EXT_image_dma_buf_import",
        )),
        RendererUnusableReason::PreviousStartupCrashed,
        RendererUnusableReason::CreationFailed(RendererCreateError::new(
            RendererCreateStage::SurfaceCreation,
            "no ARGB visual",
        )),
    ];

    for (index, reason) in reasons.iter().enumerate() {
        for (other_index, other) in reasons.iter().enumerate() {
            assert_eq!(index == other_index, reason == other);
        }
    }
    assert_ne!(
        RendererProbe::Usable,
        RendererProbe::Unusable {
            reason: RendererUnusableReason::SoftwareAdapterOnly
        }
    );
}

/// Ошибка создания несёт шаг и сообщение драйвера, имя возможности — строку спецификации.
#[test]
fn create_error_and_feature_keep_driver_facts() {
    let error = RendererCreateError::new(RendererCreateStage::DeviceCreation, "context lost");
    let feature = RequiredRendererFeature::new("GL_OES_EGL_image");

    assert_eq!(error.stage, RendererCreateStage::DeviceCreation);
    assert_eq!(error.to_string(), "DeviceCreation: context lost");
    assert_eq!(feature.specification_name(), "GL_OES_EGL_image");
}
