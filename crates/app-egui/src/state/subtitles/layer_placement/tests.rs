//! Тесты размещения слоя: каждое правило владельца проверяется по итоговым пикселям.

use super::*;

/// Окно 1000x600; видео 1000x420 по центру по вертикали (полосы по 90 px сверху и снизу).
fn letterboxed() -> LayerPlacementRequest {
    LayerPlacementRequest {
        picture_rect: PictureRect {
            x: 0.0,
            y: 90.0,
            width: 1000.0,
            height: 420.0,
        },
        viewport: RenderViewport::new(0, 0, 1000, 600),
        // Стандартное место: низ на 6 % выше низа картинки (330 + 65 = 395 -> ~25 px от низа).
        content_bounds: CanvasRect {
            x: 300,
            y: 330,
            width: 400,
            height: 65,
        },
        horizontal_offset_fraction: 0.0,
        vertical_offset_fraction: 0.0,
        obstruction: BottomControlsObstruction::Absent,
    }
}

fn placed(request: &LayerPlacementRequest) -> LayerPlacement {
    place_layer(request).expect("geometry is valid")
}

fn rect(x: i32, y: i32, width: u32, height: u32) -> PlacedLayerRect {
    PlacedLayerRect {
        x,
        y,
        width,
        height,
    }
}

fn obstruction_at(top_px: i32) -> BottomControlsObstruction {
    BottomControlsObstruction::TopEdge {
        top_px,
        clearance_px: 8,
    }
}

#[test]
fn default_style_without_controls_keeps_the_layer_exactly_where_it_is() {
    let result = placed(&letterboxed());
    assert_eq!(result.offset, SubtitleLayerOffset::NONE);
    assert_eq!(result.rect, rect(300, 420, 400, 65));
}

#[test]
fn negative_vertical_position_moves_the_layer_onto_the_bar_below_the_picture() {
    let picture_bottom = 90 + 420;
    // -25 % при пресете 6 %: сдвиг вниз на 31 % высоты картинки (130 px), низ упёрся бы в 615.
    let request = LayerPlacementRequest {
        vertical_offset_fraction: -0.31,
        ..letterboxed()
    };
    let result = placed(&request);
    // Слой прижат к низу области видео (600), а не к низу картинки: полоса - часть области.
    assert_eq!(result.rect, rect(300, 535, 400, 65));
    assert!(result.rect.y >= picture_bottom, "layer sits in the bar");
    assert_eq!(result.offset.y, 535.0 - 420.0);
}

#[test]
fn vertical_position_inside_the_picture_moves_by_the_fraction_of_picture_height() {
    let request = LayerPlacementRequest {
        vertical_offset_fraction: 0.10,
        ..letterboxed()
    };
    let result = placed(&request);
    // 10 % от 420 = 42 px вверх.
    assert_eq!(result.offset, SubtitleLayerOffset { x: 0.0, y: -42.0 });
    assert_eq!(result.rect.y, 420 - 42);
}

#[test]
fn layer_never_leaves_the_viewport_through_the_top() {
    let request = LayerPlacementRequest {
        vertical_offset_fraction: 5.0,
        ..letterboxed()
    };
    assert_eq!(placed(&request).rect.y, 0);
}

#[test]
fn horizontal_position_is_clamped_to_the_picture_edges() {
    let to_right = LayerPlacementRequest {
        horizontal_offset_fraction: 0.5,
        ..letterboxed()
    };
    let right = placed(&to_right).rect;
    assert_eq!(
        right.x + right.width as i32,
        1000,
        "right edge == picture right"
    );

    let to_left = LayerPlacementRequest {
        horizontal_offset_fraction: -0.5,
        ..letterboxed()
    };
    assert_eq!(placed(&to_left).rect.x, 0);

    let inside = LayerPlacementRequest {
        horizontal_offset_fraction: 0.1,
        ..letterboxed()
    };
    assert_eq!(placed(&inside).offset.x, 100.0);
}

#[test]
fn pillarbox_clamps_to_the_picture_not_to_the_viewport() {
    // Картинка 600 px по центру окна 1000 px: колонки по 200 px.
    let request = LayerPlacementRequest {
        picture_rect: PictureRect {
            x: 200.0,
            y: 0.0,
            width: 600.0,
            height: 600.0,
        },
        content_bounds: CanvasRect {
            x: 100,
            y: 500,
            width: 400,
            height: 60,
        },
        horizontal_offset_fraction: 0.5,
        ..letterboxed()
    };
    let rect = placed(&request).rect;
    assert_eq!(
        rect.x + rect.width as i32,
        800,
        "picture right edge, not 1000"
    );
}

