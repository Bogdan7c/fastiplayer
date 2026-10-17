use subtitle_core::{AssClip, AssClipMode, AssClipShape, AssTransformTiming, SubtitleTime};

use super::*;
use crate::ass::options::AssStorageSize;
use crate::ass::test_script::{
    cue_with, default_style, script_with, tags, text, transform, transform_range,
};
use std::sync::Arc;

use subtitle_core::SubtitleColor;

use crate::overlay::{PixelPoint, SubtitleBitmap, SubtitleBitmapKind};
use crate::renderer::AssEventTiming;

fn rect(mode: AssClipMode, corners: [i32; 4]) -> AssOverride {
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

fn clock(elapsed_ms: i64, duration_ms: i64) -> AssEventClock {
    AssEventClock::new(
        SubtitleTime::from_millis(elapsed_ms),
        AssEventTiming::new(SubtitleTime::ZERO, SubtitleTime::from_millis(duration_ms)),
    )
}

fn canvas() -> CanvasSize {
    CanvasSize::new(1280, 720).expect("canvas")
}

fn animation_of(overrides: Vec<AssOverride>) -> Option<ClipAnimation> {
    let script = script_with(vec![default_style()]);
    let cue = cue_with(&script, vec![tags(overrides), text("x")]);
    let frame = AssFrameScale::new(cue.script.info(), canvas(), AssStorageSize::Unknown);
    ClipAnimation::for_cue(&cue, &frame, canvas(), true)
}

/// Видимые куски реплики-квадрата 100x100 в начале координат после клипа `rect`.
fn visible_width(rect: Option<ScreenRectClip>) -> u32 {
    let image = SubtitleBitmap::new(
        PixelPoint { x: 0, y: 0 },
        100,
        100,
        100,
        Arc::from(vec![255_u8; 100 * 100]),
        SubtitleColor::opaque(255, 255, 255),
        SubtitleBitmapKind::Glyph,
    )
    .expect("image");
    let mut parts = Vec::new();
    match rect {
        Some(clip) => {
            clip.push_visible_parts(&image, crate::clip::ImagePaint::AsStored, &mut parts)
        }
        None => parts.push(image),
    }
    parts.iter().map(SubtitleBitmap::width).sum()
}

#[test]
fn no_transform_with_a_rect_clip_means_no_animation() {
    assert!(animation_of(vec![rect(AssClipMode::Inside, [0, 0, 10, 10])]).is_none());
    assert!(animation_of(vec![transform_range(0, 100, vec![])]).is_none());
}

#[test]
fn clip_grows_linearly_from_the_static_clip_to_the_transform_target() {
    // Статический клип 0..0, \t(0,1000) растит его до 100 px по x1.
    let animation = animation_of(vec![
        rect(AssClipMode::Inside, [0, 0, 0, 100]),
        transform_range(0, 1000, vec![rect(AssClipMode::Inside, [0, 0, 100, 100])]),
    ])
    .expect("animated clip");
    let width_at = |ms| visible_width(animation.rect_at(&clock(ms, 4000)));
    assert_eq!(width_at(0), 0);
    assert_eq!(width_at(250), 25);
    assert_eq!(width_at(500), 50);
    assert_eq!(width_at(1000), 100);
    assert_eq!(width_at(3000), 100);
}

#[test]
fn clip_starts_from_the_whole_play_res_when_there_was_no_static_clip() {
    // Без статического клипа старое значение — весь PlayRes 1280x720 (`clip_x1 = PlayResX`).
    let animation = animation_of(vec![transform(
        AssTransformTiming::Range {
            t1_ms: 0,
            t2_ms: 1000,
        },
        vec![rect(AssClipMode::Inside, [0, 0, 80, 100])],
    )])
    .expect("animated clip");
    // x1 при весе 0.5: 1280 * 0.5 + 80 * 0.5 = 680 -> картинка 100 px целиком видна.
    assert_eq!(visible_width(animation.rect_at(&clock(500, 4000))), 100);
    // К концу клип сжимается до 80 px.
    assert_eq!(visible_width(animation.rect_at(&clock(1000, 4000))), 80);
}

#[test]
fn coordinates_truncate_toward_zero_when_blended() {
    // x1: 0 -> 99 при весе 0.5 = 49.5 -> 49.
    let animation = animation_of(vec![
        rect(AssClipMode::Inside, [0, 0, 0, 100]),
        transform_range(0, 1000, vec![rect(AssClipMode::Inside, [0, 0, 99, 100])]),
    ])
    .expect("animated clip");
    assert_eq!(visible_width(animation.rect_at(&clock(500, 4000))), 49);
}

#[test]
fn clip_tags_outside_transforms_keep_last_wins() {
    let animation = animation_of(vec![
        rect(AssClipMode::Inside, [0, 0, 10, 100]),
        transform_range(0, 1000, vec![rect(AssClipMode::Inside, [0, 0, 100, 100])]),
        rect(AssClipMode::Inside, [0, 0, 60, 100]),
    ])
    .expect("animated clip");
    // Последний статичный клип (x1 = 60) перекрывает результат \t, как в libass.
    assert_eq!(visible_width(animation.rect_at(&clock(500, 4000))), 60);
}
