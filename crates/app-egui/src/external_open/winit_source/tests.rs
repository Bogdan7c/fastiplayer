use std::path::PathBuf;

use winit::event::WindowEvent;

use super::*;
use crate::external_open::request::ExternalOpenItem;

#[test]
fn external_channel_events_keep_physical_positions() {
    let entered = gesture_event_from_external_drag(ExternalDragEvent::Entered {
        position: PhysicalPosition::new(12.5, 7.0),
    });
    assert_eq!(
        entered,
        Some(DropGestureEvent::Entered {
            position: Some(PhysicalDropPosition { x: 12.5, y: 7.0 })
        })
    );
    let moved = gesture_event_from_external_drag(ExternalDragEvent::Moved {
        position: PhysicalPosition::new(30.0, 40.0),
    });
    assert_eq!(
        moved,
        Some(DropGestureEvent::Moved {
            position: PhysicalDropPosition { x: 30.0, y: 40.0 }
        })
    );
    assert_eq!(
        gesture_event_from_external_drag(ExternalDragEvent::Left),
        Some(DropGestureEvent::Left)
    );
}

#[test]
fn legacy_series_of_dropped_files_becomes_one_positionless_drop_in_order() {
    let mut collector = LegacyFileDropCollector::default();

    assert_eq!(
        collector.on_window_event(&WindowEvent::HoveredFile(PathBuf::from("/a.mkv"))),
        Some(DropGestureEvent::Entered { position: None })
    );
    // Второй HoveredFile того же жеста не создаёт второй Entered.
    assert_eq!(
        collector.on_window_event(&WindowEvent::HoveredFile(PathBuf::from("/b.mkv"))),
        None
    );

    assert_eq!(
        collector.on_window_event(&WindowEvent::DroppedFile(PathBuf::from("/b.mkv"))),
        None
    );
    assert_eq!(
        collector.on_window_event(&WindowEvent::DroppedFile(PathBuf::from("/a.mkv"))),
        None
    );

    let drop = collector.take_completed_drop().expect("collected drop");
    assert_eq!(
        drop,
        DropGestureEvent::Dropped {
            position: None,
            items: vec![
                ExternalOpenItem::LocalPath(PathBuf::from("/b.mkv")),
                ExternalOpenItem::LocalPath(PathBuf::from("/a.mkv")),
            ],
        }
    );
    assert!(
        collector.take_completed_drop().is_none(),
        "бросок выдаётся один раз"
    );
}

#[test]
fn legacy_cancel_ends_gesture_and_new_hover_starts_a_new_one() {
    let mut collector = LegacyFileDropCollector::default();
    assert_eq!(
        collector.on_window_event(&WindowEvent::HoveredFileCancelled),
        None,
        "отмена без наведения ничего не значит"
    );
    collector.on_window_event(&WindowEvent::HoveredFile(PathBuf::from("/a.mkv")));
    assert_eq!(
        collector.on_window_event(&WindowEvent::HoveredFileCancelled),
        Some(DropGestureEvent::Left)
    );
    assert_eq!(
        collector.on_window_event(&WindowEvent::HoveredFile(PathBuf::from("/a.mkv"))),
        Some(DropGestureEvent::Entered { position: None })
    );
    assert!(collector.take_completed_drop().is_none());
}
