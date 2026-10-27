//! Focused-тесты склада кадров на fake-рендере: различимость исходов, владение кадром,
//! чужие ключи/импорты, перенос импорта при пересоздании, потеря устройства.

use std::sync::{Arc, Mutex};

use video_backend_api::VideoBackendDecoderThreadHandle;
use video_core::FrameResourceHandle;
use video_frame_contract::VideoFrameContract;
use video_present_core::{
    VideoFrameLease, VideoFrameLeaseConfig, VideoFrameRelease, VideoFrameReleaseOutcome,
    VideoFrameReleaseSink,
};

use crate::conformance::ConformanceReleaseLedger;
use crate::{
    EguiFrameContent, FrameImportError, FrameImportUnsupportedReason, FrameMaterializationOutcome,
    RenderFrameFailureKind, RenderFrameInput, RenderFrameOutcome, RenderShell, RenderableFrameKey,
    RendererFamily, SurfaceFrameAcquire, VideoFrameImport, VideoImportInstallError,
    VideoImportPath, VideoImportPrepareError,
};

use super::fake_decoder::{
    FAKE_POOL_LOCK_WAIT, FakeDecoder, FakeProviderLookup, decoded_frame, set_lookup,
};
use super::fake_shell::{FakeDefect, FakeRenderShell};
use super::{HARDWARE_PLAN, SCREEN, SUPPORTED_FRAME_HANDLE, supported_frame};

/// Sink аренды как fallback плеера: освобождение сразу в provider аренды (обёртку рендера).
struct ProviderReleaseSink;

impl VideoFrameReleaseSink for ProviderReleaseSink {
    fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
        match release.resource_provider() {
            Some(provider) => {
                provider.release_frame(release.resource_handle());
                VideoFrameReleaseOutcome::Accepted
            }
            None => VideoFrameReleaseOutcome::NoOp,
        }
    }
}

/// Рендер с подключённым fake-декодером и всё, что держал бы плеер.
struct BoundRenderer {
    renderer: FakeRenderShell,
    decoder_thread: Box<VideoBackendDecoderThreadHandle>,
    ledger: ConformanceReleaseLedger,
    lookup: Arc<Mutex<FakeProviderLookup>>,
}

impl BoundRenderer {
    fn new() -> Self {
        Self::new_with_family(RendererFamily::Gles2)
    }

    fn new_with_family(family: RendererFamily) -> Self {
        let mut renderer = FakeRenderShell::new(family, FakeDefect::None);
        let decoder = FakeDecoder::start();
        let prepared = renderer
            .prepare_video_import(decoder.backend, HARDWARE_PLAN)
            .expect("fake supports hardware import");
        renderer
            .install_video_import(prepared.pending)
            .expect("pending import prepared by this renderer");
        Self {
            renderer,
            decoder_thread: prepared.backend.into_decoder_thread(),
            ledger: decoder.ledger,
            lookup: decoder.lookup,
        }
    }

    /// Аренда кадра, как её выдал бы плеер (provider — обёртка рендера).
    fn lease(&self, handle: FrameResourceHandle, contract: VideoFrameContract) -> VideoFrameLease {
        VideoFrameLease::new(
            VideoFrameLeaseConfig::new(
                1,
                decoded_frame(handle, contract),
                Arc::new(ProviderReleaseSink),
            )
            .with_resource_provider(self.decoder_thread.resource_provider()),
        )
    }

    fn supported_lease(&self, handle: FrameResourceHandle) -> VideoFrameLease {
        self.lease(handle, supported_frame(handle).frame_contract)
    }
}

fn ready_key(renderer: &mut dyn RenderShell, lease: &VideoFrameLease) -> RenderableFrameKey {
    match renderer.video_frames().materialize(lease).outcome {
        FrameMaterializationOutcome::Ready(key) => key,
        other => panic!("expected Ready, got {other:?}"),
    }
}

fn present(renderer: &mut dyn RenderShell, key: Option<&RenderableFrameKey>) -> RenderFrameOutcome {
    let SurfaceFrameAcquire::Acquired(surface) = renderer.acquire_surface_frame(SCREEN) else {
        panic!("fake always acquires a surface");
    };
    renderer.render_frame(RenderFrameInput {
        surface,
        video_frame: key,
        egui: EguiFrameContent::empty_fresh(),
        screen: SCREEN,
        video_viewport: render_core::RenderViewport::full_surface(800, 600),
        video_exclusion_rects: Vec::new(),
        subtitle_overlay: None,
    })
}

