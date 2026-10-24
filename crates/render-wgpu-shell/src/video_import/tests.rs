//! Focused-тесты склада кадров wgpu (сессия 02 `user/gles2-renderer/`).
//!
//! GPU-тесты на настоящем рендере без окна; без Vulkan-адаптера — явный пропуск. Каждый
//! тест проверяет наблюдаемый результат: исход `materialize`, пиксель цели, журнал
//! освобождений декодера, активный путь импорта.

use render_shell_api::{
    FrameImportError, FrameImportUnsupportedReason, FrameMaterializationOutcome,
    RenderFrameFailureKind, RenderFrameOutcome, RenderShell, RendererFamily, VideoImportHandoff,
    VideoImportInstallError, VideoImportPath, VideoImportPrepareError,
};
use video_core::FrameResourceHandle;
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use crate::Renderer;
use crate::test_support::{
    BLACK_FRAME, GPU_TEST_TIMEOUT, HEADLESS_SIZE, SOFTWARE_PLAN, SolidFrameDecoder,
    SolidFrameLookup, WHITE_FRAME, WHITE_PIXEL, decoded_frame, expect_ready,
    headless_renderer_or_skip, host_planar_frame, lease_for, pixel_matches, present, set_lookup,
};

/// Подключённый к рендеру «декодер» стенда: provider обёртки для аренд и журнал.
struct BoundDecoder {
    decoder_thread: Box<video_backend_api::VideoBackendDecoderThreadHandle>,
    ledger: render_shell_api::conformance::ConformanceReleaseLedger,
    lookup: std::sync::Arc<std::sync::Mutex<SolidFrameLookup>>,
}

impl BoundDecoder {
    /// Аренда кадра через provider обёртки рендера (как у плеера).
    fn lease(&self, frame: video_core::DecodedFrame) -> video_present_core::VideoFrameLease {
        lease_for(frame, self.decoder_thread.resource_provider())
    }
}

/// Подключает «декодер» стенда программным импортом.
fn bind_software_decoder(renderer: &mut Renderer) -> BoundDecoder {
    let decoder = SolidFrameDecoder::start();
    let lookup = decoder.lookup_control();
    let ledger = decoder.ledger.clone();
    let prepared = renderer
        .video_frames()
        .prepare_video_import(decoder.backend, SOFTWARE_PLAN)
        .expect("prepare software import");
    renderer
        .video_frames()
        .install_video_import(prepared.pending)
        .expect("install software import");
    BoundDecoder {
        decoder_thread: prepared.backend.into_decoder_thread(),
        ledger,
        lookup,
    }
}

/// Порядок проверок `materialize`: контракт → подключён ли декодер → provider.
#[test]
fn materialize_checks_declared_contract_before_bound_decoder() {
    let Some(mut renderer) =
        headless_renderer_or_skip("materialize_checks_declared_contract_before_bound_decoder")
    else {
        return;
    };
    let undeclared_contract = VideoFrameContract {
        pixel_layout: video_frame_contract::VideoFramePixelLayout::Rgba8,
        transfer_path: video_frame_contract::VideoFrameTransferPath::SoftwareHostUpload,
    };
    let unsupported_lease = lease_for(
        decoded_frame(FrameResourceHandle(7), undeclared_contract),
        no_provider_handle(),
    );
    let supported_lease = lease_for(host_planar_frame(WHITE_FRAME), no_provider_handle());

    assert_eq!(renderer.video_frames().active_video_import_path(), None);
    assert!(matches!(
        renderer.video_frames().materialize(&unsupported_lease).outcome,
        FrameMaterializationOutcome::Unsupported(FrameImportUnsupportedReason::ContractNotDeclared {
            frame_contract
        }) if frame_contract == undeclared_contract
    ));
    assert!(matches!(
        renderer
            .video_frames()
            .materialize(&supported_lease)
            .outcome,
        FrameMaterializationOutcome::NoVideoBackendBound
    ));

    // С подключённым декодером необъявленный контракт всё равно отклоняется первым.
    let _bound = bind_software_decoder(&mut renderer);
    assert_eq!(
        renderer.video_frames().active_video_import_path(),
        Some(VideoImportPath::SoftwareHostUpload)
    );
    assert!(matches!(
        renderer
            .video_frames()
            .materialize(&unsupported_lease)
            .outcome,
        FrameMaterializationOutcome::Unsupported(
            FrameImportUnsupportedReason::ContractNotDeclared { .. }
        )
    ));
}

