//! Размещение PGS-слоя: числа проверяются по итоговым сдвигам групп и прямоугольнику слоя.

use render_shell_api::SubtitleLayerOffset;

use super::*;
use crate::state::subtitles::layer_placement::PlacedLayerRect;

/// Картинка 1000x420, верх на 90 (по 90 пикселей чёрных полос сверху и снизу).
fn picture() -> PictureRect {
    PictureRect {
        x: 0.0,
        y: 90.0,
        width: 1000.0,
        height: 420.0,
    }
}

/// Нижняя картинка (основная группа): холст y 330..395.
fn bottom_picture() -> CanvasRect {
    CanvasRect {
        x: 300,
        y: 330,
        width: 400,
        height: 65,
    }
}

/// Верхняя надпись (фиксированная группа): холст y 20..50.
fn top_sign() -> CanvasRect {
    CanvasRect {
        x: 40,
        y: 20,
        width: 120,
        height: 30,
    }
}

fn request(
    vertical_offset_fraction: f32,
    obstruction: BottomControlsObstruction,
) -> PictureGroupPlacementRequest {
    PictureGroupPlacementRequest {
        picture_rect: picture(),
        main_group_bounds: Some(bottom_picture()),
        fixed_group_bounds: Some(top_sign()),
        vertical_offset_fraction,
        obstruction,
    }
}

fn controls_at(top_px: i32) -> BottomControlsObstruction {
    BottomControlsObstruction::TopEdge {
        top_px,
        clearance_px: 8,
    }
}

fn place(vertical_offset_fraction: f32, obstruction: BottomControlsObstruction) -> MainGroupLift {
    place_picture_groups(&request(vertical_offset_fraction, obstruction)).expect("valid geometry")
}

#[test]
fn author_placement_stays_without_offset_and_controls() {
    let placed = place(0.0, BottomControlsObstruction::Absent);
    assert_eq!(placed.offsets, SubtitleLayerOffsets::NONE);
    // Надпись: x 40, y 110; нижняя картинка: низ 90 + 395 = 485.
    assert_eq!(
        placed.rect,
        PlacedLayerRect {
            x: 40,
            y: 110,
            width: 660,
            height: 375,
        }
    );
}

#[test]
fn controls_lift_only_the_bottom_picture() {
    // Низ нижней картинки 485, панель с зазором 8 начинается с 472: подъём на 13.
    let placed = place(0.0, controls_at(480));
    assert_eq!(
        placed.offsets.main,
        SubtitleLayerOffset { x: 0.0, y: -13.0 }
    );
    assert_eq!(placed.offsets.fixed, SubtitleLayerOffset::NONE);
    assert_eq!(
        placed.rect.height, 362,
        "union shrinks by the lift: 110..485-13"
    );
}

#[test]
fn user_offset_moves_only_the_bottom_picture_vertically() {
    // 0.1 высоты картинки = 42 px вверх; горизонтали у PGS нет вовсе.
    let placed = place(0.1, BottomControlsObstruction::Absent);
    assert_eq!(
        placed.offsets.main,
        SubtitleLayerOffset { x: 0.0, y: -42.0 }
    );
    assert_eq!(placed.offsets.fixed, SubtitleLayerOffset::NONE);
}

#[test]
fn huge_upward_offset_stops_at_the_picture_top() {
    let placed = place(50.0, BottomControlsObstruction::Absent);
    // Верх нижней картинки 420 -> верх видео 90: сдвиг -330.
    assert_eq!(placed.offsets.main.y, -330.0);
}

#[test]
fn huge_downward_offset_stops_at_the_picture_bottom_never_on_the_bars() {
    let placed = place(-50.0, BottomControlsObstruction::Absent);
    // Низ картинки видео 510: верх группы 510 - 65 = 445, был 420.
    assert_eq!(placed.offsets.main.y, 25.0);
}

#[test]
fn offset_down_below_the_controls_is_lifted_back_above_them() {
    let placed = place(-50.0, controls_at(400));
    // Группа прижата к низу картинки (445..510), панель с зазором с 392: верх 445 - (510-392).
    assert_eq!(placed.offsets.main.y, 445.0 - 118.0 - 420.0);
}

#[test]
fn lift_never_pushes_the_group_above_the_picture() {
    // Панель выше всей картинки: подъём упирается в верх видео.
    let placed = place(0.0, controls_at(100));
    assert_eq!(placed.offsets.main.y, -330.0);
}

#[test]
fn non_finite_offset_fraction_is_ignored() {
    let placed = place(f32::NAN, BottomControlsObstruction::Absent);
    assert_eq!(placed.offsets, SubtitleLayerOffsets::NONE);
}

#[test]
fn layer_with_only_a_sign_never_moves() {
    let placed = place_picture_groups(&PictureGroupPlacementRequest {
        main_group_bounds: None,
        ..request(0.3, controls_at(120))
    })
    .expect("sign is enough for a layer");
    assert_eq!(placed.offsets, SubtitleLayerOffsets::NONE);
    assert_eq!(
        placed.rect,
        PlacedLayerRect {
            x: 40,
            y: 110,
            width: 120,
            height: 30,
        }
    );
}

#[test]
fn empty_layer_and_non_finite_picture_give_no_placement() {
    let empty = PictureGroupPlacementRequest {
        main_group_bounds: None,
        fixed_group_bounds: None,
        ..request(0.0, BottomControlsObstruction::Absent)
    };
    assert_eq!(place_picture_groups(&empty), None);
    let broken_picture = PictureGroupPlacementRequest {
        picture_rect: PictureRect {
            width: f32::NAN,
            ..picture()
        },
        ..request(0.0, BottomControlsObstruction::Absent)
    };
    assert_eq!(place_picture_groups(&broken_picture), None);
}
