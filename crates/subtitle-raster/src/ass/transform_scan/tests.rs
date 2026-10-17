use subtitle_core::{
    AssAxes, AssClip, AssClipMode, AssClipShape, AssColorSlot, AssFontSize, AssOverride, AssRgb,
    AssRotationAxis, AssShearAxis, AssTransformTiming, SubtitleTime,
};

use super::*;
use crate::ass::test_script::{
    cue_with, default_style, script_with, tags, text, transform, transform_range,
};
use crate::renderer::AssEventTiming;

fn cue_of(overrides: Vec<AssOverride>) -> AssCue {
    let script = script_with(vec![default_style()]);
    cue_with(&script, vec![tags(overrides), text("x")])
}

fn clock(elapsed_ms: i64, duration_ms: i64) -> AssEventClock {
    AssEventClock::new(
        SubtitleTime::from_millis(elapsed_ms),
        AssEventTiming::new(SubtitleTime::ZERO, SubtitleTime::from_millis(duration_ms)),
    )
}

fn at(ms: i64) -> AnimationChange {
    AnimationChange::At(SubtitleTime::from_millis(ms))
}

fn size(value: f64) -> AssOverride {
    AssOverride::FontSize(AssFontSize::Absolute(value))
}

fn rect_clip() -> AssOverride {
    AssOverride::Clip(AssClip {
        mode: AssClipMode::Inside,
        shape: AssClipShape::Rect {
            x0: 0,
            y0: 0,
            x1: 10,
            y1: 10,
        },
    })
}

fn vector_clip() -> AssOverride {
    AssOverride::Clip(AssClip {
        mode: AssClipMode::Inside,
        shape: AssClipShape::Vector {
            scale: 1,
            drawing: "m 0 0 l 5 5".to_owned(),
        },
    })
}

#[test]
fn only_tags_with_a_value_that_use_pwr_are_animated() {
    assert_eq!(effect_of(&size(50.0)), TransformEffect::Raster);
    assert_eq!(
        effect_of(&AssOverride::FontSize(AssFontSize::Relative(2.0))),
        TransformEffect::Raster
    );
    assert_eq!(
        effect_of(&AssOverride::Color {
            slot: AssColorSlot::Primary,
            value: Some(AssRgb::new(1, 2, 3)),
        }),
        TransformEffect::Raster
    );
    assert_eq!(
        effect_of(&AssOverride::Rotation {
            axis: AssRotationAxis::Z,
            degrees: Some(10.0),
        }),
        TransformEffect::Raster
    );
    assert_eq!(effect_of(&rect_clip()), TransformEffect::RectClip);
    // Без аргумента libass берёт стиль сразу (pwr = 1), векторный клип не анимируется.
    assert_eq!(
        effect_of(&AssOverride::FontSize(AssFontSize::Reset)),
        TransformEffect::None
    );
    assert_eq!(
        effect_of(&AssOverride::Border {
            axes: AssAxes::Both,
            value: None
        }),
        TransformEffect::None
    );
    assert_eq!(effect_of(&vector_clip()), TransformEffect::None);
    assert_eq!(
        effect_of(&AssOverride::Position { x: 1.0, y: 2.0 }),
        TransformEffect::None
    );
    assert_eq!(
        effect_of(&AssOverride::Italic(Some(true))),
        TransformEffect::None
    );
}

#[test]
fn raster_transform_weights_enter_the_key_in_tag_order() {
    let cue = cue_of(vec![
        transform_range(0, 1000, vec![size(50.0)]),
        transform_range(0, 2000, vec![size(60.0)]),
    ]);
    let digest = digest_transforms(&cue, &clock(500, 4000));
    assert_eq!(
        digest.raster_weights,
        vec![0.5_f64.to_bits(), 0.25_f64.to_bits()]
    );
    assert!(digest.time_dependent);
}

#[test]
fn clip_only_transform_stays_out_of_the_key_but_is_time_dependent() {
    let cue = cue_of(vec![transform_range(0, 1000, vec![rect_clip()])]);
    let digest = digest_transforms(&cue, &clock(500, 4000));
    assert!(digest.raster_weights.is_empty());
    assert!(digest.time_dependent);
    assert_eq!(digest.change(), AnimationChange::EveryFrame);
}

#[test]
fn mixed_transform_puts_only_the_weight_in_the_key() {
    let cue = cue_of(vec![transform_range(
        0,
        1000,
        vec![rect_clip(), size(50.0)],
    )]);
    let digest = digest_transforms(&cue, &clock(500, 4000));
    assert_eq!(digest.raster_weights, vec![0.5_f64.to_bits()]);
}

