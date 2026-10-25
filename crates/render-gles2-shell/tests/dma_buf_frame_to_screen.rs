//! Функциональный тест аппаратного пути на настоящем EGL и настоящих буферах ядра
//! (`/dev/udmabuf`): кадр NV12 из DMA-BUF через `materialize_dma_buf_frame` доходит до экрана
//! без копии, а кадр декодера возвращается провайдеру только после fence показа.
//!
//! Провайдер ресурсов — подставной, но обёрнут `wrap_video_backend_for_gles2_release`, как его
//! получит плеер. Проверяются пиксели цели и журнал провайдера, а не вызовы.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use gles2_egui::FenceStatus;
use gles2_video::VideoDrawOutcome;
use render_gles2_shell::{
    DmaBufMaterializeOutcome, DmaBufShownFrom, DmaBufUnsupportedReason,
    dma_buf_import_capacity_for, materialize_dma_buf_frame,
};
use support::nv12_buffers::Nv12Layout;
use support::provider::{DescriptorReply, IdentityReply};
use support::{Rig, WAIT, assert_palette_on_screen, contract_for, decoded_frame};
use video_core::{DmaBufDescriptorRejection, FrameResourceHandle};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

fn materialize(rig: &mut Rig, frame: &video_core::DecodedFrame) -> DmaBufMaterializeOutcome {
    materialize_dma_buf_frame(&mut rig.video, &rig.context, &rig.provider, frame).outcome
}

fn assert_fresh_import(outcome: &DmaBufMaterializeOutcome) {
    assert!(
        matches!(
            outcome,
            DmaBufMaterializeOutcome::Shown(DmaBufShownFrom::FreshImport(_))
        ),
        "expected a fresh import, got {outcome:?}"
    );
}

/// Сценарий a: известные блоки цветов доходят до экрана, letterbox чёрный.
fn block_colors_reach_the_screen(layout: Nv12Layout, name: &str) {
    let Some(mut rig) = Rig::new(name, layout) else {
        return;
    };
    let frame = decoded_frame(contract_for(layout), 1);
    assert_fresh_import(&materialize(&mut rig, &frame));
    let presented = rig.draw_and_present(&frame);
    assert_eq!(presented.outcome, VideoDrawOutcome::Drawn { draw_calls: 1 });
    assert_palette_on_screen(&presented, &frame, name);
    let snapshot = rig.video.dma_buf_import_snapshot();
    assert_eq!((snapshot.imports, snapshot.entries), (1, 1));
    // Один дескриптор (один dup fd) на весь кадр, идентичность спрошена один раз.
    assert_eq!(rig.state.descriptor_lookups(), 1);
    assert_eq!(rig.state.identity_lookups(), 1);
    rig.video.destroy(&rig.context).unwrap();
}

#[test]
fn composed_nv12_frame_reaches_the_screen() {
    block_colors_reach_the_screen(
        Nv12Layout::Composed,
        "composed_nv12_frame_reaches_the_screen",
    );
}

#[test]
fn separate_nv12_frame_reaches_the_screen() {
    block_colors_reach_the_screen(
        Nv12Layout::Separate,
        "separate_nv12_frame_reaches_the_screen",
    );
}

/// Сценарий b: второй кадр той же поверхности (новый handle, тот же паспорт) показывается из
/// кэша БЕЗ запроса дескриптора, и картинка та же.
#[test]
fn second_frame_on_the_same_surface_is_a_cache_hit_without_a_descriptor() {
    let name = "second_frame_on_the_same_surface_is_a_cache_hit";
    let Some(mut rig) = Rig::new(name, Nv12Layout::Composed) else {
        return;
    };
    let contract = contract_for(rig.layout);
    assert_fresh_import(&materialize(&mut rig, &decoded_frame(contract, 1)));
    let descriptors_after_first = rig.state.descriptor_lookups();

    let second = decoded_frame(contract, 2);
    assert_eq!(
        materialize(&mut rig, &second),
        DmaBufMaterializeOutcome::Shown(DmaBufShownFrom::CacheHit)
    );
    assert_eq!(
        rig.state.descriptor_lookups(),
        descriptors_after_first,
        "a cache hit must not duplicate descriptors"
    );
    assert_eq!(rig.state.identity_lookups(), 2);
    let presented = rig.draw_and_present(&second);
    assert_eq!(presented.outcome, VideoDrawOutcome::Drawn { draw_calls: 1 });
    assert_palette_on_screen(&presented, &second, name);
    let snapshot = rig.video.dma_buf_import_snapshot();
    assert_eq!((snapshot.imports, snapshot.hits), (1, 1));

    // Тот же кадр ещё раз — ничего не меняется.
    assert_eq!(
        materialize(&mut rig, &second),
        DmaBufMaterializeOutcome::Shown(DmaBufShownFrom::AlreadyCurrent)
    );
    rig.video.destroy(&rig.context).unwrap();
}

