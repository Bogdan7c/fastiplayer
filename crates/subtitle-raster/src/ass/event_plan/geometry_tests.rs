//! Тесты плана реплики для статической геометрии слоя 2: повороты, наклон, `\org`.
//! Проверяется результат планирования (виды символов, центр поворота, коллизии, группа
//! размещения), а не вызов функций.

use subtitle_core::{AssOverride, AssRotationAxis, AssShearAxis};

use super::*;
use crate::ass::options::AssRenderOptions;
use crate::ass::test_script::{cue_with, default_style, script_with, tags, text};
use crate::event_style::RotationOrigin;
use crate::glyph_transform::{Rotation3d, ShearFactors};

fn canvas(width: u32, height: u32) -> CanvasSize {
    CanvasSize::new(width, height).expect("canvas")
}

fn plan_on(items: Vec<AssTextItem>, canvas: CanvasSize) -> AssEventPlan {
    let script = script_with(vec![default_style()]);
    plan_ass_event(
        &cue_with(&script, items),
        canvas,
        &AssRenderOptions::default(),
        &SubtitleAppearance::outline_and_shadow(),
        &AssAnimationState::STILL,
    )
}

fn plan(items: Vec<AssTextItem>) -> AssEventPlan {
    plan_on(items, canvas(1280, 720))
}

fn rotate(axis: AssRotationAxis, degrees: Option<f64>) -> AssOverride {
    AssOverride::Rotation { axis, degrees }
}

fn rotation_of_first_char(plan: &AssEventPlan) -> Rotation3d {
    plan.source.looks[plan.source.chars[0].look].rotation
}

#[test]
fn rotation_and_shear_tags_reach_the_look_of_the_following_text() {
    let result = plan(vec![
        tags(vec![
            rotate(AssRotationAxis::X, Some(10.0)),
            rotate(AssRotationAxis::Y, Some(20.0)),
            rotate(AssRotationAxis::Z, Some(30.0)),
            AssOverride::Shear {
                axis: AssShearAxis::X,
                factor: Some(0.5),
            },
            AssOverride::Shear {
                axis: AssShearAxis::Y,
                factor: Some(-0.25),
            },
        ]),
        text("a"),
    ]);
    let look = &result.source.looks[result.source.chars[0].look];
    assert_eq!(
        look.rotation,
        Rotation3d {
            x_degrees: 10.0,
            y_degrees: 20.0,
            z_degrees: 30.0
        }
    );
    assert_eq!(
        look.shear,
        ShearFactors {
            horizontal: 0.5,
            vertical: -0.25
        }
    );
}

#[test]
fn text_before_the_tag_stays_flat_and_gets_its_own_look() {
    let result = plan(vec![
        text("a"),
        tags(vec![rotate(AssRotationAxis::Z, Some(45.0))]),
        text("b"),
    ]);
    let looks: Vec<_> = result.source.chars.iter().map(|c| c.look).collect();
    assert_ne!(looks[0], looks[1], "смена угла начинает новый участок");
    assert_eq!(
        result.source.looks[looks[0]].rotation,
        Rotation3d::default()
    );
    assert_eq!(result.source.looks[looks[1]].rotation.z_degrees, 45.0);
}

#[test]
fn frz_without_argument_returns_the_style_angle_and_frx_fry_return_zero() {
    let mut style = default_style();
    style.angle = 15.0;
    let script = script_with(vec![style]);
    let cue = cue_with(
        &script,
        vec![
            tags(vec![
                rotate(AssRotationAxis::X, Some(60.0)),
                rotate(AssRotationAxis::Y, Some(70.0)),
                rotate(AssRotationAxis::Z, Some(80.0)),
                rotate(AssRotationAxis::X, None),
                rotate(AssRotationAxis::Y, None),
                rotate(AssRotationAxis::Z, None),
            ]),
            text("a"),
        ],
    );
    let result = plan_ass_event(
        &cue,
        canvas(1280, 720),
        &AssRenderOptions::default(),
        &SubtitleAppearance::outline_and_shadow(),
        &AssAnimationState::STILL,
    );
    assert_eq!(
        rotation_of_first_char(&result),
        Rotation3d {
            x_degrees: 0.0,
            y_degrees: 0.0,
            z_degrees: 15.0
        }
    );
}

