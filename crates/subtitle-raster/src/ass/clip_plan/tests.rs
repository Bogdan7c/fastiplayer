use subtitle_core::{
    AssClip, AssClipMode, AssClipShape, AssKerning, AssLayoutRes, AssOverride, AssPlayRes,
    AssScaledBorderAndShadow, AssScriptInfo, AssWrapStyle, AssYCbCrMatrix,
};

use super::*;
use crate::ass::options::AssStorageSize;

fn info(play: (i32, i32)) -> AssScriptInfo {
    AssScriptInfo {
        play_res: AssPlayRes::resolve(play.0, play.1),
        layout_res: None::<AssLayoutRes>,
        scaled_border_and_shadow: AssScaledBorderAndShadow::Enabled,
        wrap_style: AssWrapStyle::Smart,
        kerning: AssKerning::Disabled,
        ycbcr_matrix: AssYCbCrMatrix::Default,
        language: None,
    }
}

fn filters_with(tags: &[AssOverride]) -> FilterParams {
    let mut filters = FilterParams::default();
    for tag in tags {
        filters.apply(tag);
    }
    filters
}

fn rect_tag(mode: AssClipMode, corners: [i32; 4]) -> AssOverride {
    AssOverride::Clip(AssClip {
        mode,
        shape: AssClipShape::Rect {
            x0: corners[0],
            y0: corners[1],
            x1: corners[2],
            y1: corners[3],
        },
    })
}

fn plan_for(
    play: (i32, i32),
    canvas: (u32, u32),
    storage: AssStorageSize,
    tags: &[AssOverride],
    explicit: bool,
) -> EventClipPlan {
    let canvas = CanvasSize::new(canvas.0, canvas.1).expect("canvas");
    let frame = AssFrameScale::new(&info(play), canvas, storage);
    let scale = frame.event_scale(None);
    EventClipPlan::from_filters(&filters_with(tags), &frame, &scale, canvas, explicit)
}

/// Углы прямоугольника, как их видит клип (через публичное поведение: обрезка картинки).
fn rect_of(plan: &EventClipPlan) -> ScreenRectClip {
    plan.rect.expect("rect clip")
}

#[test]
fn no_clip_tags_give_no_clip() {
    let plan = plan_for((384, 288), (1280, 720), AssStorageSize::Unknown, &[], true);
    assert_eq!(plan, EventClipPlan::default());
}

#[test]
fn rect_corners_are_scaled_to_the_canvas_with_lround() {
    // 1280/384 = 3.333...: 100 -> 333.33 -> 333; 101 -> 336.67 -> 337; 144 -> 360.
    let plan = plan_for(
        (384, 288),
        (1280, 720),
        AssStorageSize::Unknown,
        &[rect_tag(AssClipMode::Inside, [100, 100, 101, 144])],
        true,
    );
    assert_eq!(
        rect_of(&plan),
        ScreenRectClip::new(
            AssClipMode::Inside,
            [333, 250, 337, 360],
            CanvasSize::new(1280, 720).expect("canvas")
        )
    );
}

#[test]
fn rect_corners_ignore_pixel_aspect_like_x2scr_pos_scaled() {
    // Хранение 4:3 на холсте 16:9 даёт par_scale_x = 4/3, но углы клипа его не делят.
    let storage = AssStorageSize::from_pixels(640, 480);
    let stretched = plan_for(
        (640, 360),
        (1280, 720),
        storage,
        &[rect_tag(AssClipMode::Outside, [10, 10, 100, 100])],
        true,
    );
    let square = plan_for(
        (640, 360),
        (1280, 720),
        AssStorageSize::Unknown,
        &[rect_tag(AssClipMode::Outside, [10, 10, 100, 100])],
        true,
    );
    assert_eq!(rect_of(&stretched), rect_of(&square));
}

#[test]
fn explicit_events_clamp_corners_to_the_frame_but_keep_their_order() {
    let canvas = CanvasSize::new(640, 360).expect("canvas");
    let plan = plan_for(
        (640, 360),
        (640, 360),
        AssStorageSize::Unknown,
        &[rect_tag(AssClipMode::Inside, [-50, -20, 9000, 8000])],
        true,
    );
    assert_eq!(
        rect_of(&plan),
        ScreenRectClip::new(AssClipMode::Inside, [0, 0, 640, 360], canvas)
    );
    // Не явная реплика (недостижимо при реальных тегах) не зажимается.
    let plan = plan_for(
        (640, 360),
        (640, 360),
        AssStorageSize::Unknown,
        &[rect_tag(AssClipMode::Inside, [-50, -20, 9000, 8000])],
        false,
    );
    assert_eq!(
        rect_of(&plan),
        ScreenRectClip::new(AssClipMode::Inside, [-50, -20, 9000, 8000], canvas)
    );
}

#[test]
fn vector_clip_carries_the_frame_scale_times_the_precision_weight() {
    let tag = AssOverride::Clip(AssClip {
        mode: AssClipMode::Outside,
        shape: AssClipShape::Vector {
            scale: 3,
            drawing: "m 0 0 l 4 0 4 4".to_owned(),
        },
    });
    let plan = plan_for(
        (640, 360),
        (1280, 720),
        AssStorageSize::Unknown,
        &[tag],
        true,
    );
    let vector = plan.vector.expect("vector clip");
    assert_eq!(vector.mode, AssClipMode::Outside);
    assert_eq!(vector.drawing, "m 0 0 l 4 0 4 4");
    // Экран 2x, точность 2^-(3-1) = 1/4.
    assert_eq!((vector.scale_x, vector.scale_y), (0.5, 0.5));
}

#[test]
fn vector_clip_with_scale_32_collapses_the_matrix_like_libass() {
    let tag = AssOverride::Clip(AssClip {
        mode: AssClipMode::Inside,
        shape: AssClipShape::Vector {
            scale: 32,
            drawing: "m 0 0".to_owned(),
        },
    });
    let plan = plan_for(
        (640, 360),
        (640, 360),
        AssStorageSize::Unknown,
        &[tag],
        true,
    );
    let vector = plan.vector.expect("vector clip");
    assert_eq!((vector.scale_x, vector.scale_y), (0.0, 0.0));
}