/// Сценарий c: кадр отдаётся провайдеру ровно один раз и после fence; ждёт его поток-ожидатель
/// на настоящем `GpuFence`, а ничего больше не рисуется (UI «спит»).
#[test]
fn decoder_frame_is_released_after_the_real_fence_without_further_rendering() {
    let name = "decoder_frame_is_released_after_the_real_fence";
    let Some(mut rig) = Rig::new(name, Nv12Layout::Composed) else {
        return;
    };
    let frame = decoded_frame(contract_for(rig.layout), 1);
    assert_fresh_import(&materialize(&mut rig, &frame));
    let presented = rig.draw_and_present(&frame);
    assert_eq!(presented.outcome, VideoDrawOutcome::Drawn { draw_calls: 1 });

    // Плеер отпускает аренду кадра: запрос идёт в обёрнутый провайдер.
    rig.provider.release_frame(FrameResourceHandle(1));
    let right_after_request = rig.binding.stats();
    // Дальше UI не рисует ни одного кадра: возврат обязан прийти сам.
    assert!(
        rig.state.wait_for_release_count(1, WAIT),
        "frame was not released while the UI sleeps; stats {:?}",
        rig.binding.stats()
    );
    assert_eq!(rig.state.released_handles(), [1]);
    // К моменту возврата fence показа сработал.
    assert_eq!(
        presented.fence.wait(WAIT).unwrap(),
        FenceStatus::Signaled,
        "the frame went back before the GPU finished with it"
    );
    let stats = rig.binding.stats();
    assert_eq!(stats.released_without_fence, 0);
    assert_eq!(
        stats.released_by_waiter + stats.released_after_fence_signaled_fast,
        1,
        "exactly one release; stats {stats:?}"
    );
    assert_eq!(stats.pending_now, 0);
    if right_after_request.pending_now == 1 {
        // Кадр ждал fence в очереди: вернул его именно поток-ожидатель.
        assert_eq!(stats.released_by_waiter, 1);
    }
    // Повторного возврата не будет.
    assert!(
        !rig.state
            .wait_for_release_count(2, std::time::Duration::from_millis(100))
    );
    rig.video.destroy(&rig.context).unwrap();
}

/// Сценарий d: провайдер без идентичности — кэш всё равно работает по паспорту дескриптора:
/// второй кадр той же поверхности без импорта, но с запросом дескриптора (как у wgpu).
#[test]
fn provider_without_identity_still_avoids_the_second_import() {
    let name = "provider_without_identity_still_avoids_the_second_import";
    let Some(mut rig) = Rig::new(name, Nv12Layout::Separate) else {
        return;
    };
    rig.state.reply_to_identity(IdentityReply::Unsupported);
    let contract = contract_for(rig.layout);
    assert_fresh_import(&materialize(&mut rig, &decoded_frame(contract, 1)));
    let second = decoded_frame(contract, 2);
    assert_eq!(
        materialize(&mut rig, &second),
        DmaBufMaterializeOutcome::Shown(DmaBufShownFrom::CacheHit)
    );
    assert_eq!(
        rig.state.descriptor_lookups(),
        2,
        "no identity: a descriptor per frame"
    );
    assert_eq!(rig.video.dma_buf_import_snapshot().imports, 1);
    let presented = rig.draw_and_present(&second);
    assert_palette_on_screen(&presented, &second, name);
    rig.video.destroy(&rig.context).unwrap();
}

