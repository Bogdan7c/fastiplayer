//! Планировщик частичного показа: чистая логика, результаты решений.

use super::{
    BufferAge, DAMAGE_HISTORY_FRAMES, DamageHistory, FullRepaintReason, LayersDamage, PixelRect,
    RepaintPlan, TargetArea, union_damage,
};
use crate::context::driver::SurfaceSize;

fn target() -> SurfaceSize {
    match SurfaceSize::new(100, 50) {
        Some(size) => size,
        None => panic!("100×50 is a valid size"),
    }
}

fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
    PixelRect {
        x,
        y,
        width,
        height,
    }
}

/// История, в которой показаны кадры с заданными повреждениями (старейший первым).
fn history_of(damages: &[Option<PixelRect>]) -> DamageHistory {
    let mut history = DamageHistory::new();
    for damage in damages {
        history.record_presented(target(), TargetArea::from_damage(*damage));
    }
    history
}

fn plan(history: &DamageHistory, age: BufferAge, ui: Option<PixelRect>) -> RepaintPlan {
    history.plan(age, target(), LayersDamage::Unchanged, ui)
}

#[test]
fn age_one_repaints_only_current_damage() {
    let history = history_of(&[Some(rect(0, 0, 10, 10)), Some(rect(50, 0, 10, 10))]);
    let current = rect(20, 20, 5, 5);
    assert_eq!(
        plan(&history, BufferAge::Frames(1), Some(current)),
        RepaintPlan::Partial {
            repaint: TargetArea::Region(current),
            changed: TargetArea::Region(current),
        }
    );
}

#[test]
fn age_two_adds_the_previous_frame_damage() {
    // Кадры: A (0,0,10,10), B (50,0,10,10). Текущий C (20,20,5,5). Буфер возраста 2 содержит
    // кадр A, то есть отличается от C на повреждения B и C.
    let history = history_of(&[Some(rect(0, 0, 10, 10)), Some(rect(50, 0, 10, 10))]);
    let current = rect(20, 20, 5, 5);
    assert_eq!(
        plan(&history, BufferAge::Frames(2), Some(current)),
        RepaintPlan::Partial {
            repaint: TargetArea::Region(rect(20, 0, 40, 25)),
            changed: TargetArea::Region(current),
        },
        "bounding box of B and C only; A is already in the buffer"
    );
}

#[test]
fn age_three_adds_two_previous_frames() {
    let history = history_of(&[
        Some(rect(0, 0, 10, 10)),
        Some(rect(50, 0, 10, 10)),
        Some(rect(0, 40, 10, 10)),
    ]);
    let current = rect(20, 20, 5, 5);
    assert_eq!(
        plan(&history, BufferAge::Frames(3), Some(current)),
        RepaintPlan::Partial {
            repaint: TargetArea::Region(rect(0, 0, 60, 50)),
            changed: TargetArea::Region(current),
        }
    );
}

#[test]
fn history_union_matters_empty_current_still_repaints_older_damage() {
    let history = history_of(&[Some(rect(0, 0, 10, 10)), Some(rect(50, 0, 10, 10))]);
    // Текущий кадр ничего не менял (`Reused` UI), но буфер возраста 2 отстаёт на кадр B.
    assert_eq!(
        plan(&history, BufferAge::Frames(2), None),
        RepaintPlan::Partial {
            repaint: TargetArea::Region(rect(50, 0, 10, 10)),
            changed: TargetArea::Nothing,
        }
    );
}

#[test]
fn empty_damage_with_age_one_repaints_nothing() {
    let history = history_of(&[Some(rect(0, 0, 10, 10))]);
    assert_eq!(
        plan(&history, BufferAge::Frames(1), None),
        RepaintPlan::Partial {
            repaint: TargetArea::Nothing,
            changed: TargetArea::Nothing,
        }
    );
}

#[test]
fn unsupported_and_undefined_age_force_full_repaint() {
    let history = history_of(&[Some(rect(0, 0, 10, 10))]);
    assert_eq!(
        plan(&history, BufferAge::Unsupported, None),
        RepaintPlan::Full(FullRepaintReason::BufferAgeUnsupported)
    );
    assert_eq!(
        plan(&history, BufferAge::Undefined, None),
        RepaintPlan::Full(FullRepaintReason::BufferAgeUndefined)
    );
    assert_eq!(BufferAge::from_egl_value(0), BufferAge::Undefined);
    assert_eq!(BufferAge::from_egl_value(3), BufferAge::Frames(3));
}

#[test]
fn buffer_older_than_history_forces_full_repaint() {
    // Помним столько кадров, сколько показано; возраст 3 требует двух предыдущих.
    let history = history_of(&[Some(rect(0, 0, 10, 10))]);
    assert_eq!(
        plan(&history, BufferAge::Frames(3), Some(rect(1, 1, 1, 1))),
        RepaintPlan::Full(FullRepaintReason::BufferTooOld)
    );
    let long = history_of(&[Some(rect(0, 0, 1, 1)); DAMAGE_HISTORY_FRAMES + 3]);
    let oldest_supported = u32::try_from(DAMAGE_HISTORY_FRAMES + 1).unwrap_or(u32::MAX);
    assert!(matches!(
        plan(&long, BufferAge::Frames(oldest_supported), None),
        RepaintPlan::Partial { .. }
    ));
    assert_eq!(
        plan(&long, BufferAge::Frames(oldest_supported + 1), None),
        RepaintPlan::Full(FullRepaintReason::BufferTooOld)
    );
}