/// Busy/Missing/Fatal provider-а различимы и не освобождают кадр декодера.
#[test]
fn provider_busy_missing_fatal_stay_distinct_and_keep_the_frame() {
    let Some(mut renderer) =
        headless_renderer_or_skip("provider_busy_missing_fatal_stay_distinct_and_keep_the_frame")
    else {
        return;
    };
    let bound = bind_software_decoder(&mut renderer);
    let lease = bound.lease(host_planar_frame(BLACK_FRAME));

    set_lookup(&bound.lookup, SolidFrameLookup::Busy);
    let busy = renderer.video_frames().materialize(&lease);
    assert!(busy.lookup_was_busy(), "Busy must stay Busy: {busy:?}");

    set_lookup(&bound.lookup, SolidFrameLookup::Missing);
    assert!(matches!(
        renderer.video_frames().materialize(&lease).outcome,
        FrameMaterializationOutcome::Missing
    ));

    set_lookup(&bound.lookup, SolidFrameLookup::Fatal);
    assert!(matches!(
        renderer.video_frames().materialize(&lease).outcome,
        FrameMaterializationOutcome::Error(FrameImportError::ImportFailed { .. })
    ));

    assert_eq!(
        bound.ledger.release_count(BLACK_FRAME),
        0,
        "lookups must not release the decoder frame while the app holds the lease"
    );
    // Аренда стенда отдаёт освобождение обёртке рендера, а та ждёт работы GPU.
    drop(lease);
    renderer
        .wait_for_gpu_idle(GPU_TEST_TIMEOUT)
        .expect("GPU idle after dropping the lease");
    assert_eq!(
        bound.ledger.release_count(BLACK_FRAME),
        1,
        "the frame is released exactly once after the app dropped the lease"
    );
}

/// Аппаратный кадр на программном импорте — `Unsupported`, активный импорт не тронут.
#[test]
fn hardware_frame_on_software_import_is_unsupported_not_error() {
    let Some(mut renderer) =
        headless_renderer_or_skip("hardware_frame_on_software_import_is_unsupported_not_error")
    else {
        return;
    };
    let bound = bind_software_decoder(&mut renderer);
    let hardware_lease = bound.lease(decoded_frame(
        FrameResourceHandle(5),
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
    ));

    let outcome = renderer.video_frames().materialize(&hardware_lease).outcome;

    assert!(
        matches!(
            outcome,
            FrameMaterializationOutcome::Unsupported(
                FrameImportUnsupportedReason::TransferPathNotHandledByActiveImport
            )
        ),
        "hardware frame on a software import must be a typed Unsupported, got {outcome:?}"
    );
    assert_eq!(
        renderer.video_frames().active_video_import_path(),
        Some(VideoImportPath::SoftwareHostUpload)
    );
}

/// Ключ и подготовленный импорт другого экземпляра рендера отклоняются без изменений.
#[test]
fn foreign_key_and_foreign_pending_import_are_rejected() {
    let Some(mut first) =
        headless_renderer_or_skip("foreign_key_and_foreign_pending_import_are_rejected")
    else {
        return;
    };
    let Some(mut second) =
        headless_renderer_or_skip("foreign_key_and_foreign_pending_import_are_rejected")
    else {
        return;
    };
    let bound = bind_software_decoder(&mut first);
    let lease = bound.lease(host_planar_frame(WHITE_FRAME));
    let first_key = expect_ready(first.video_frames().materialize(&lease).outcome);

    match present(&mut second, Some(&first_key)) {
        RenderFrameOutcome::Failed(failure) => {
            assert_eq!(failure.kind, RenderFrameFailureKind::ForeignVideoFrame);
        }
        other => panic!("foreign key must fail with ForeignVideoFrame, got {other:?}"),
    }

    let decoder = SolidFrameDecoder::start();
    let prepared_on_first = first
        .video_frames()
        .prepare_video_import(decoder.backend, SOFTWARE_PLAN)
        .expect("prepare on the first renderer");
    let install_on_second = second
        .video_frames()
        .install_video_import(prepared_on_first.pending);
    assert!(matches!(
        install_on_second,
        Err(VideoImportInstallError::PreparedByAnotherRenderer { prepared_by })
            if prepared_by == first.instance_id()
    ));
    assert_eq!(
        second.video_frames().active_video_import_path(),
        None,
        "a rejected foreign import must not become active"
    );
}