/// Ready/Busy/Missing/Fatal provider-а превращаются в четыре разных исхода, ожидание lock
/// пула доходит до вызывающего, а неуспешные исходы не освобождают кадр.
#[test]
fn provider_lookup_outcomes_stay_distinct_and_keep_pool_wait() {
    let mut bound = BoundRenderer::new();
    let lease = bound.supported_lease(SUPPORTED_FRAME_HANDLE);

    let ready = bound.renderer.materialize(&lease);
    assert_eq!(ready.resource_pool_lock_wait, FAKE_POOL_LOCK_WAIT);
    assert!(matches!(
        ready.outcome,
        FrameMaterializationOutcome::Ready(_)
    ));

    set_lookup(&bound.lookup, FakeProviderLookup::Busy);
    let busy = bound.renderer.materialize(&lease);
    assert!(busy.lookup_was_busy());
    assert_eq!(busy.resource_pool_lock_wait, FAKE_POOL_LOCK_WAIT);

    set_lookup(&bound.lookup, FakeProviderLookup::Missing);
    let missing = bound.renderer.materialize(&lease);
    assert!(matches!(
        missing.outcome,
        FrameMaterializationOutcome::Missing
    ));
    assert!(!missing.lookup_was_busy());

    set_lookup(&bound.lookup, FakeProviderLookup::Fatal);
    let fatal = bound.renderer.materialize(&lease);
    assert!(matches!(
        fatal.outcome,
        FrameMaterializationOutcome::Error(FrameImportError::ProviderFatal)
    ));

    drop(ready);
    bound
        .renderer
        .wait_for_gpu_idle(std::time::Duration::ZERO)
        .expect("fake drains");
    assert_eq!(
        bound.ledger.release_count(SUPPORTED_FRAME_HANDLE),
        0,
        "lookups must not release the frame while the app holds the lease"
    );
}

/// Кадр другого пути передачи (программный при аппаратном импорте) — `Unsupported`, а не
/// `Error`, и активный импорт не меняется.
#[test]
fn frame_of_another_transfer_path_is_unsupported_without_touching_active_import() {
    let mut bound = BoundRenderer::new();
    let software_lease = bound.lease(
        FrameResourceHandle(21),
        VideoFrameContract::host_yuv420_planar8(),
    );

    let outcome = bound.renderer.materialize(&software_lease).outcome;

    assert!(matches!(
        outcome,
        FrameMaterializationOutcome::Unsupported(
            FrameImportUnsupportedReason::TransferPathNotHandledByActiveImport
        )
    ));
    assert_eq!(
        bound.renderer.active_video_import_path(),
        Some(VideoImportPath::HardwareZeroCopy)
    );
}

/// Кадр возвращается декодеру только после «работы GPU», а не при drop-е аренды.
#[test]
fn submitted_frame_is_released_only_after_gpu_work_completes() {
    let mut bound = BoundRenderer::new();
    let lease = bound.supported_lease(SUPPORTED_FRAME_HANDLE);
    let key = ready_key(&mut bound.renderer, &lease);
    assert!(matches!(
        present(&mut bound.renderer, Some(&key)),
        RenderFrameOutcome::Presented(_)
    ));
    lease.mark_submitted_to_renderer();
    assert_eq!(bound.renderer.presented_video(), &[SUPPORTED_FRAME_HANDLE]);

    drop(key);
    drop(lease);
    assert_eq!(bound.renderer.pending_gpu_releases(), 1);
    assert_eq!(bound.ledger.release_count(SUPPORTED_FRAME_HANDLE), 0);

    bound
        .renderer
        .wait_for_gpu_idle(std::time::Duration::ZERO)
        .expect("fake drains");
    assert_eq!(bound.ledger.release_count(SUPPORTED_FRAME_HANDLE), 1);
    assert_eq!(bound.renderer.pending_gpu_releases(), 0);
}

/// Ключ удерживает кадр: аренда отпущена, но пока жив клон ключа (кэш «прошлого кадра»),
/// кадр не возвращается декодеру.
#[test]
fn renderable_key_pins_the_decoder_frame_until_its_last_clone_drops() {
    let mut bound = BoundRenderer::new();
    let lease = bound.supported_lease(SUPPORTED_FRAME_HANDLE);
    let key = ready_key(&mut bound.renderer, &lease);
    let cached_key = key.clone();
    assert!(key.shares_prepared_frame_with(&cached_key));
    assert_eq!(
        cached_key.decoded_frame().resource_handle,
        SUPPORTED_FRAME_HANDLE
    );

    drop(lease);
    drop(key);
    bound
        .renderer
        .wait_for_gpu_idle(std::time::Duration::ZERO)
        .expect("fake drains");
    assert_eq!(bound.ledger.release_count(SUPPORTED_FRAME_HANDLE), 0);

    drop(cached_key);
    bound
        .renderer
        .wait_for_gpu_idle(std::time::Duration::ZERO)
        .expect("fake drains");
    assert_eq!(bound.ledger.release_count(SUPPORTED_FRAME_HANDLE), 1);
}

