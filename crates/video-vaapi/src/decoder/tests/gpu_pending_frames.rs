//! Unit-тесты очереди «отправлено в GPU, ещё не отдано» (сессия 09a).
//!
//! Fake handles дают управляемую готовность surface, флаг вызова `sync()` и
//! флаг «decoder отпустил handle» (в production — возврат surface в пул).

use super::*;
use crate::decoder::gpu_pending_frames::{
    GpuPendingCompletionReason, GpuPendingFrame, GpuPendingFrames, GpuPendingSurvivorScope,
};

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

/// Контракт NV12, с которым кадры выходят из H.264/HEVC Main.
fn nv12_contract() -> DecodedSurfaceContract {
    DecodedSurfaceContract {
        format: DecodedPixelFormat::Nv12,
        bit_depth: BitDepth::Eight,
        chroma: ChromaSubsampling::Yuv420,
    }
}

/// Пробы одного fake-кадра: был ли blocking sync и отпустил ли decoder handle.
struct FrameProbes {
    sync_called: Rc<Cell<bool>>,
    released: Rc<Cell<bool>>,
}

/// Ставит fake-кадр в очередь; `generation` служит меткой кадра в проверках порядка.
fn enqueue_fake_frame(
    queue: &mut GpuPendingFrames,
    readiness: FakeSurfaceReadiness,
    generation: u64,
) -> FrameProbes {
    let (handle, sync_called, released) = fake_decoded_frame_handle_with_release_probe(readiness);
    queue.enqueue(handle, generation, nv12_contract());
    FrameProbes {
        sync_called,
        released,
    }
}

/// Метки кадров в порядке, в котором очередь их отдала.
fn generations(frames: &[GpuPendingFrame]) -> Vec<u64> {
    frames.iter().map(|frame| frame.generation).collect()
}

#[test]
fn frame_of_current_packet_is_not_waited_on_while_gpu_is_busy() {
    let mut queue = GpuPendingFrames::new(4, Arc::default());
    queue.begin_packet_submission();
    let probes = enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), 1);

    assert!(
        queue.pop_completable_after_packet_submit().is_none(),
        "a frame the GPU is still decoding must stay queued until the next packet is submitted"
    );
    assert!(
        !probes.sync_called.get(),
        "the queue must only query readiness, never block on sync"
    );
    assert_eq!(queue.depth(), 1);
    assert!(
        !probes.released.get(),
        "the frame must still be owned by the queue"
    );
}

#[test]
fn frame_of_earlier_packet_completes_once_next_packet_is_submitted() {
    let mut queue = GpuPendingFrames::new(4, Arc::default());
    queue.begin_packet_submission();
    enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), 1);

    // GPU получил следующий packet: ожидание прошлого кадра его не останавливает.
    queue.begin_packet_submission();
    let completed = queue
        .pop_completable_after_packet_submit()
        .expect("frame of the previous packet must be handed out for completion");

    assert_eq!(completed.generation, 1);
    assert!(queue.is_empty());
    let stats = queue.stats();
    assert_eq!(stats.completed_after_next_packet, 1);
    assert_eq!(stats.completed_already_ready, 0);
}

#[test]
fn frame_already_finished_by_gpu_completes_without_waiting_for_next_packet() {
    let mut queue = GpuPendingFrames::new(4, Arc::default());
    queue.begin_packet_submission();
    let probes = enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(true), 7);

    let completed = queue
        .pop_completable_after_packet_submit()
        .expect("a ready surface must not wait for the next packet");

    assert_eq!(completed.generation, 7);
    assert!(!probes.sync_called.get(), "readiness query must not block");
    assert_eq!(queue.stats().completed_already_ready, 1);
}

/// Правило простоя: декодеру нечего подавать, и неготовые кадры текущего packet-а
/// больше не ждут следующего. С 12a.1 на быстрых GPU кадры почти всегда приходят
/// готовыми, и аппаратный тест паузы это правило не всегда задевает, поэтому
/// очередь проверяется здесь детерминированно.
#[test]
fn decoder_idle_hands_out_every_unfinished_frame_in_order_and_counts_it_as_idle() {
    let mut queue = GpuPendingFrames::new(4, Arc::default());
    queue.begin_packet_submission();
    enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), 1);
    enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), 2);
    assert!(
        queue.pop_completable_after_packet_submit().is_none(),
        "unfinished frames of the current packet wait while packets keep coming"
    );

    let mut completed = Vec::new();
    while let Some(frame) = queue.pop_front_for(GpuPendingCompletionReason::DecoderIdle) {
        completed.push(frame);
    }

    assert_eq!(generations(&completed), vec![1, 2]);
    assert!(queue.is_empty());
    let stats = queue.stats();
    assert_eq!(stats.completed_on_idle, 2);
    assert_eq!(
        stats.completed_after_next_packet + stats.completed_already_ready,
        0
    );
}

#[test]
fn readiness_query_error_is_not_swallowed_and_hands_frame_to_sync_path() {
    let mut queue = GpuPendingFrames::new(4, Arc::default());
    queue.begin_packet_submission();
    enqueue_fake_frame(
        &mut queue,
        FakeSurfaceReadiness::QueryError("VA_STATUS_ERROR_INVALID_SURFACE"),
        3,
    );

    let completed = queue
        .pop_completable_after_packet_submit()
        .expect("query error must not leave the frame stuck in the queue");

    assert_eq!(completed.generation, 3);
    assert_eq!(
        queue.stats().completed_after_query_error,
        1,
        "query errors must be visible in counters, not treated as an ordinary ready surface"
    );
    assert_eq!(queue.stats().completed_already_ready, 0);
}

