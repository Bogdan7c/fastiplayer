//! Тесты плана реплики с `\t`: вид букв на момент показа, группа размещения, коллизии, клип.

use subtitle_core::{
    AssClip, AssClipMode, AssClipShape, AssFontSize, AssOverride, AssRotationAxis, AssShearAxis,
    SubtitleTime,
};

use super::*;
use crate::ass::animation::AssEventClock;
use crate::ass::options::AssRenderOptions;
use crate::ass::test_script::{cue_with, default_style, script_with, tags, text, transform_range};
use crate::renderer::AssEventTiming;

fn canvas() -> CanvasSize {
    CanvasSize::new(1280, 720).expect("canvas")
}

/// План реплики на `elapsed_ms` от её начала (событие длится 4 секунды).
fn plan_at(overrides: Vec<AssOverride>, elapsed_ms: i64) -> AssEventPlan {
    let script = script_with(vec![default_style()]);
    let cue = cue_with(&script, vec![tags(overrides), text("a")]);
    let clock = AssEventClock::new(
        SubtitleTime::from_millis(elapsed_ms),
        AssEventTiming::new(SubtitleTime::ZERO, SubtitleTime::from_millis(4000)),
    );
    let animation = AssAnimationState {
        clock: Some(clock),
        ..AssAnimationState::STILL
    };
    plan_ass_event(
        &cue,
        canvas(),
        &AssRenderOptions::default(),
        &SubtitleAppearance::outline_and_shadow(),
        &animation,
    )
}

fn first_look(plan: &AssEventPlan) -> &SpanLook {
    &plan.source.looks[plan.source.chars[0].look]
}

fn size_transform(t1_ms: i32, t2_ms: i32, size: f64) -> AssOverride {
    transform_range(
        t1_ms,
        t2_ms,
        vec![AssOverride::FontSize(AssFontSize::Absolute(size))],
    )
}

fn rect_clip(x1: i32) -> AssOverride {
    AssOverride::Clip(AssClip {
        mode: AssClipMode::Inside,
        shape: AssClipShape::Rect {
            x0: 0,
            y0: 0,
            x1,
            y1: 720,
        },
    })
}

#[test]
fn transform_scales_the_letters_between_the_start_and_the_target_size() {
    let overrides = vec![size_transform(0, 1000, 120.0)];
    let at = |ms| first_look(&plan_at(overrides.clone(), ms)).font_size;
    let start = at(0);
    let middle = at(500);
    let end = at(1000);
    // Размер шрифта 40 -> 120, середина (вес 0.5) — 80: размеры в пикселях пропорциональны.
    assert!(start < middle && middle < end, "{start} {middle} {end}");
    assert!((middle / start - 2.0).abs() < 1e-9, "{start} {middle}");
    assert!((end / start - 3.0).abs() < 1e-9, "{start} {end}");
    assert_eq!(at(1000), at(3000));
}

#[test]
fn transform_disables_collisions_even_when_it_changes_nothing() {
    let plain = plan_at(vec![], 0);
    assert!(plain.collision.detect);
    let animated = plan_at(vec![size_transform(0, 1000, 120.0)], 0);
    assert!(!animated.collision.detect);
    // Даже пустой (некорректный) `\t`: `detect_collisions = 0` стоит до проверки формы.
    let empty = plan_at(
        vec![AssOverride::Transform(subtitle_core::AssTransform {
            timing: subtitle_core::AssTransformTiming::Default,
            tags: Vec::new(),
        })],
        0,
    );
    assert!(!empty.collision.detect);
}

#[test]
fn transform_animating_rotation_or_shear_keeps_the_fixed_group_at_every_moment() {
    // В нулевой момент поворота ещё нет, но группа не должна меняться во времени.
    for nested in [
        AssOverride::Rotation {
            axis: AssRotationAxis::Z,
            degrees: Some(30.0),
        },
        AssOverride::Shear {
            axis: AssShearAxis::Y,
            factor: Some(0.3),
        },
    ] {
        for ms in [0, 500, 2000] {
            let result = plan_at(vec![transform_range(0, 1000, vec![nested.clone()])], ms);
            assert_eq!(
                result.placement_group,
                OverlayPlacementGroup::Fixed,
                "{nested:?} на {ms} мс"
            );
        }
    }
    let size_only = plan_at(vec![size_transform(0, 1000, 80.0)], 0);
    assert_eq!(size_only.placement_group, OverlayPlacementGroup::Main);
}

#[test]
fn clip_inside_transform_is_animated_after_the_cache_and_the_static_clip_is_not() {
    let animated = plan_at(vec![transform_range(0, 1000, vec![rect_clip(640)])], 500);
    assert!(animated.clip.animation.is_some());
    // План хранит клип на момент рисования: середина пути от всего PlayRes (1280) к 640.
    assert!(animated.clip.rect.is_some());

    let fixed = plan_at(vec![rect_clip(640)], 500);
    assert!(fixed.clip.animation.is_none());
}