/// Сценарий e: контракт не совпал с дескриптором — честный отказ до драйвера: импорта нет,
/// кадр провайдеру не возвращён раньше времени, текущего кадра в видеослое нет.
#[test]
fn contract_mismatch_is_rejected_before_any_import() {
    let name = "contract_mismatch_is_rejected_before_any_import";
    let Some(mut rig) = Rig::new(name, Nv12Layout::Separate) else {
        return;
    };
    // Буферы separate, а кадр обещает composed.
    let wrong_layout = decoded_frame(
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
        1,
    );
    let outcome = materialize(&mut rig, &wrong_layout);
    assert!(
        matches!(
            outcome,
            DmaBufMaterializeOutcome::Unsupported(DmaBufUnsupportedReason::DescriptorRejected(
                DmaBufDescriptorRejection::ImageLayoutMismatch { .. }
            ))
        ),
        "{outcome:?}"
    );
    // Другой размер кадра при верной раскладке.
    let mut wrong_size = decoded_frame(contract_for(rig.layout), 2);
    wrong_size.width += 2;
    let outcome = materialize(&mut rig, &wrong_size);
    assert!(
        matches!(
            outcome,
            DmaBufMaterializeOutcome::Unsupported(DmaBufUnsupportedReason::DescriptorRejected(
                DmaBufDescriptorRejection::CodedSizeMismatch { .. }
            ))
        ),
        "{outcome:?}"
    );
    assert_eq!(rig.video.dma_buf_import_snapshot().imports, 0);
    assert_eq!(rig.video.dma_buf_import_snapshot().entries, 0);
    assert_eq!(
        rig.state.descriptor_lookups(),
        0,
        "rejected by identity, no dup fd"
    );
    assert!(rig.state.released_handles().is_empty());
    rig.video.destroy(&rig.context).unwrap();
}

/// Сценарий f: дескриптор памяти процесса на DMA-BUF пути — Unsupported, не ошибка.
#[test]
fn host_planar_descriptor_on_the_dma_buf_path_is_unsupported() {
    let name = "host_planar_descriptor_on_the_dma_buf_path_is_unsupported";
    let Some(mut rig) = Rig::new(name, Nv12Layout::Composed) else {
        return;
    };
    rig.state.reply_to_identity(IdentityReply::Unsupported);
    rig.state.reply_to_descriptor(DescriptorReply::HostPlanar);
    let frame = decoded_frame(contract_for(rig.layout), 1);
    assert_eq!(
        materialize(&mut rig, &frame),
        DmaBufMaterializeOutcome::Unsupported(DmaBufUnsupportedReason::HostPlanarDescriptor)
    );
    assert_eq!(rig.video.dma_buf_import_snapshot().imports, 0);
    rig.video.destroy(&rig.context).unwrap();
}

/// Сценарий g: Busy/Missing/Fatal провайдера — свои исходы и ни одного дескриптора.
#[test]
fn provider_busy_missing_and_fatal_never_reach_the_descriptor() {
    let name = "provider_busy_missing_and_fatal_never_reach_the_descriptor";
    let Some(mut rig) = Rig::new(name, Nv12Layout::Composed) else {
        return;
    };
    let frame = decoded_frame(contract_for(rig.layout), 1);
    for (reply, expected) in [
        (IdentityReply::Busy, DmaBufMaterializeOutcome::Busy),
        (IdentityReply::Missing, DmaBufMaterializeOutcome::Missing),
        (
            IdentityReply::Fatal,
            DmaBufMaterializeOutcome::ProviderFatal,
        ),
    ] {
        rig.state.reply_to_identity(reply);
        assert_eq!(materialize(&mut rig, &frame), expected);
    }
    assert_eq!(rig.state.descriptor_lookups(), 0);
    assert_eq!(rig.video.dma_buf_import_snapshot().imports, 0);
    rig.video.destroy(&rig.context).unwrap();
}

/// Ёмкость кэша = пул поверхностей декодера, применяется к видеослою.
#[test]
fn import_cache_capacity_follows_the_decoder_surface_pool() {
    let name = "import_cache_capacity_follows_the_decoder_surface_pool";
    let Some(mut rig) = Rig::new(name, Nv12Layout::Composed) else {
        return;
    };
    rig.video
        .set_dma_buf_import_capacity(&rig.context, dma_buf_import_capacity_for(24))
        .unwrap();
    assert_eq!(rig.video.dma_buf_import_snapshot().capacity, 24);
    assert_eq!(dma_buf_import_capacity_for(0).entries(), 1);
    rig.video.destroy(&rig.context).unwrap();
}