#[test]
fn ready_tail_never_overtakes_busy_head_so_presentation_order_is_kept() {
    let mut queue = GpuPendingFrames::new(4, Arc::default());
    queue.begin_packet_submission();
    enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), 1);
    enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(true), 2);

    assert!(
        queue.pop_completable_after_packet_submit().is_none(),
        "a ready frame behind a busy one must wait: frames leave in decoder output order"
    );

    queue.begin_packet_submission();
    let mut completed = Vec::new();
    while let Some(frame) = queue.pop_completable_after_packet_submit() {
        completed.push(frame);
    }
    assert_eq!(generations(&completed), vec![1, 2]);
}

#[test]
fn queue_over_capacity_hands_out_oldest_frame_and_stays_bounded() {
    let mut queue = GpuPendingFrames::new(2, Arc::default());
    queue.begin_packet_submission();
    for generation in 1..=3 {
        enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), generation);
    }

    let evicted = queue
        .pop_over_capacity()
        .expect("third frame exceeds capacity 2");
    assert_eq!(evicted.generation, 1, "the oldest frame is completed first");
    assert!(queue.pop_over_capacity().is_none());
    assert_eq!(queue.depth(), 2);
    let stats = queue.stats();
    assert_eq!(stats.completed_on_capacity, 1);
    assert_eq!(stats.peak_depth, 3);
    assert_eq!(stats.enqueued, 3);
}

#[test]
fn zero_capacity_is_normalized_to_one_frame() {
    let mut queue = GpuPendingFrames::new(0, Arc::default());
    queue.begin_packet_submission();
    enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), 1);

    assert!(queue.pop_over_capacity().is_none(), "one frame fits");
    enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), 2);
    assert_eq!(
        queue.pop_over_capacity().map(|frame| frame.generation),
        Some(1)
    );
}

#[test]
fn seek_flush_during_flight_discards_every_frame_and_releases_surfaces() {
    let mut queue = GpuPendingFrames::new(4, Arc::default());
    queue.begin_packet_submission();
    let first = enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), 1);
    queue.begin_packet_submission();
    let second = enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), 1);

    let discarded = queue.take_all_for_discard();
    assert_eq!(discarded.len(), 2);
    assert!(queue.is_empty());
    assert_eq!(queue.stats().discarded, 2);

    // Production выбрасывает кадр через `sync_discard_ready_frame`: sync, затем release.
    for frame in discarded {
        sync_discard_ready_frame(frame.handle, "flush").expect("fake sync succeeds");
    }
    for probes in [&first, &second] {
        assert!(
            probes.sync_called.get(),
            "discard must wait for the GPU first"
        );
        assert!(
            probes.released.get(),
            "discarded surface must return to the pool"
        );
    }
}

#[test]
fn end_of_stream_drain_hands_out_whole_queue_in_order() {
    let mut queue = GpuPendingFrames::new(8, Arc::default());
    queue.begin_packet_submission();
    for generation in 1..=4 {
        enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), generation);
    }

    let mut drained = Vec::new();
    while let Some(frame) = queue.pop_front_for(GpuPendingCompletionReason::LifecycleBoundary) {
        drained.push(frame);
    }

    assert_eq!(generations(&drained), vec![1, 2, 3, 4]);
    assert_eq!(queue.stats().completed_on_lifecycle, 4);
    assert!(queue.is_empty());
}

#[test]
fn format_change_keeps_frames_of_earlier_packets_and_discards_current_packet_frames() {
    let mut queue = GpuPendingFrames::new(8, Arc::default());
    queue.begin_packet_submission();
    enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), 1);
    queue.begin_packet_submission();
    enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), 2);
    enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), 3);

    let discarded =
        queue.take_non_survivors_for_discard(GpuPendingSurvivorScope::EarlierPacketsOnly);

    assert_eq!(
        generations(&discarded),
        vec![2, 3],
        "frames of the current decode call were released by this boundary before 09a too"
    );
    assert_eq!(queue.depth(), 1, "the earlier packet's frame survives");
    assert_eq!(
        queue
            .pop_front_for(GpuPendingCompletionReason::LifecycleBoundary)
            .map(|frame| frame.generation),
        Some(1)
    );
    assert_eq!(queue.stats().discarded, 2);
}

#[test]
fn stream_reconfigure_keeps_all_pending_frames() {
    let mut queue = GpuPendingFrames::new(8, Arc::default());
    queue.begin_packet_submission();
    enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), 1);
    enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(false), 2);

    let discarded = queue.take_non_survivors_for_discard(GpuPendingSurvivorScope::AllPending);

    assert!(
        discarded.is_empty(),
        "configure_stream arrives between decode calls: every pending frame was already decoded"
    );
    assert_eq!(queue.depth(), 2);
    assert_eq!(queue.stats().discarded, 0);
}

#[test]
fn dropping_handed_out_frame_returns_its_surface() {
    let mut queue = GpuPendingFrames::new(4, Arc::default());
    queue.begin_packet_submission();
    let probes = enqueue_fake_frame(&mut queue, FakeSurfaceReadiness::Ready(true), 1);

    let frame = queue
        .pop_completable_after_packet_submit()
        .expect("ready frame is handed out");
    assert!(
        !probes.released.get(),
        "handed-out frame is owned by the caller now"
    );
    drop(frame);

    assert!(
        probes.released.get(),
        "an error path that drops the frame must still release the VA surface"
    );
}
