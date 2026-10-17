//! Подъём основной группы: числа проверяются по итоговым сдвигам и прямоугольнику слоя.

use super::*;

/// Картинка 1000x420 со смещением 90 сверху (полосы по бокам вертикали).
fn picture() -> PictureRect {
    PictureRect {
        x: 0.0,
        y: 90.0,
        width: 1000.0,
        height: 420.0,
    }
}

/// Основной диалог у низа картинки: холст y 330..395.
fn dialogue() -> CanvasRect {
    CanvasRect {
        x: 300,
        y: 330,
        width: 400,
        height: 65,
    }
}

/// Надпись у верха картинки.
fn sign() -> CanvasRect {
    CanvasRect {
        x: 40,
        y: 20,
        width: 120,
        height: 30,
    }
}

fn request(obstruction: BottomControlsObstruction) -> MainGroupLiftRequest {
    MainGroupLiftRequest {
        picture_rect: picture(),
        main_group_bounds: Some(dialogue()),
        fixed_group_bounds: Some(sign()),
        obstruction,
    }
}

fn controls_at(top_px: i32) -> BottomControlsObstruction {
    BottomControlsObstruction::TopEdge {
        top_px,
        clearance_px: 8,
    }
}

#[test]
fn without_controls_nothing_moves_and_rect_covers_both_groups() {
    let lift = lift_main_group_above_controls(&request(BottomControlsObstruction::Absent))
        .expect("valid geometry");
    assert_eq!(lift.offsets, SubtitleLayerOffsets::NONE);
    // Надпись: x 40, y 90+20; диалог: низ 90+395 = 485.
    assert_eq!(
        lift.rect,
        PlacedLayerRect {
            x: 40,
            y: 110,
            width: 660,
            height: 375
        }
    );
}

#[test]
fn controls_over_the_dialogue_lift_only_the_main_group_by_the_overlap() {
    // Диалог: низ на экране 420 + 65 = 485; панель с зазором оставляет до 460 -> подъём 25.
    let lift = lift_main_group_above_controls(&request(controls_at(468))).expect("geometry");
    assert_eq!(lift.offsets.main, SubtitleLayerOffset { x: 0.0, y: -25.0 });
    assert_eq!(lift.offsets.fixed, SubtitleLayerOffset::NONE);
}

#[test]
fn controls_below_the_dialogue_do_not_move_anything() {
    let lift = lift_main_group_above_controls(&request(controls_at(600))).expect("geometry");
    assert_eq!(lift.offsets, SubtitleLayerOffsets::NONE);
}

#[test]
fn partially_visible_panel_gives_partial_lift_so_the_motion_is_smooth() {
    let lifts: Vec<f32> = [500, 490, 480, 470]
        .into_iter()
        .map(|top| {
            lift_main_group_above_controls(&request(controls_at(top)))
                .expect("geometry")
                .offsets
                .main
                .y
        })
        .collect();
    // Панель выезжает вверх шаг за шагом - диалог поднимается монотонно и без скачков.
    assert_eq!(
        lifts,
        [-0.0, -3.0, -13.0, -23.0].map(|value: f32| value.min(0.0))
    );
    assert!(lifts.windows(2).all(|pair| pair[1] <= pair[0]));
}

#[test]
fn lift_never_pushes_the_group_above_the_picture_top() {
    // Панель почти во всё окно: подъём ограничен верхом картинки (холст y = 330 -> максимум 330).
    let lift = lift_main_group_above_controls(&request(controls_at(0))).expect("geometry");
    assert_eq!(lift.offsets.main.y, -330.0);
    assert_eq!(lift.rect.y, 90);
}

#[test]
fn group_in_the_main_group_only_overlay_still_lifts_and_fixed_only_overlay_never_does() {
    let main_only = MainGroupLiftRequest {
        fixed_group_bounds: None,
        ..request(controls_at(468))
    };
    let lift = lift_main_group_above_controls(&main_only).expect("geometry");
    assert_eq!(lift.offsets.main.y, -25.0);
    assert_eq!(lift.rect.y, 420 - 25);

    let fixed_only = MainGroupLiftRequest {
        main_group_bounds: None,
        ..request(controls_at(0))
    };
    let lift = lift_main_group_above_controls(&fixed_only).expect("geometry");
    assert_eq!(lift.offsets, SubtitleLayerOffsets::NONE);
    assert_eq!(lift.rect.y, 110);
}

#[test]
fn empty_overlay_and_broken_geometry_give_no_placement() {
    let empty = MainGroupLiftRequest {
        main_group_bounds: None,
        fixed_group_bounds: None,
        ..request(BottomControlsObstruction::Absent)
    };
    assert_eq!(lift_main_group_above_controls(&empty), None);
    let broken = MainGroupLiftRequest {
        picture_rect: PictureRect {
            x: f32::NAN,
            ..picture()
        },
        ..request(BottomControlsObstruction::Absent)
    };
    assert_eq!(lift_main_group_above_controls(&broken), None);
}
