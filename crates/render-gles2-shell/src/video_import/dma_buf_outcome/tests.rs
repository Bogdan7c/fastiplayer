//! Таблица перевода отказов аппаратного пути: каждый исход — своя реакция приложения.

use gles2_egui::{
    DmaBufTextureAttached, DmaBufTextureError, GlAccessError, ImageRelease, ModifierAttribute,
};
use gles2_video::{DmaBufImportReport, VideoImportError};
use video_core::DmaBufDescriptorRejection;

use super::*;
use crate::dma_buf_frame::{DmaBufShownFrom, Nv12DmaBufError};

/// Отчёт об импорте двух плоскостей с исправно освобождёнными `EGLImage`.
fn import_report() -> DmaBufImportReport {
    let attached = DmaBufTextureAttached {
        modifier: ModifierAttribute::DriverChosen,
        image_release: ImageRelease::Released,
    };
    DmaBufImportReport {
        luma: attached,
        chroma: attached,
    }
}

/// Показ любым способом — кадр на видеослое; занятость, отсутствие и фатальный провайдер не
/// сливаются в один исход.
#[test]
fn shown_busy_missing_and_fatal_stay_distinct() {
    for shown in [
        DmaBufShownFrom::CacheHit,
        DmaBufShownFrom::AlreadyCurrent,
        DmaBufShownFrom::FreshImport(import_report()),
    ] {
        assert!(matches!(
            layer_outcome_for_dma_buf(DmaBufMaterializeOutcome::Shown(shown)),
            LayerShowOutcome::Shown
        ));
    }
    assert!(matches!(
        layer_outcome_for_dma_buf(DmaBufMaterializeOutcome::Busy),
        LayerShowOutcome::Busy
    ));
    assert!(matches!(
        layer_outcome_for_dma_buf(DmaBufMaterializeOutcome::Missing),
        LayerShowOutcome::Missing
    ));
    assert!(matches!(
        layer_outcome_for_dma_buf(DmaBufMaterializeOutcome::ProviderFatal),
        LayerShowOutcome::Failed(FrameImportError::ProviderFatal)
    ));
}

/// Настоящий отказ дескриптора проходит как есть (с исходной причиной), не как отказ рендера.
#[test]
fn descriptor_rejection_keeps_its_original_reason() {
    let original = DmaBufDescriptorRejection::AbsentLayers;
    let outcome = layer_outcome_for_dma_buf(DmaBufMaterializeOutcome::Unsupported(
        DmaBufUnsupportedReason::DescriptorRejected(original.clone()),
    ));
    assert!(
        matches!(
            outcome,
            LayerShowOutcome::Unsupported(FrameImportUnsupportedReason::DmaBufDescriptorRejected(
                ref rejection
            )) if *rejection == original
        ),
        "{outcome:?}"
    );
}

/// Отказы «рендер не умеет» получают свою честную причину, а не маску `InvalidFrameContract`.
#[test]
fn renderer_limits_are_reported_as_renderer_refusals_not_descriptor_rejections() {
    let driver_limit = VideoImportError::Attach {
        plane: gles2_video::DmaBufPlaneRole::Luma,
        source: DmaBufTextureError::ImageTargetFunctionMissing,
    };
    let cases = [
        (
            DmaBufUnsupportedReason::HostPlanarDescriptor,
            "host-memory",
            "HostMemoryDescriptor",
        ),
        (
            DmaBufUnsupportedReason::FrameFormat(Nv12DmaBufError::NoObjects),
            "no kernel objects",
            "FrameFormatNotShown",
        ),
        (
            DmaBufUnsupportedReason::DriverCannotImport(driver_limit),
            "cannot import",
            "DriverCannotImport",
        ),
    ];
    for (reason, expected_fragment, expected_variant) in cases {
        let outcome =
            layer_outcome_for_dma_buf(DmaBufMaterializeOutcome::Unsupported(reason.clone()));
        let LayerShowOutcome::Unsupported(
            FrameImportUnsupportedReason::RendererCannotShowHardwareFrame(refusal),
        ) = outcome
        else {
            panic!("{reason:?} must be a renderer refusal, got {outcome:?}");
        };
        let text = refusal.to_string();
        assert!(
            text.contains(expected_fragment),
            "{reason:?}: `{text}` must mention `{expected_fragment}`"
        );
        assert!(
            format!("{refusal:?}").starts_with(expected_variant),
            "{reason:?}: {refusal:?} must be {expected_variant}"
        );
    }
}

/// Сбой драйвера на валидном запросе — ошибка, а не «не поддерживается»: фолбэк его не лечит.
#[test]
fn driver_failure_on_a_valid_request_is_an_error_not_unsupported() {
    let failure = VideoImportError::GlAccess(GlAccessError::DetachedWithoutSurfacelessContext);
    match layer_outcome_for_dma_buf(DmaBufMaterializeOutcome::ImportFailed(failure)) {
        LayerShowOutcome::Failed(FrameImportError::ImportFailed { message }) => {
            assert!(message.contains("unavailable"), "{message}");
        }
        other => panic!("expected ImportFailed, got {other:?}"),
    }
}
