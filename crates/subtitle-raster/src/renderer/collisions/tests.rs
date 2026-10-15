use super::*;

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> PixelRect {
    PixelRect {
        left,
        top,
        right,
        bottom,
    }
}

fn item(extent: Option<PixelRect>, direction: CollisionDirection) -> CollisionItem {
    CollisionItem { extent, direction }
}

#[test]
fn bottom_cues_stack_upwards_without_overlap() {
    let items = [
        item(Some(rect(100, 600, 500, 650)), CollisionDirection::Up),
        item(Some(rect(150, 600, 450, 650)), CollisionDirection::Up),
        item(Some(rect(120, 610, 480, 660)), CollisionDirection::Up),
    ];
    let shifts = resolve_vertical_shifts(&items, 720);
    // Первая стоит; вторая встаёт над ней; третья — над второй.
    assert_eq!(shifts, vec![0, -50, -110]);
}

#[test]
fn top_cues_move_down() {
    let items = [
        item(Some(rect(0, 20, 400, 70)), CollisionDirection::Down),
        item(Some(rect(0, 30, 400, 80)), CollisionDirection::Down),
    ];
    assert_eq!(resolve_vertical_shifts(&items, 720), vec![0, 40]);
}

#[test]
fn cues_that_do_not_share_area_stay_in_place() {
    let items = [
        item(Some(rect(0, 600, 100, 650)), CollisionDirection::Up),
        // Другая колонка.
        item(Some(rect(100, 600, 200, 650)), CollisionDirection::Up),
        // Касается сверху (общей площади нет).
        item(Some(rect(0, 650, 100, 700)), CollisionDirection::Up),
        item(None, CollisionDirection::Up),
    ];
    assert_eq!(resolve_vertical_shifts(&items, 720), vec![0, 0, 0, 0]);
}

#[test]
fn shift_never_pushes_a_cue_outside_the_canvas() {
    let items = [
        item(Some(rect(0, 0, 100, 100)), CollisionDirection::Up),
        item(Some(rect(0, 10, 100, 110)), CollisionDirection::Up),
    ];
    // Подъём нужен на 110, но выше верхнего края холста (на 10) подняться нельзя.
    assert_eq!(resolve_vertical_shifts(&items, 720), vec![0, -10]);
}