#[test]
fn transform_without_animated_tags_is_static() {
    // `\pos`, `\b` внутри `\t` применяются сразу: во времени картинка не меняется.
    let cue = cue_of(vec![transform_range(
        0,
        1000,
        vec![
            AssOverride::Italic(Some(true)),
            AssOverride::Position { x: 1.0, y: 2.0 },
        ],
    )]);
    let digest = digest_transforms(&cue, &clock(500, 4000));
    assert!(!digest.time_dependent);
    assert!(digest.raster_weights.is_empty());
    assert_eq!(digest.change(), AnimationChange::None);
    // Пустой (некорректный) `\t` тоже ни от чего не зависит.
    let empty = cue_of(vec![transform(AssTransformTiming::Default, Vec::new())]);
    assert!(!digest_transforms(&empty, &clock(0, 1000)).time_dependent);
}

#[test]
fn change_is_at_t1_plus_one_before_every_frame_during_and_none_after() {
    let cue = cue_of(vec![transform_range(500, 1500, vec![size(50.0)])]);
    let change = |ms: i64| digest_transforms(&cue, &clock(ms, 4000)).change();
    // До `t1` и на самом `t1` вес 0: картинка постоянна по `t1 + 1 мс`.
    assert_eq!(change(100), at(501));
    assert_eq!(change(500), at(501));
    // Внутри `[t1, t2)` — каждый кадр.
    assert_eq!(change(501), AnimationChange::EveryFrame);
    assert_eq!(change(1499), AnimationChange::EveryFrame);
    // С `t2` вес полный навсегда.
    assert_eq!(change(1500), AnimationChange::None);
    assert_eq!(change(9000), AnimationChange::None);
}

#[test]
fn zero_accel_changes_exactly_at_t1() {
    // pow(0, 0) = 1: вес прыгает на `t1`, поэтому до него картинка постоянна только строго.
    let timing = AssTransformTiming::RangeAccel {
        t1_ms: 500,
        t2_ms: 1500,
        accel: 0.0,
    };
    let cue = cue_of(vec![transform(timing, vec![size(50.0)])]);
    assert_eq!(digest_transforms(&cue, &clock(100, 4000)).change(), at(500));
}

#[test]
fn transform_with_t2_before_t1_jumps_at_t1() {
    let cue = cue_of(vec![transform_range(800, 100, vec![size(50.0)])]);
    let change = |ms: i64| digest_transforms(&cue, &clock(ms, 4000)).change();
    assert_eq!(change(0), at(800));
    assert_eq!(change(800), AnimationChange::None);
}

#[test]
fn zero_t2_runs_to_the_end_of_the_event() {
    let cue = cue_of(vec![transform_range(500, 0, vec![size(50.0)])]);
    let change = |ms: i64| digest_transforms(&cue, &clock(ms, 2000)).change();
    assert_eq!(change(1999), AnimationChange::EveryFrame);
    assert_eq!(change(2000), AnimationChange::None);
}

#[test]
fn several_transforms_report_the_earliest_change() {
    let cue = cue_of(vec![
        transform_range(1000, 2000, vec![size(50.0)]),
        transform_range(100, 600, vec![rect_clip()]),
    ]);
    let change = |ms: i64| digest_transforms(&cue, &clock(ms, 4000)).change();
    assert_eq!(change(0), at(101));
    assert_eq!(change(300), AnimationChange::EveryFrame);
    assert_eq!(change(700), at(1001));
}

#[test]
fn nested_transforms_are_visited_too() {
    let inner = transform_range(0, 400, vec![size(70.0)]);
    let cue = cue_of(vec![transform_range(0, 800, vec![rect_clip(), inner])]);
    let digest = digest_transforms(&cue, &clock(200, 4000));
    // Внешний двигает только клип, внутренний — размер: в ключе вес внутреннего (200/400).
    assert_eq!(digest.raster_weights, vec![0.5_f64.to_bits()]);
    assert!(animates_rect_clip(&cue));
}

#[test]
fn rect_clip_and_orientation_queries_look_inside_transforms_only() {
    assert!(!animates_rect_clip(&cue_of(vec![rect_clip()])));
    assert!(animates_rect_clip(&cue_of(vec![transform_range(
        0,
        10,
        vec![rect_clip()]
    )])));
    assert!(!animates_rect_clip(&cue_of(vec![transform_range(
        0,
        10,
        vec![vector_clip()]
    )])));

    let rotate = AssOverride::Rotation {
        axis: AssRotationAxis::Z,
        degrees: Some(30.0),
    };
    let shear = AssOverride::Shear {
        axis: AssShearAxis::X,
        factor: Some(0.5),
    };
    assert!(!animates_orientation(&cue_of(vec![rotate.clone()])));
    assert!(animates_orientation(&cue_of(vec![transform_range(
        0,
        10,
        vec![rotate]
    )])));
    assert!(animates_orientation(&cue_of(vec![transform_range(
        0,
        10,
        vec![shear]
    )])));
    assert!(!animates_orientation(&cue_of(vec![transform_range(
        0,
        10,
        vec![size(30.0)]
    )])));
}
