//! Чистая математика жестов: формулы с проверяемыми числами.

use egui::{pos2, vec2};

use super::harness::{layer_rect_px, picture_rect_px};
use crate::state::AppearanceEdit;
use crate::ui::subtitle_appearance_edit::gesture::{
    WHEEL_POINTS_PER_SIZE_STEP, WHEEL_SIZE_STEP_FACTOR, corner_scale_edit, frame_rect_points,
    move_edit, wheel_scale_edit,
};

#[test]
fn frame_rect_converts_pixels_to_points_and_pads_every_side() {
    let rect = frame_rect_points(layer_rect_px(), 2.0, 4.0).expect("valid layer");
    // 300 px / 2 = 150 pt, минус запас 4 pt; ширина 200 px = 100 pt плюс 2 * 4 pt.
    assert_eq!(rect.min, pos2(146.0, 221.0));
    assert_eq!(rect.size(), vec2(108.0, 28.0));
}

#[test]
fn frame_rect_rejects_nonsense_scale() {
    for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert_eq!(
            frame_rect_points(layer_rect_px(), scale, 4.0),
            None,
            "{scale}"
        );
    }
}

#[test]
fn move_is_the_pointer_shift_in_picture_fractions_with_vertical_flipped() {
    // 40 pt вправо и 20 pt вниз при масштабе 2: 80 px из 1600 и 40 px из 1000.
    let edit = move_edit(vec2(40.0, 20.0), 2.0, picture_rect_px()).expect("valid move");
    let AppearanceEdit::MoveBy {
        horizontal_fraction,
        vertical_fraction,
    } = edit
    else {
        panic!("expected MoveBy, got {edit:?}");
    };
    assert!((horizontal_fraction - 80.0 / 1600.0).abs() < 1e-6);
    // Вниз по экрану = вниз для субтитров = отрицательное «вверх».
    assert!((vertical_fraction + 40.0 / 1000.0).abs() < 1e-6);
}

#[test]
fn move_ignores_zero_shift_and_degenerate_picture() {
    assert_eq!(move_edit(vec2(0.0, 0.0), 1.0, picture_rect_px()), None);
    let mut empty_picture = picture_rect_px();
    empty_picture.width = 0.0;
    assert_eq!(move_edit(vec2(5.0, 5.0), 1.0, empty_picture), None);
    assert_eq!(move_edit(vec2(f32::NAN, 5.0), 1.0, picture_rect_px()), None);
}

#[test]
fn corner_factor_is_the_ratio_of_distances_from_the_frame_centre() {
    let centre = pos2(100.0, 100.0);
    let edit = corner_scale_edit(centre, pos2(150.0, 100.0), pos2(175.0, 100.0)).expect("outward");
    let AppearanceEdit::ScaleTextBy { factor } = edit else {
        panic!("expected ScaleTextBy, got {edit:?}");
    };
    assert!((factor - 1.5).abs() < 1e-6, "75 / 50 = 1.5, got {factor}");

    let inward = corner_scale_edit(centre, pos2(100.0, 200.0), pos2(100.0, 150.0)).expect("inward");
    let AppearanceEdit::ScaleTextBy { factor } = inward else {
        panic!("expected ScaleTextBy, got {inward:?}");
    };
    assert!((factor - 0.5).abs() < 1e-6);
}

#[test]
fn corner_ignores_motionless_pointer_and_the_dead_zone_around_the_centre() {
    let centre = pos2(100.0, 100.0);
    assert_eq!(
        corner_scale_edit(centre, pos2(150.0, 100.0), pos2(150.0, 100.0)),
        None
    );
    assert_eq!(
        corner_scale_edit(centre, pos2(101.0, 100.0), pos2(150.0, 100.0)),
        None
    );
}

#[test]
fn corner_factor_per_frame_is_limited_against_pointer_jumps() {
    let centre = pos2(0.0, 0.0);
    let edit = corner_scale_edit(centre, pos2(10.0, 0.0), pos2(1000.0, 0.0)).expect("jump");
    assert_eq!(edit, AppearanceEdit::ScaleTextBy { factor: 2.0 });
}

#[test]
fn wheel_step_is_ten_percent_per_forty_points_and_symmetric() {
    let up = wheel_scale_edit(WHEEL_POINTS_PER_SIZE_STEP).expect("scroll up");
    assert_eq!(
        up,
        AppearanceEdit::ScaleTextBy {
            factor: WHEEL_SIZE_STEP_FACTOR
        }
    );
    let AppearanceEdit::ScaleTextBy { factor: down } =
        wheel_scale_edit(-WHEEL_POINTS_PER_SIZE_STEP).expect("scroll down")
    else {
        panic!("expected ScaleTextBy");
    };
    assert!((down * WHEEL_SIZE_STEP_FACTOR - 1.0).abs() < 1e-6);
    assert_eq!(wheel_scale_edit(0.0), None);
    assert_eq!(wheel_scale_edit(f32::NAN), None);
}
