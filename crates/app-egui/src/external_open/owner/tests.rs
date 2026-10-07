use std::path::PathBuf;

use super::*;
use crate::external_open::event::PhysicalDropPosition;
use crate::external_open::request::ExternalOpenItem;

fn panel() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(800.0, 0.0), egui::pos2(1200.0, 700.0))
}

fn video() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 700.0))
}

fn at(x: f64, y: f64) -> Option<PhysicalDropPosition> {
    Some(PhysicalDropPosition { x, y })
}

#[test]
fn overlay_follows_hover_target_and_disappears_on_left_and_drop() {
    let mut owner = ExternalOpenOwner::default();
    owner.record_playlist_panel_rect(Some(panel()));
    assert!(owner.drop_overlay(video(), 1.0).is_none());

    owner.apply_gesture_event(
        DropGestureEvent::Entered {
            position: at(100.0, 100.0),
        },
        1.0,
    );
    assert_eq!(
        owner.drop_overlay(video(), 1.0),
        Some(DropOverlay {
            target: DropTarget::Video,
            rect: video()
        })
    );

    owner.apply_gesture_event(
        DropGestureEvent::Moved {
            position: PhysicalDropPosition { x: 900.0, y: 50.0 },
        },
        1.0,
    );
    assert_eq!(
        owner.drop_overlay(video(), 1.0),
        Some(DropOverlay {
            target: DropTarget::Playlist,
            rect: panel()
        })
    );

    owner.apply_gesture_event(DropGestureEvent::Left, 1.0);
    assert!(owner.drop_overlay(video(), 1.0).is_none());

    owner.apply_gesture_event(DropGestureEvent::Entered { position: None }, 1.0);
    assert_eq!(
        owner
            .drop_overlay(video(), 1.0)
            .map(|overlay| overlay.target),
        Some(DropTarget::Video)
    );
    owner.apply_gesture_event(
        DropGestureEvent::Dropped {
            position: None,
            items: Vec::new(),
        },
        1.0,
    );
    assert!(owner.drop_overlay(video(), 1.0).is_none());
}

#[test]
fn drop_on_published_panel_resolves_to_playlist_with_scale_factor() {
    let mut owner = ExternalOpenOwner::default();
    owner.record_playlist_panel_rect(Some(panel()));
    let items = vec![ExternalOpenItem::LocalPath(PathBuf::from("/a.mkv"))];

    // 1700 физ. px при масштабе 1.7 = 1000 точек — внутри панели.
    let request = owner
        .apply_gesture_event(
            DropGestureEvent::Dropped {
                position: at(1700.0, 100.0),
                items: items.clone(),
            },
            1.7,
        )
        .expect("request");
    assert_eq!(request.target, DropTarget::Playlist);
    assert_eq!(request.items, items);

    // Панель спрятана — тот же бросок идёт на видео.
    owner.record_playlist_panel_rect(None);
    let request = owner
        .apply_gesture_event(
            DropGestureEvent::Dropped {
                position: at(1700.0, 100.0),
                items,
            },
            1.7,
        )
        .expect("request");
    assert_eq!(request.target, DropTarget::Video);
}