/// Пересоздание: импорт переносится на новый рендер, рисует там и освобождает кадр
/// ровно один раз после работы GPU нового рендера; чужое семейство отклоняется.
#[test]
fn recreated_import_draws_on_new_renderer_and_releases_once() {
    let Some(mut old_renderer) =
        headless_renderer_or_skip("recreated_import_draws_on_new_renderer_and_releases_once")
    else {
        return;
    };
    let Some(mut new_renderer) =
        headless_renderer_or_skip("recreated_import_draws_on_new_renderer_and_releases_once")
    else {
        return;
    };
    let bound = bind_software_decoder(&mut old_renderer);
    let handoff = old_renderer
        .video_frames()
        .export_video_import_for_recreation()
        .expect("an active import is exported");
    assert_eq!(handoff.family(), RendererFamily::WgpuVulkan);
    let pending = new_renderer
        .video_frames()
        .prepare_recreated_video_import(handoff)
        .expect("prepare recreated import");
    assert_eq!(
        new_renderer.video_frames().active_video_import_path(),
        None,
        "preparing must not touch the active import of the new renderer"
    );
    new_renderer
        .video_frames()
        .install_video_import(pending)
        .expect("install recreated import");

    let lease = bound.lease(host_planar_frame(WHITE_FRAME));
    let key = expect_ready(new_renderer.video_frames().materialize(&lease).outcome);
    assert!(matches!(
        present(&mut new_renderer, Some(&key)),
        RenderFrameOutcome::Presented(_)
    ));
    lease.mark_submitted_to_renderer();
    let pixel = new_renderer
        .read_offscreen_pixel(HEADLESS_SIZE[0] / 2, HEADLESS_SIZE[1] / 2)
        .expect("offscreen target");
    assert!(pixel_matches(pixel, WHITE_PIXEL), "got {pixel:?}");

    drop(key);
    drop(lease);
    new_renderer
        .wait_for_gpu_idle(GPU_TEST_TIMEOUT)
        .expect("new renderer GPU idle");
    old_renderer
        .wait_for_gpu_idle(GPU_TEST_TIMEOUT)
        .expect("old renderer GPU idle");
    assert_eq!(bound.ledger.release_count(WHITE_FRAME), 1);

    let foreign_family = VideoImportHandoff::new(
        RendererFamily::Gles2,
        VideoImportPath::SoftwareHostUpload,
        (),
    );
    assert!(matches!(
        new_renderer
            .video_frames()
            .prepare_recreated_video_import(foreign_family),
        Err(VideoImportPrepareError::HandoffFromAnotherFamily {
            handoff_family: RendererFamily::Gles2
        })
    ));
}

/// Счётчиков кэша импорта нет без декодера и у программного импорта (кэш только у DMA-BUF).
#[test]
fn import_stats_exist_only_for_dma_buf_import_cache() {
    let Some(mut renderer) =
        headless_renderer_or_skip("import_stats_exist_only_for_dma_buf_import_cache")
    else {
        return;
    };
    assert_eq!(renderer.video_frames().import_stats(), None);
    let _bound = bind_software_decoder(&mut renderer);
    assert_eq!(renderer.video_frames().import_stats(), None);
    assert!(
        renderer
            .video_frames()
            .export_video_import_for_recreation()
            .is_some()
    );
}

/// Provider для аренд, которые до provider-а дойти не должны (проверки до lookup-а).
fn no_provider_handle() -> video_backend_api::PresentFrameResourceProviderHandle {
    SolidFrameDecoder::start()
        .backend
        .into_decoder_thread()
        .resource_provider()
}