#[test]
fn layer_wider_than_the_picture_is_centred() {
    let request = LayerPlacementRequest {
        picture_rect: PictureRect {
            x: 100.0,
            y: 90.0,
            width: 800.0,
            height: 420.0,
        },
        content_bounds: CanvasRect {
            x: 0,
            y: 330,
            width: 900,
            height: 65,
        },
        horizontal_offset_fraction: 0.4,
        ..letterboxed()
    };
    // Центры совпадают: слой выходит за картинку на 50 px с каждой стороны.
    assert_eq!(placed(&request).rect.x, 50);
}

#[test]
fn controls_do_not_move_a_layer_that_is_placed_high_enough() {
    // Низ слоя 485, граница подъёма 540 - 8 = 532.
    let request = LayerPlacementRequest {
        obstruction: obstruction_at(540),
        ..letterboxed()
    };
    assert_eq!(placed(&request).offset, SubtitleLayerOffset::NONE);

    let placed_high = LayerPlacementRequest {
        vertical_offset_fraction: 0.30,
        obstruction: obstruction_at(540),
        ..letterboxed()
    };
    assert_eq!(
        placed(&placed_high).offset.y,
        -126.0,
        "only the user's own shift"
    );
}

#[test]
fn controls_lift_an_obstructing_layer_exactly_to_the_clearance() {
    // Пользователь поставил слой низко: -10 % при пресете -> сдвиг вниз на 16 % (67 px).
    let request = LayerPlacementRequest {
        vertical_offset_fraction: -0.16,
        obstruction: obstruction_at(540),
        ..letterboxed()
    };
    let result = placed(&request);
    let bottom = result.rect.y + result.rect.height as i32;
    assert_eq!(bottom, 540 - 8, "bottom stops at panel top minus clearance");
}

#[test]
fn partly_hidden_panel_lifts_partially_and_smoothly() {
    // Панель выезжает: верх ползёт с 600 вверх. Слой стоит по умолчанию (низ 485).
    let lift_for = |panel_top: i32| {
        let request = LayerPlacementRequest {
            obstruction: obstruction_at(panel_top),
            ..letterboxed()
        };
        -placed(&request).offset.y
    };
    assert_eq!(lift_for(600), 0.0);
    assert_eq!(lift_for(493), 0.0, "exactly at the clearance: no lift yet");
    assert_eq!(lift_for(492), 1.0);
    assert_eq!(lift_for(480), 13.0);
    assert_eq!(lift_for(440), 53.0);
    // Монотонно: чем выше панель, тем сильнее подъём, без скачков больше шага панели.
    let mut previous = lift_for(600);
    for panel_top in (440..=600).rev() {
        let current = lift_for(panel_top);
        assert!(current >= previous && current - previous <= 1.0);
        previous = current;
    }
}

#[test]
fn top_of_the_viewport_wins_over_the_lift() {
    let request = LayerPlacementRequest {
        obstruction: obstruction_at(40),
        ..letterboxed()
    };
    let result = placed(&request);
    assert_eq!(result.rect.y, 0, "layer is kept inside the viewport");
}

#[test]
fn same_fractions_give_proportional_results_after_a_resize() {
    let fractions = |request: LayerPlacementRequest| LayerPlacementRequest {
        horizontal_offset_fraction: -0.10,
        vertical_offset_fraction: 0.10,
        ..request
    };
    let big = placed(&fractions(letterboxed()));
    let small = placed(&fractions(LayerPlacementRequest {
        picture_rect: PictureRect {
            x: 0.0,
            y: 45.0,
            width: 500.0,
            height: 210.0,
        },
        viewport: RenderViewport::new(0, 0, 500, 300),
        content_bounds: CanvasRect {
            x: 150,
            y: 165,
            width: 200,
            height: 32,
        },
        ..letterboxed()
    }));
    assert_eq!(
        big.offset,
        SubtitleLayerOffset {
            x: -100.0,
            y: -42.0
        }
    );
    assert_eq!(small.offset, SubtitleLayerOffset { x: -50.0, y: -21.0 });
}

#[test]
fn broken_geometry_or_fractions_never_produce_a_garbage_offset() {
    let nan_picture = LayerPlacementRequest {
        picture_rect: PictureRect {
            x: f32::NAN,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        },
        ..letterboxed()
    };
    assert_eq!(place_layer(&nan_picture), None);

    let empty_picture = LayerPlacementRequest {
        picture_rect: PictureRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 100.0,
        },
        ..letterboxed()
    };
    assert_eq!(place_layer(&empty_picture), None);

    let nan_fractions = LayerPlacementRequest {
        horizontal_offset_fraction: f32::NAN,
        vertical_offset_fraction: f32::INFINITY,
        ..letterboxed()
    };
    assert_eq!(placed(&nan_fractions).offset, SubtitleLayerOffset::NONE);
}
