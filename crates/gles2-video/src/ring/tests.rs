use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

/// Fence, который тест «срабатывает» вручную.
#[derive(Debug, Default)]
struct ManualFence {
    signaled: AtomicBool,
    lost: AtomicBool,
}

impl ManualFence {
    fn signal(&self) {
        self.signaled.store(true, Ordering::SeqCst);
    }

    fn lose(&self) {
        self.lost.store(true, Ordering::SeqCst);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FenceLost;

impl FenceProbe for ManualFence {
    type Error = FenceLost;

    fn is_signaled(&self) -> Result<bool, Self::Error> {
        if self.lost.load(Ordering::SeqCst) {
            return Err(FenceLost);
        }
        Ok(self.signaled.load(Ordering::SeqCst))
    }
}

type TestRing = UploadRing<&'static str, ManualFence>;

fn three_slot_ring() -> TestRing {
    UploadRing::new(UploadSlotCount::DEFAULT)
}

fn free_slot(ring: &mut TestRing) -> WritableSlot {
    match ring.slot_for_write() {
        Ok(SlotForWrite::Free(slot)) => slot,
        other => panic!("expected a free slot, got {other:?}"),
    }
}

/// Записать кадр `key` и нарисовать его (как делает рендер за один кадр показа).
fn upload_and_draw(ring: &mut TestRing, key: u64) -> WritableSlot {
    let slot = free_slot(ring);
    ring.commit_upload(slot, VideoFrameKey(key));
    ring.mark_current_drawn();
    slot
}

#[test]
fn slot_count_requires_current_plus_one() {
    assert_eq!(UploadSlotCount::new(1), None);
    assert_eq!(UploadSlotCount::new(0), None);
    assert_eq!(UploadSlotCount::new(2).map(UploadSlotCount::get), Some(2));
    assert_eq!(UploadSlotCount::DEFAULT.get(), 3);
}

#[test]
fn uploads_rotate_through_slots_and_never_overwrite_the_current_frame() {
    let mut ring = three_slot_ring();
    let fence = Arc::new(ManualFence::default());
    let mut written = Vec::new();
    for key in 0..6 {
        let slot = upload_and_draw(&mut ring, key);
        assert_eq!(ring.current_key(), Some(VideoFrameKey(key)));
        written.push(slot);
        fence.signal();
        ring.retire(&fence);
    }
    assert_eq!(
        written,
        [0, 1, 2, 0, 1, 2].map(WritableSlot),
        "загрузки идут по кругу"
    );
}

#[test]
fn all_slots_in_unretired_work_report_missing_fence() {
    let mut ring = three_slot_ring();
    // Три кадра без retire: все три слота в неотданной работе.
    for key in 0..3 {
        upload_and_draw(&mut ring, key);
    }
    assert_eq!(
        ring.slot_for_write(),
        Ok(SlotForWrite::Busy(BusyReason::AwaitingSubmittedWorkFence))
    );
    // Текущий кадр не тронут отказом.
    assert_eq!(ring.current_key(), Some(VideoFrameKey(2)));
}

#[test]
fn pending_fence_keeps_slots_busy_until_it_signals() {
    let mut ring = three_slot_ring();
    let fence = Arc::new(ManualFence::default());
    for key in 0..3 {
        upload_and_draw(&mut ring, key);
    }
    ring.retire(&fence);
    assert_eq!(
        ring.slot_for_write(),
        Ok(SlotForWrite::Busy(BusyReason::GpuStillReading))
    );

    fence.signal();
    // Слот 0 освободился (после текущего слота 2 поиск идёт с 0).
    assert_eq!(
        ring.slot_for_write(),
        Ok(SlotForWrite::Free(WritableSlot(0)))
    );
}

#[test]
fn later_fence_replaces_earlier_one_for_the_same_slot() {
    let mut ring = UploadRing::new(UploadSlotCount::new(2).expect("two slots"));
    let early = Arc::new(ManualFence::default());
    let late = Arc::new(ManualFence::default());
    upload_and_draw(&mut ring, 1);
    ring.retire(&early);
    // Тот же кадр нарисован ещё раз: слот ждёт уже позднего fence.
    ring.mark_current_drawn();
    ring.retire(&late);
    upload_and_draw(&mut ring, 2);
    late.signal();
    ring.retire(&late);
    // Слот 0 ждал позднего fence, ранний (не сработавший) больше не держит его.
    assert_eq!(
        ring.slot_for_write(),
        Ok(SlotForWrite::Free(WritableSlot(0)))
    );
}

#[test]
fn fence_failure_is_an_error_not_a_free_slot() {
    let mut ring = three_slot_ring();
    let fence = Arc::new(ManualFence::default());
    for key in 0..3 {
        upload_and_draw(&mut ring, key);
    }
    ring.retire(&fence);
    fence.lose();
    assert_eq!(ring.slot_for_write(), Err(FenceLost));
}

#[test]
fn abandoned_upload_keeps_previous_frame_current_and_waits_for_fence() {
    let mut ring = three_slot_ring();
    let fence = Arc::new(ManualFence::default());
    upload_and_draw(&mut ring, 7);
    let broken = free_slot(&mut ring);
    ring.abandon_upload(broken);
    assert_eq!(ring.current_key(), Some(VideoFrameKey(7)));
    // Оборванная запись тоже «в неотданной работе»: после retire слот ждёт fence.
    ring.retire(&fence);
    let next = free_slot(&mut ring);
    assert_ne!(next, broken, "слот с оборванной записью ждёт fence");
}

#[test]
fn forgetting_keys_keeps_gpu_use() {
    let mut ring = three_slot_ring();
    for key in 0..3 {
        upload_and_draw(&mut ring, key);
    }
    ring.forget_keys();
    assert_eq!(ring.current_key(), None);
    assert_eq!(
        ring.slot_for_write(),
        Ok(SlotForWrite::Busy(BusyReason::AwaitingSubmittedWorkFence)),
        "забытый кадр всё ещё может читаться GPU"
    );
}

#[test]
fn textures_are_kept_per_slot_and_taken_once() {
    let mut ring = three_slot_ring();
    let slot = free_slot(&mut ring);
    if let Some(target) = ring.slot_mut(slot) {
        target.textures = Some("slot textures");
    }
    ring.commit_upload(slot, VideoFrameKey(1));
    assert_eq!(ring.current_textures(), Some(&"slot textures"));
    assert_eq!(ring.allocated_slots(), 1);
    assert_eq!(ring.take_all_textures(), vec!["slot textures"]);
    assert_eq!(ring.allocated_slots(), 0);
    assert_eq!(ring.current_key(), None);
}

#[test]
fn cleared_current_stops_being_current_but_the_gpu_still_guards_the_slot() {
    let mut ring = three_slot_ring();
    let shown = upload_and_draw(&mut ring, 1);
    assert_eq!(ring.current_key(), Some(VideoFrameKey(1)));
    ring.clear_current();
    assert_eq!(ring.current_key(), None);
    assert!(ring.current_textures().is_none());
    // Слот нарисованного кадра ещё в неотданной работе: писать в него нельзя, пока нет fence.
    let fence = Arc::new(ManualFence::default());
    ring.retire(&fence);
    let mut written = Vec::new();
    for key in 2..=3 {
        let slot = free_slot(&mut ring);
        assert_ne!(slot, shown, "the drawn slot is still read by the GPU");
        ring.commit_upload(slot, VideoFrameKey(key));
        ring.mark_current_drawn();
        ring.retire(&fence);
        written.push(slot);
    }
    assert!(matches!(ring.slot_for_write(), Ok(SlotForWrite::Busy(_))));
    fence.signal();
    assert!(matches!(ring.slot_for_write(), Ok(SlotForWrite::Free(_))));
}
