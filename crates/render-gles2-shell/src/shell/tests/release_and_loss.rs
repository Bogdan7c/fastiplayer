//! Кадры декодера и потеря устройства: возврат только после fence кадра показа и ровно один
//! раз, потеря контекста, закрытие рендера и перенос импорта при пересоздании.

use std::time::Duration;

use gles2_egui::{ContextLoss, ContextLossDetection, ContextLossReason};
use render_shell_api::{
    RenderFrameFailureKind, RenderFrameOutcome, RenderShell, RendererFamily, RendererGpuDrainError,
    SurfaceFrameAcquire, VideoImportHandoff, VideoImportInstallError, VideoImportPath,
    VideoImportPrepareError,
};

use super::{
    ConnectedDecoder, FrameExtras, SOFTWARE_PLAN, assert_gray, connect_software_decoder,
    materialize_key, offscreen_or_skip, pixel, present, screen_for,
};
use crate::Gles2Renderer;
use crate::capabilities::HardwareZeroCopy;
use crate::frame_release::Gles2FrameReleaseBinding;
use crate::test_support::fake_decoder::{FakeDecoder, SplitPicture};
use crate::video_import::ImportEnvironment;

const TARGET: [u32; 2] = [64, 32];
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

fn picture(handle: u64, left_luma: u8, right_luma: u8) -> SplitPicture {
    SplitPicture {
        handle,
        size: TARGET,
        left_luma,
        right_luma,
    }
}

fn idle(renderer: &mut Gles2Renderer) {
    renderer
        .wait_for_gpu_idle(IDLE_TIMEOUT)
        .expect("the GPU becomes idle");
}

fn context_loss() -> ContextLoss {
    ContextLoss {
        reason: ContextLossReason::GuiltyReset,
        detection: ContextLossDetection::ResetStatusQuery,
    }
}

