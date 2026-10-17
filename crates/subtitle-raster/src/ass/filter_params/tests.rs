use subtitle_core::{AssClip, AssClipMode, AssClipShape, AssOverride};

use super::*;

fn rect(mode: AssClipMode, x0: i32, y0: i32, x1: i32, y1: i32) -> AssOverride {
    AssOverride::Clip(AssClip {
        mode,
        shape: AssClipShape::Rect { x0, y0, x1, y1 },
    })
}

fn vector(mode: AssClipMode, scale: i32, drawing: &str) -> AssOverride {
    AssOverride::Clip(AssClip {
        mode,
        shape: AssClipShape::Vector {
            scale,
            drawing: drawing.to_owned(),
        },
    })
}

#[test]
fn blur_radius_is_clamped_and_missing_argument_resets_to_zero() {
    let mut params = FilterParams::default();
    params.apply(&AssOverride::Blur(Some(2.5)));
    assert_eq!(params.blur_radius(), 2.5);
    params.apply(&AssOverride::Blur(Some(1000.0)));
    assert_eq!(params.blur_radius(), ASS_BLUR_MAX_RADIUS);
    params.apply(&AssOverride::Blur(Some(-3.0)));
    assert_eq!(params.blur_radius(), 0.0);
    params.apply(&AssOverride::Blur(Some(4.0)));
    params.apply(&AssOverride::Blur(None));
    assert_eq!(params.blur_radius(), 0.0);
}

#[test]
fn edge_blur_rounds_half_up_and_clamps_like_libass() {
    let cases = [
        (0.4, 0),
        (0.5, 1),
        (1.49, 1),
        (2.5, 3),
        // VSFilter всегда прибавляет 0.5, даже к отрицательному: -0.4 -> 0.1 -> 0.
        (-0.4, 0),
        (-3.0, 0),
        (127.0, 127),
        (500.0, ASS_EDGE_BLUR_MAX),
        // dtoi32 libass: значение вне i32 даёт INT32_MIN, то есть 0, а не 127.
        (1.0e20, 0),
        (f64::INFINITY, 0),
    ];
    for (raw, expected) in cases {
        let mut params = FilterParams::default();
        params.apply(&AssOverride::EdgeBlur(Some(raw)));
        assert_eq!(params.edge_blur(), expected, "\\be{raw}");
    }
    let mut params = FilterParams::default();
    params.apply(&AssOverride::EdgeBlur(Some(5.0)));
    params.apply(&AssOverride::EdgeBlur(None));
    assert_eq!(params.edge_blur(), 0);
}

#[test]
fn rect_clip_last_wins_and_vector_clip_first_wins_independently() {
    let mut params = FilterParams::default();
    params.apply(&rect(AssClipMode::Inside, 1, 2, 3, 4));
    params.apply(&vector(AssClipMode::Outside, 2, "m 0 0 l 5 5"));
    params.apply(&rect(AssClipMode::Outside, 10, 20, 30, 40));
    params.apply(&vector(AssClipMode::Inside, 1, "m 9 9"));
    assert_eq!(
        params.rect_clip(),
        Some(RectClip {
            mode: AssClipMode::Outside,
            x0: 10,
            y0: 20,
            x1: 30,
            y1: 40
        })
    );
    assert_eq!(
        params.vector_clip(),
        Some(&VectorClip {
            mode: AssClipMode::Outside,
            scale: 2,
            drawing: "m 0 0 l 5 5".to_owned()
        })
    );
}

#[test]
fn reset_blur_keeps_clips_and_baseline_offset() {
    let mut params = FilterParams::default();
    params.apply(&AssOverride::Blur(Some(3.0)));
    params.apply(&AssOverride::EdgeBlur(Some(2.0)));
    params.apply(&AssOverride::DrawingBaselineOffset(-4.5));
    params.apply(&rect(AssClipMode::Inside, 0, 0, 5, 5));
    params.reset_blur();
    assert_eq!(params.blur_radius(), 0.0);
    assert_eq!(params.edge_blur(), 0);
    assert_eq!(params.baseline_offset(), -4.5);
    assert!(params.rect_clip().is_some());
}

#[test]
fn unrelated_tags_do_not_change_filter_params() {
    let mut params = FilterParams::default();
    params.apply(&AssOverride::Blur(Some(2.0)));
    let before = params.clone();
    params.apply(&AssOverride::ResetScale);
    assert_eq!(params, before);
}
