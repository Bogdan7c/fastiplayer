use subtitle_core::{SubtitleEventId, SubtitleLayer};

use super::*;
use crate::ass::AssRenderOptions;
use crate::renderer::request::CueSourceId;

fn canvas() -> CanvasSize {
    CanvasSize::new(1280, 720).expect("canvas")
}

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> PixelRect {
    PixelRect {
        left,
        top,
        right,
        bottom,
    }
}

/// Ключ с настоящим идентификатором события.
fn key(number: usize) -> CueKey {
    let mut timeline = subtitle_core::SubtitleTimeline::new();
    let mut last: Option<SubtitleEventId> = None;
    for index in 0..=number {
        let event = subtitle_core::SubtitleEvent::new(
            subtitle_core::SubtitleTime::from_millis(index as i64),
            subtitle_core::SubtitleTime::from_millis(index as i64 + 1),
            SubtitleLayer::DEFAULT,
            subtitle_core::SubtitleContent::Text(subtitle_core::SubtitleTextCue {
                text: subtitle_core::StyledText::new(vec![]),
                placement: subtitle_core::CuePlacement::default(),
            }),
        )
        .expect("interval");
        if let subtitle_core::TimelineInsertOutcome::Inserted(id) = timeline.insert(event) {
            last = Some(id);
        }
    }
    CueKey::new(CueSourceId::new(1), last.expect("inserted event"))
}

fn item(
    number: usize,
    layer: i32,
    rect: Option<PixelRect>,
    direction: CollisionDirection,
) -> AssCollisionItem {
    AssCollisionItem {
        key: key(number),
        layer: SubtitleLayer::new(layer),
        rect,
        direction,
    }
}

fn resolve(memory: &mut AssCollisionMemory, items: &[AssCollisionItem]) -> Vec<i32> {
    memory.resolve(items, canvas(), &AssRenderOptions::default())
}

#[test]
fn same_layer_events_stack_and_other_layers_are_independent() {
    let mut memory = AssCollisionMemory::default();
    let up = CollisionDirection::Up;
    let items = [
        item(0, 0, Some(rect(100, 600, 500, 650)), up),
        item(1, 0, Some(rect(150, 600, 450, 650)), up),
        // Слой 1: та же рамка, но другой слой не мешает.
        item(2, 1, Some(rect(100, 600, 500, 650)), up),
    ];
    assert_eq!(resolve(&mut memory, &items), vec![0, -50, 0]);
}

#[test]
fn events_without_a_rect_are_neither_moved_nor_remembered() {
    let mut memory = AssCollisionMemory::default();
    let items = [
        item(0, 0, None, CollisionDirection::Up),
        item(1, 0, Some(rect(0, 600, 100, 650)), CollisionDirection::Up),
    ];
    assert_eq!(resolve(&mut memory, &items), vec![0, 0]);
    assert_eq!(memory.len(), 1);
}

#[test]
fn a_placed_event_keeps_its_position_after_the_neighbour_disappears() {
    let mut memory = AssCollisionMemory::default();
    let up = CollisionDirection::Up;
    let lower = item(0, 0, Some(rect(100, 600, 500, 650)), up);
    let upper = item(1, 0, Some(rect(100, 600, 500, 650)), up);
    assert_eq!(resolve(&mut memory, &[lower, upper]), vec![0, -50]);
    // Нижняя исчезла: верхняя остаётся на сдвинутом месте (запомненное положение).
    assert_eq!(resolve(&mut memory, &[upper]), vec![-50]);
}

#[test]
fn remembered_position_is_dropped_when_the_height_changes() {
    let mut memory = AssCollisionMemory::default();
    let up = CollisionDirection::Up;
    let lower = item(0, 0, Some(rect(100, 600, 500, 650)), up);
    let upper = item(1, 0, Some(rect(100, 600, 500, 650)), up);
    resolve(&mut memory, &[lower, upper]);
    let taller = item(1, 0, Some(rect(100, 580, 500, 650)), up);
    // Высота другая (70 вместо 50): положение забыто, событие встаёт заново (без соседей — на месте).
    assert_eq!(resolve(&mut memory, &[taller]), vec![0]);
}

#[test]
fn a_new_event_stands_around_an_already_remembered_one() {
    let mut memory = AssCollisionMemory::default();
    let up = CollisionDirection::Up;
    let a = item(0, 0, Some(rect(100, 600, 500, 650)), up);
    let b = item(1, 0, Some(rect(100, 600, 500, 650)), up);
    // Сначала b стоит один и запоминает рамку 600..650.
    assert_eq!(resolve(&mut memory, &[b]), vec![0]);
    // Потом появляется a, которая по порядку идёт раньше: запомненное место b неприкосновенно
    // (оно «зафиксировано»), a обходит его и встаёт выше.
    assert_eq!(resolve(&mut memory, &[a, b]), vec![-50, 0]);
}

#[test]
fn memory_is_reset_when_canvas_or_options_change_and_is_bounded() {
    let mut memory = AssCollisionMemory::default();
    let up = CollisionDirection::Up;
    let lower = item(0, 0, Some(rect(100, 600, 500, 650)), up);
    let upper = item(1, 0, Some(rect(100, 600, 500, 650)), up);
    assert_eq!(resolve(&mut memory, &[lower, upper]), vec![0, -50]);
    // Другие настройки: прежние положения недействительны.
    let scaled = AssRenderOptions {
        text_scale: crate::ass::AssTextScale::new(1.5).expect("scale"),
        ..AssRenderOptions::default()
    };
    assert_eq!(
        memory.resolve(&[upper], canvas(), &scaled),
        vec![0],
        "upper event stands at its own place after the reset"
    );
    // Другой холст — то же.
    let small = CanvasSize::new(640, 360).expect("canvas");
    assert_eq!(memory.resolve(&[upper], small, &scaled), vec![0]);
    // Память ограничена активными событиями.
    memory.resolve(&[], small, &scaled);
    assert_eq!(memory.len(), 0);
}

#[test]
fn clear_forgets_everything() {
    let mut memory = AssCollisionMemory::default();
    let up = CollisionDirection::Up;
    let lower = item(0, 0, Some(rect(100, 600, 500, 650)), up);
    let upper = item(1, 0, Some(rect(100, 600, 500, 650)), up);
    resolve(&mut memory, &[lower, upper]);
    memory.clear();
    assert_eq!(memory.len(), 0);
    assert_eq!(resolve(&mut memory, &[upper]), vec![0]);
}