/// Кадр декодера вернётся не раньше, чем приложение отпустит ключ и аренду, и ровно один раз;
/// возврат шёл по fence кадра показа (рендер его опубликовал), а не «без fence».
#[test]
fn a_decoder_frame_comes_back_once_after_the_app_lets_go_and_by_the_presented_fence() {
    let Some(mut renderer) = offscreen_or_skip(
        "a_decoder_frame_comes_back_once_after_the_app_lets_go_and_by_the_presented_fence",
        TARGET,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (key, lease) = materialize_key(&mut renderer, &decoder, picture(1, 180, 60));
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    idle(&mut renderer);
    assert_eq!(
        decoder.state.release_count(1),
        0,
        "the app still holds the key and the lease"
    );

    drop(key);
    drop(lease);
    idle(&mut renderer);

    assert_eq!(decoder.state.release_count(1), 1, "returned exactly once");
    let stats = renderer.frame_release_stats();
    assert_eq!(
        stats.released_without_fence, 0,
        "the presented frame published its fence before the release: {stats:?}"
    );
    assert_eq!(stats.pending_now, 0);

    // Ещё кадр и ожидание не возвращают тот же ресурс повторно.
    assert!(matches!(
        present(&mut renderer, TARGET, None, FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    idle(&mut renderer);
    assert_eq!(decoder.state.release_count(1), 1);
}

/// Потеря контекста: ожидание GPU и кадр сообщают её типизированно, а кадры декодера
/// освобождаются ровно один раз (повторный вызов — ноль).
#[test]
fn a_lost_context_is_reported_and_decoder_frames_are_released_exactly_once() {
    let Some(mut renderer) = offscreen_or_skip(
        "a_lost_context_is_reported_and_decoder_frames_are_released_exactly_once",
        TARGET,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (key, lease) = materialize_key(&mut renderer, &decoder, picture(7, 180, 60));
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));

    renderer.context.simulate_context_loss(context_loss());

    match renderer.wait_for_gpu_idle(IDLE_TIMEOUT) {
        Err(RendererGpuDrainError::DeviceLost(lost)) => {
            assert!(lost.message.contains("GuiltyReset"), "{}", lost.message);
        }
        other => panic!("a lost context must be DeviceLost, got {other:?}"),
    }
    let SurfaceFrameAcquire::Acquired(surface) = renderer.acquire_surface_frame(screen_for(TARGET))
    else {
        panic!("the lost context is reported by the frame, not hidden by a drop");
    };
    let outcome = renderer.render_frame(render_shell_api::RenderFrameInput {
        surface,
        video_frame: Some(&key),
        egui: render_shell_api::EguiFrameContent::empty_fresh(),
        screen: screen_for(TARGET),
        video_viewport: render_core::RenderViewport::full_surface(TARGET[0], TARGET[1]),
        video_exclusion_rects: Vec::new(),
        window_corner_mask: render_shell_api::WindowCornerMask::square(),
        subtitle_overlay: None,
    });
    match outcome {
        RenderFrameOutcome::Failed(failure) => {
            assert_eq!(failure.kind, RenderFrameFailureKind::VideoRender);
            assert!(failure.message.contains("lost"), "{}", failure.message);
        }
        other => panic!("a frame on a lost context must fail, got {other:?}"),
    }

    // Приложение держало кадр до потери; освобождение после потери идёт сразу и один раз.
    let first = renderer
        .video_frames()
        .release_after_device_lost()
        .expect("release after device lost");
    assert_eq!(
        first.get(),
        0,
        "nothing was waiting yet: the app holds the lease"
    );
    assert_eq!(decoder.state.release_count(7), 0);
    drop(key);
    drop(lease);
    assert_eq!(
        decoder.state.release_count(7),
        1,
        "after the loss a released frame goes back at once, once"
    );
    let again = renderer
        .video_frames()
        .release_after_device_lost()
        .expect("repeated release after device lost");
    assert_eq!(again.get(), 0, "the repeat releases nothing");
    assert_eq!(decoder.state.release_count(7), 1);
}

/// Закрытие рендера с живой арендой: кадр всё равно возвращается декодеру ровно один раз,
/// привязка штатно отвязана (повторная отвязка — no-op).
#[test]
fn closing_the_renderer_returns_held_frames_once_and_detaches_the_binding() {
    let Some(mut renderer) = offscreen_or_skip(
        "closing_the_renderer_returns_held_frames_once_and_detaches_the_binding",
        TARGET,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (key, lease) = materialize_key(&mut renderer, &decoder, picture(3, 180, 60));
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    let binding = renderer.release.binding().clone();

    drop(renderer);

    let repeated = binding.renderer_detaching(crate::RendererDetachGrace::DEFAULT);
    assert!(
        repeated.already_detached,
        "dropping the renderer already detached the binding"
    );
    drop(key);
    drop(lease);
    assert_eq!(decoder.state.release_count(3), 1);
    assert_eq!(binding.stats().released_after_renderer_detach, 1);
}

/// Пересоздание внутри семейства: новый рендер продолжает с обёрткой декодера прежнего, кадры
/// нового рендера возвращаются по ЕГО fence, а закрытие старого рендера не ломает привязку.
#[test]
fn recreation_moves_the_import_and_the_old_renderer_does_not_detach_the_new_ones_binding() {
    let Some(mut old_renderer) = offscreen_or_skip(
        "recreation_moves_the_import_and_the_old_renderer_does_not_detach_the_new_ones_binding",
        TARGET,
    ) else {
        return;
    };
    let Some(mut new_renderer) = offscreen_or_skip(
        "recreation_moves_the_import_and_the_old_renderer_does_not_detach_the_new_ones_binding",
        TARGET,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut old_renderer);
    let (old_key, old_lease) = materialize_key(&mut old_renderer, &decoder, picture(1, 180, 60));
    assert!(matches!(
        present(
            &mut old_renderer,
            TARGET,
            Some(&old_key),
            FrameExtras::default()
        ),
        RenderFrameOutcome::Presented(_)
    ));
    drop(old_key);
    drop(old_lease);

    // Транзакция приложения: экспорт → подготовка → ожидание старого GPU → установка → замена.
    let handoff = old_renderer
        .video_frames()
        .export_video_import_for_recreation()
        .expect("the old renderer has an import to move");
    assert_eq!(handoff.family(), RendererFamily::Gles2);
    let pending = new_renderer
        .video_frames()
        .prepare_recreated_video_import(handoff)
        .expect("the new renderer prepares the moved import");
    idle(&mut old_renderer);
    new_renderer
        .video_frames()
        .install_video_import(pending)
        .expect("the moved import is installed");
    drop(old_renderer);

    assert_eq!(
        new_renderer.video_frames().active_video_import_path(),
        Some(VideoImportPath::SoftwareHostUpload)
    );
    let (new_key, new_lease) = materialize_key(&mut new_renderer, &decoder, picture(2, 30, 220));
    assert!(matches!(
        present(
            &mut new_renderer,
            TARGET,
            Some(&new_key),
            FrameExtras::default()
        ),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(
        pixel(&new_renderer, 16, 16),
        30,
        "frame after the recreation",
    );
    drop(new_key);
    drop(new_lease);
    idle(&mut new_renderer);
    assert_eq!(decoder.state.release_count(2), 1);
    let stats = new_renderer.frame_release_stats();
    assert_eq!(
        stats.released_after_renderer_detach, 0,
        "closing the old renderer must not detach the binding the new one uses: {stats:?}"
    );
    assert_eq!(stats.released_without_fence, 0, "{stats:?}");
}

/// Чужие коробки отвергаются типизированно: перенос из другого семейства, импорт, подготовленный
/// другим рендером.
#[test]
fn foreign_handoffs_and_foreign_pending_imports_are_refused() {
    let Some(mut renderer) = offscreen_or_skip(
        "foreign_handoffs_and_foreign_pending_imports_are_refused",
        TARGET,
    ) else {
        return;
    };
    let Some(mut other) = offscreen_or_skip(
        "foreign_handoffs_and_foreign_pending_imports_are_refused",
        TARGET,
    ) else {
        return;
    };

    let wgpu_handoff = VideoImportHandoff::new(
        RendererFamily::WgpuVulkan,
        VideoImportPath::SoftwareHostUpload,
        (),
    );
    assert_eq!(
        renderer
            .video_frames()
            .prepare_recreated_video_import(wgpu_handoff)
            .unwrap_err(),
        VideoImportPrepareError::HandoffFromAnotherFamily {
            handoff_family: RendererFamily::WgpuVulkan
        }
    );

    let decoder = FakeDecoder::start();
    let prepared = other
        .video_frames()
        .prepare_video_import(decoder.backend, SOFTWARE_PLAN)
        .expect("the other renderer prepares");
    let other_id = other.instance_id();
    assert_eq!(
        renderer
            .video_frames()
            .install_video_import(prepared.pending)
            .unwrap_err(),
        VideoImportInstallError::PreparedByAnotherRenderer {
            prepared_by: other_id
        }
    );
    assert_eq!(
        renderer.video_frames().active_video_import_path(),
        None,
        "a refused import changes nothing"
    );
}

/// Импорт подготовлен паспортом этого рендера, но обёртка декодера сделана ЧУЖОЙ привязкой
/// освобождения: установка отказывает типизированной ошибкой и ничего не меняет (иначе кадры
/// возвращались бы по fence чужого рендера). Обёртка привязкой этого же рендера ставится.
#[test]
fn an_import_wrapped_by_a_foreign_release_binding_is_refused_and_the_own_one_installs() {
    let Some(mut renderer) = offscreen_or_skip(
        "an_import_wrapped_by_a_foreign_release_binding_is_refused_and_the_own_one_installs",
        TARGET,
    ) else {
        return;
    };
    let foreign_environment = ImportEnvironment::new(
        renderer.instance_id(),
        Gles2FrameReleaseBinding::new(),
        HardwareZeroCopy::Unavailable,
    );
    let decoder = FakeDecoder::start();
    let prepared = foreign_environment
        .prepare(decoder.backend, SOFTWARE_PLAN)
        .expect("the foreign environment prepares");

    let error = renderer
        .video_frames()
        .install_video_import(prepared.pending)
        .unwrap_err();

    assert!(
        matches!(
            error,
            VideoImportInstallError::SubmissionBindingFailed { .. }
        ),
        "got {error:?}"
    );
    assert_eq!(
        renderer.video_frames().active_video_import_path(),
        None,
        "a refused import changes nothing"
    );

    let own_decoder = FakeDecoder::start();
    let own = renderer
        .video_frames()
        .prepare_video_import(own_decoder.backend, SOFTWARE_PLAN)
        .expect("the own environment prepares");
    renderer
        .video_frames()
        .install_video_import(own.pending)
        .expect("a wrapper made by the own binding installs");
    assert_eq!(
        renderer.video_frames().active_video_import_path(),
        Some(VideoImportPath::SoftwareHostUpload)
    );
}

/// Подготовщик работает в другом потоке и ставится только на выдавший рендер; аппаратный путь
/// принимается ровно тогда, когда драйвер объявил аппаратные контракты.
#[test]
fn the_preparer_works_off_thread_and_hardware_paths_follow_the_capabilities() {
    let Some(renderer) = offscreen_or_skip(
        "the_preparer_works_off_thread_and_hardware_paths_follow_the_capabilities",
        TARGET,
    ) else {
        return;
    };
    let hardware_contract = video_frame_contract::VideoFrameContract::dma_buf_nv12(
        video_frame_contract::DmaBufImageLayout::ComposedLayers,
    );
    let mut preparer = renderer.video_import_preparer();
    let decoder = FakeDecoder::start();
    let hardware_plan = render_shell_api::VideoImportPlan {
        path: VideoImportPath::HardwareZeroCopy,
        decoder_surface_pool: render_shell_api::DecoderSurfacePoolSlots::new(4),
    };
    let prepared_hardware =
        std::thread::spawn(move || preparer.prepare_video_import(decoder.backend, hardware_plan))
            .join()
            .expect("the preparer thread does not panic");

    let declared = renderer
        .capabilities()
        .supports_frame_contract(hardware_contract);
    assert_eq!(
        prepared_hardware.is_ok(),
        declared,
        "hardware import is accepted exactly when the driver declared hardware contracts: \
         {prepared_hardware:?}"
    );
    if let Err(error) = prepared_hardware {
        assert_eq!(
            error,
            VideoImportPrepareError::PathUnsupported {
                path: VideoImportPath::HardwareZeroCopy
            }
        );
    }
}

/// Новая установка заменяет импорт целиком: кадр прежнего декодера через новый импорт не
/// находится (`Missing`), а не рисуется чужой картинкой.
#[test]
fn installing_a_new_decoder_replaces_the_active_import() {
    let Some(mut renderer) = offscreen_or_skip(
        "installing_a_new_decoder_replaces_the_active_import",
        TARGET,
    ) else {
        return;
    };
    let first: ConnectedDecoder = connect_software_decoder(&mut renderer);
    let first_lease = first.lease(picture(1, 180, 60));
    let second: ConnectedDecoder = connect_software_decoder(&mut renderer);

    let outcome = renderer.video_frames().materialize(&first_lease).outcome;

    assert!(
        matches!(
            outcome,
            render_shell_api::FrameMaterializationOutcome::Missing
        ),
        "the new import knows nothing of the old decoder frames: {outcome:?}"
    );
    let (key, _lease) = materialize_key(&mut renderer, &second, picture(2, 30, 220));
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(pixel(&renderer, 16, 16), 30, "the new decoder frame");
}