#[test]
fn style_angle_rotates_text_without_any_tag_and_reset_restores_it() {
    let mut style = default_style();
    style.angle = 20.0;
    let script = script_with(vec![style]);
    let cue = cue_with(
        &script,
        vec![
            text("a"),
            tags(vec![
                rotate(AssRotationAxis::Z, Some(90.0)),
                AssOverride::Shear {
                    axis: AssShearAxis::X,
                    factor: Some(1.0),
                },
            ]),
            text("b"),
            tags(vec![AssOverride::Reset(None)]),
            text("c"),
        ],
    );
    let result = plan_ass_event(
        &cue,
        canvas(1280, 720),
        &AssRenderOptions::default(),
        &SubtitleAppearance::outline_and_shadow(),
        &AssAnimationState::STILL,
    );
    let look_of = |index: usize| &result.source.looks[result.source.chars[index].look];
    assert_eq!(look_of(0).rotation.z_degrees, 20.0);
    assert_eq!(look_of(1).rotation.z_degrees, 90.0);
    assert_eq!(look_of(1).shear.horizontal, 1.0);
    assert_eq!(
        look_of(2).rotation.z_degrees,
        20.0,
        "\\r сбрасывает угол к стилю"
    );
    assert_eq!(
        look_of(2).shear,
        ShearFactors::default(),
        "\\r сбрасывает наклон"
    );
}

#[test]
fn shear_without_argument_resets_to_zero() {
    let result = plan(vec![
        tags(vec![
            AssOverride::Shear {
                axis: AssShearAxis::Y,
                factor: Some(0.7),
            },
            AssOverride::Shear {
                axis: AssShearAxis::Y,
                factor: None,
            },
        ]),
        text("a"),
    ]);
    assert_eq!(
        result.source.looks[0].shear,
        ShearFactors::default(),
        "\\fay без аргумента — 0"
    );
}

#[test]
fn non_finite_angles_are_neutralised_instead_of_breaking_the_pipeline() {
    let result = plan(vec![
        tags(vec![
            rotate(AssRotationAxis::Z, Some(f64::NAN)),
            AssOverride::Shear {
                axis: AssShearAxis::X,
                factor: Some(f64::INFINITY),
            },
        ]),
        text("a"),
    ]);
    let look = &result.source.looks[0];
    assert_eq!(look.rotation, Rotation3d::default());
    assert_eq!(look.shear, ShearFactors::default());
}

#[test]
fn origin_becomes_a_scaled_canvas_point_and_the_first_origin_wins() {
    // Сценарий 1280x720 на холсте 640x360: всё вдвое меньше.
    let result = plan_on(
        vec![
            tags(vec![
                AssOverride::Origin { x: 400.0, y: 200.0 },
                AssOverride::Origin { x: 10.0, y: 10.0 },
            ]),
            text("a"),
        ],
        canvas(640, 360),
    );
    let EventPlacement::Exact(exact) = result.style.placement else {
        panic!("exact placement");
    };
    assert_eq!(
        exact.rotation_origin,
        RotationOrigin::AtPoint { x: 200.0, y: 100.0 }
    );
}

#[test]
fn without_origin_rotation_centre_follows_the_alignment() {
    let result = plan(vec![text("a")]);
    let EventPlacement::Exact(exact) = result.style.placement else {
        panic!("exact placement");
    };
    assert_eq!(exact.rotation_origin, RotationOrigin::ByAlignment);
}

#[test]
fn origin_switches_collision_detection_off_and_moves_the_event_to_the_fixed_group() {
    let plain = plan(vec![text("a")]);
    assert!(plain.collision.detect);
    assert_eq!(plain.placement_group, OverlayPlacementGroup::Main);

    let with_origin = plan(vec![
        tags(vec![AssOverride::Origin { x: 1.0, y: 2.0 }]),
        text("a"),
    ]);
    assert!(!with_origin.collision.detect);
    assert_eq!(with_origin.placement_group, OverlayPlacementGroup::Fixed);
}

#[test]
fn rotated_or_sheared_text_without_origin_still_collides_but_is_a_fixed_caption() {
    // libass отключает коллизии только у \pos, \move, \org и \t: повёрнутая реплика
    // ищет место как обычная, но это уже не диалог — группа «надписи».
    for tag in [
        rotate(AssRotationAxis::Z, Some(10.0)),
        rotate(AssRotationAxis::X, Some(10.0)),
        AssOverride::Shear {
            axis: AssShearAxis::X,
            factor: Some(0.2),
        },
    ] {
        let result = plan(vec![tags(vec![tag.clone()]), text("a")]);
        assert!(result.collision.detect, "{tag:?}");
        assert_eq!(
            result.placement_group,
            OverlayPlacementGroup::Fixed,
            "{tag:?}"
        );
    }
}

#[test]
fn rotation_reset_before_text_leaves_the_event_a_main_line() {
    let result = plan(vec![
        tags(vec![
            rotate(AssRotationAxis::Z, Some(10.0)),
            rotate(AssRotationAxis::Z, Some(0.0)),
        ]),
        text("a"),
    ]);
    assert_eq!(result.placement_group, OverlayPlacementGroup::Main);
}