#[test]
fn resize_layers_change_and_abandoned_frame_force_full_repaint() {
    let mut history = history_of(&[Some(rect(0, 0, 10, 10))]);
    let other_size = match SurfaceSize::new(120, 50) {
        Some(size) => size,
        None => panic!("120×50 is a valid size"),
    };
    assert_eq!(
        history.plan(
            BufferAge::Frames(1),
            other_size,
            LayersDamage::Unchanged,
            None
        ),
        RepaintPlan::Full(FullRepaintReason::TargetResized)
    );
    assert_eq!(
        history.plan(BufferAge::Frames(1), target(), LayersDamage::Changed, None),
        RepaintPlan::Full(FullRepaintReason::LayersChanged)
    );
    history.record_abandoned();
    assert_eq!(
        plan(&history, BufferAge::Frames(1), None),
        RepaintPlan::Full(FullRepaintReason::PreviousFrameAbandoned)
    );
    // Показанный кадр снимает метку.
    history.record_presented(target(), TargetArea::Whole);
    assert!(matches!(
        plan(&history, BufferAge::Frames(1), None),
        RepaintPlan::Partial { .. }
    ));
}

#[test]
fn full_repaint_is_remembered_as_whole_target_damage() {
    let mut history = history_of(&[Some(rect(0, 0, 10, 10))]);
    history.record_presented(target(), TargetArea::Whole);
    // Буфер возраста 2 старее полной перерисовки: перерисовать надо всё.
    assert_eq!(
        plan(&history, BufferAge::Frames(2), Some(rect(1, 1, 2, 2))),
        RepaintPlan::Full(FullRepaintReason::DamageCoversTarget)
    );
    // Буфер возраста 1 — это и есть полный кадр.
    assert!(matches!(
        plan(&history, BufferAge::Frames(1), Some(rect(1, 1, 2, 2))),
        RepaintPlan::Partial { .. }
    ));
}

#[test]
fn rect_algebra_ignores_empty_operands_and_disjoint_intersections() {
    let a = rect(0, 0, 10, 10);
    assert_eq!(a.intersection(rect(10, 0, 5, 5)), None);
    assert_eq!(a.intersection(rect(5, 5, 10, 10)), Some(rect(5, 5, 5, 5)));
    assert_eq!(a.bounding_union(rect(7, 7, 0, 0)), a);
    assert_eq!(union_damage(None, None), None);
    assert_eq!(union_damage(Some(a), None), Some(a));
    assert_eq!(
        TargetArea::Region(a).clip(rect(5, 5, 20, 20)),
        Some(rect(5, 5, 5, 5))
    );
    assert_eq!(TargetArea::Nothing.clip(a), None);
    assert_eq!(TargetArea::Whole.clip(a), Some(a));
    assert_eq!(TargetArea::Region(a).pixels(target()), 100);
    assert_eq!(TargetArea::Whole.pixels(target()), 5000);
}

#[test]
fn stale_buffer_full_repaint_is_recorded_as_its_ui_damage_not_the_whole_target() {
    let ui = Some(rect(5, 5, 4, 4));
    // Полный кадр только из-за возраста буфера: на экране изменился лишь интерфейс.
    for reason in [
        FullRepaintReason::BufferAgeUnsupported,
        FullRepaintReason::BufferAgeUndefined,
        FullRepaintReason::BufferTooOld,
        FullRepaintReason::DamageCoversTarget,
    ] {
        assert_eq!(
            RepaintPlan::Full(reason).presented_change(LayersDamage::Unchanged, ui),
            TargetArea::Region(rect(5, 5, 4, 4)),
            "{reason:?}"
        );
        // Но если менялись слои, то менялось всё.
        assert_eq!(
            RepaintPlan::Full(reason).presented_change(LayersDamage::Changed, ui),
            TargetArea::Whole,
            "{reason:?}"
        );
    }
    for reason in [
        FullRepaintReason::TargetResized,
        FullRepaintReason::PreviousFrameAbandoned,
        FullRepaintReason::LayersChanged,
    ] {
        assert_eq!(
            RepaintPlan::Full(reason).presented_change(LayersDamage::Unchanged, ui),
            TargetArea::Whole,
            "{reason:?}"
        );
    }
}

#[test]
fn stats_summary_names_every_reason_and_the_pixel_ratio() {
    let mut stats = super::PartialPresentStats::default();
    let size = target();
    stats.record_presented(
        RepaintPlan::Partial {
            repaint: TargetArea::Region(rect(0, 0, 10, 10)),
            changed: TargetArea::Region(rect(0, 0, 10, 10)),
        },
        size,
        true,
    );
    stats.record_presented(
        RepaintPlan::Full(FullRepaintReason::LayersChanged),
        size,
        false,
    );
    let line = stats.to_string();
    assert!(
        line.contains("partial presents 1 (damage swaps 1)"),
        "{line}"
    );
    assert!(line.contains("full presents 1"), "{line}");
    assert!(line.contains("LayersChanged: 1"), "{line}");
    assert!(line.contains("BufferAgeUnsupported: 0"), "{line}");
    assert!(
        line.contains("repainted 5100 of 10000 target pixels"),
        "{line}"
    );
}
