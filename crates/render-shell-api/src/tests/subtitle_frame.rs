//! Формула угла холста субтитров (перенесена из wgpu-шелла без изменения ожиданий: общий
//! источник истины для wgpu и GLES2).

use render_core::PictureRect;

use super::{SubtitleCanvasOrigins, SubtitleLayerOffset, SubtitleLayerOffsets};

fn rect(x: f32, y: f32) -> PictureRect {
    PictureRect {
        x,
        y,
        width: 100.0,
        height: 50.0,
    }
}

/// Угол основной группы при одном сдвиге на обе группы.
fn main_origin(picture_rect: PictureRect, offset: SubtitleLayerOffset) -> Option<[f32; 2]> {
    SubtitleCanvasOrigins::for_picture(picture_rect, SubtitleLayerOffsets::uniform(offset))
        .map(|origins| origins.main)
}

#[test]
fn canvas_origin_rounds_to_whole_surface_pixels() {
    let none = SubtitleLayerOffset::NONE;
    assert_eq!(main_origin(rect(10.4, 18.5), none), Some([10.0, 19.0]));
    assert_eq!(main_origin(rect(0.0, 17.6), none), Some([0.0, 18.0]));
}

#[test]
fn canvas_origin_rejects_non_finite_coordinates() {
    let none = SubtitleLayerOffset::NONE;
    assert_eq!(main_origin(rect(f32::NAN, 0.0), none), None);
    assert_eq!(main_origin(rect(0.0, f32::INFINITY), none), None);
    let nan_offset = SubtitleLayerOffset {
        x: f32::NAN,
        y: 0.0,
    };
    assert_eq!(main_origin(rect(0.0, 0.0), nan_offset), None);
    let inf_offset = SubtitleLayerOffset {
        x: 0.0,
        y: f32::NEG_INFINITY,
    };
    assert_eq!(main_origin(rect(0.0, 0.0), inf_offset), None);
}

#[test]
fn group_origins_follow_their_own_offsets() {
    let offsets = SubtitleLayerOffsets {
        main: SubtitleLayerOffset { x: 0.0, y: -30.0 },
        fixed: SubtitleLayerOffset::NONE,
    };
    assert_eq!(
        SubtitleCanvasOrigins::for_picture(rect(10.0, 20.0), offsets),
        Some(SubtitleCanvasOrigins {
            main: [10.0, -10.0],
            fixed: [10.0, 20.0],
        })
    );
    let broken = SubtitleLayerOffsets {
        main: SubtitleLayerOffset::NONE,
        fixed: SubtitleLayerOffset {
            x: f32::NAN,
            y: 0.0,
        },
    };
    assert_eq!(
        SubtitleCanvasOrigins::for_picture(rect(0.0, 0.0), broken),
        None
    );
    assert_eq!(
        SubtitleLayerOffsets::uniform(SubtitleLayerOffset { x: 1.0, y: 2.0 }).fixed,
        SubtitleLayerOffset { x: 1.0, y: 2.0 }
    );
}

#[test]
fn canvas_origin_adds_the_layer_offset_before_rounding() {
    let offset = SubtitleLayerOffset { x: 0.4, y: -30.4 };
    // 10.4 + 0.4 = 10.8 -> 11; 18.5 - 30.4 = -11.9 -> -12 (слой может уйти выше картинки).
    assert_eq!(main_origin(rect(10.4, 18.5), offset), Some([11.0, -12.0]));
}