/// Ключ другого экземпляра рендера не рисуется и не попадает в показанные кадры.
#[test]
fn key_from_another_renderer_instance_is_rejected() {
    let mut first = BoundRenderer::new();
    let mut second = FakeRenderShell::new(RendererFamily::Gles2, FakeDefect::None);
    let lease = first.supported_lease(SUPPORTED_FRAME_HANDLE);
    let foreign_key = ready_key(&mut first.renderer, &lease);

    let outcome = present(&mut second, Some(&foreign_key));

    match outcome {
        RenderFrameOutcome::Failed(failure) => {
            assert_eq!(failure.kind, RenderFrameFailureKind::ForeignVideoFrame);
        }
        other => panic!("foreign key must fail, got {other:?}"),
    }
    assert!(second.presented_video().is_empty());
}

/// Импорт, подготовленный другим рендером, не ставится, и активный импорт не меняется.
#[test]
fn pending_import_from_another_renderer_is_refused_without_side_effects() {
    let mut owner = FakeRenderShell::new(RendererFamily::Gles2, FakeDefect::None);
    let mut other = BoundRenderer::new();
    let prepared = owner
        .prepare_video_import(FakeDecoder::start().backend, HARDWARE_PLAN)
        .expect("fake supports hardware import");
    let prepared_by = prepared.pending.owner();

    let refusal = other
        .renderer
        .install_video_import(prepared.pending)
        .expect_err("pending import of another renderer must be refused");

    assert_eq!(
        refusal,
        VideoImportInstallError::PreparedByAnotherRenderer { prepared_by }
    );
    assert_eq!(owner.active_video_import_path(), None);
    assert_eq!(
        other.renderer.active_video_import_path(),
        Some(VideoImportPath::HardwareZeroCopy)
    );
}

/// Состояние другого семейства нельзя перенести при пересоздании.
#[test]
fn handoff_from_another_family_is_refused() {
    let wgpu_like = BoundRenderer::new_with_family(RendererFamily::WgpuVulkan);
    let mut gles2 = FakeRenderShell::new(RendererFamily::Gles2, FakeDefect::None);
    let handoff = wgpu_like
        .renderer
        .export_video_import_for_recreation()
        .expect("active import can be exported");

    let refusal = gles2
        .prepare_recreated_video_import(handoff)
        .expect_err("handoff of another family must be refused");

    assert_eq!(
        refusal,
        VideoImportPrepareError::HandoffFromAnotherFamily {
            handoff_family: RendererFamily::WgpuVulkan
        }
    );
    assert_eq!(gles2.active_video_import_path(), None);
}

/// Пересоздание: после установки на новом рендере освобождения ждут его «GPU», а кадры,
/// отправленные старому, освобождает старый drain (порядок транзакции приложения).
#[test]
fn recreated_import_rebinds_deferred_releases_to_the_new_renderer() {
    let mut old = BoundRenderer::new();
    let old_lease = old.supported_lease(SUPPORTED_FRAME_HANDLE);
    let old_key = ready_key(&mut old.renderer, &old_lease);
    assert!(matches!(
        present(&mut old.renderer, Some(&old_key)),
        RenderFrameOutcome::Presented(_)
    ));
    drop(old_key);
    drop(old_lease);
    old.renderer
        .wait_for_gpu_idle(std::time::Duration::ZERO)
        .expect("old drain");
    assert_eq!(old.ledger.release_count(SUPPORTED_FRAME_HANDLE), 1);

    let mut new = FakeRenderShell::new(RendererFamily::Gles2, FakeDefect::None);
    let handoff = old
        .renderer
        .export_video_import_for_recreation()
        .expect("active import can be exported");
    let pending = new
        .prepare_recreated_video_import(handoff)
        .expect("same family handoff");
    assert_eq!(pending.owner(), new.instance_id());
    new.install_video_import(pending)
        .expect("prepared by the new renderer");

    let next_handle = FrameResourceHandle(12);
    let next_lease = old.supported_lease(next_handle);
    let next_key = ready_key(&mut new, &next_lease);
    assert!(matches!(
        present(&mut new, Some(&next_key)),
        RenderFrameOutcome::Presented(_)
    ));
    drop(next_key);
    drop(next_lease);

    old.renderer
        .wait_for_gpu_idle(std::time::Duration::ZERO)
        .expect("old drain");
    assert_eq!(
        old.ledger.release_count(next_handle),
        0,
        "old GPU must not release new frames"
    );
    assert_eq!(new.pending_gpu_releases(), 1);
    new.wait_for_gpu_idle(std::time::Duration::ZERO)
        .expect("new drain");
    assert_eq!(old.ledger.release_count(next_handle), 1);
}

/// После потери устройства новые освобождения проходят сразу, без ожидания GPU.
#[test]
fn releases_after_device_lost_are_immediate() {
    let mut bound = BoundRenderer::new();
    let released = bound
        .renderer
        .release_after_device_lost()
        .expect("fake release after device lost");
    assert_eq!(released.get(), 0);

    let lease = bound.supported_lease(SUPPORTED_FRAME_HANDLE);
    let key = ready_key(&mut bound.renderer, &lease);
    drop(key);
    drop(lease);

    assert_eq!(bound.renderer.pending_gpu_releases(), 0);
    assert_eq!(bound.ledger.release_count(SUPPORTED_FRAME_HANDLE), 1);
}
