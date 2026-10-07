use std::path::PathBuf;

use super::*;
use crate::external_open::request::ExternalOpenItem;

fn position(x: f64, y: f64) -> PhysicalDropPosition {
    PhysicalDropPosition { x, y }
}

fn items(paths: &[&str]) -> Vec<ExternalOpenItem> {
    paths
        .iter()
        .map(|path| ExternalOpenItem::LocalPath(PathBuf::from(path)))
        .collect()
}

/// Цель: левая половина — плейлист, правая — видео (по физическому X).
fn split_target(position: Option<PhysicalDropPosition>) -> DropTarget {
    match position {
        Some(position) if position.x < 100.0 => DropTarget::Playlist,
        _ => DropTarget::Video,
    }
}

#[test]
fn gesture_series_yields_one_request_with_all_items_in_source_order() {
    let mut tracker = DropGestureTracker::default();
    assert!(tracker.hovering_position().is_none());

    assert!(
        tracker
            .apply_event(
                DropGestureEvent::Entered {
                    position: Some(position(150.0, 10.0))
                },
                split_target
            )
            .is_none()
    );
    assert_eq!(
        tracker.hovering_position(),
        Some(Some(position(150.0, 10.0)))
    );
    assert!(
        tracker
            .apply_event(
                DropGestureEvent::Moved {
                    position: position(40.0, 20.0)
                },
                split_target
            )
            .is_none()
    );

    let request = tracker
        .apply_event(
            DropGestureEvent::Dropped {
                position: Some(position(30.0, 20.0)),
                items: items(&["/c.mkv", "/a.mkv", "/b.mkv"]),
            },
            split_target,
        )
        .expect("drop must yield a request");

    assert_eq!(request.target, DropTarget::Playlist);
    assert_eq!(request.items, items(&["/c.mkv", "/a.mkv", "/b.mkv"]));
    assert!(
        tracker.hovering_position().is_none(),
        "бросок сбрасывает подсветку"
    );
}

#[test]
fn left_without_drop_yields_no_request_and_clears_hover() {
    let mut tracker = DropGestureTracker::default();
    tracker.apply_event(DropGestureEvent::Entered { position: None }, split_target);
    assert_eq!(tracker.hovering_position(), Some(None));

    assert!(
        tracker
            .apply_event(DropGestureEvent::Left, split_target)
            .is_none()
    );
    assert!(tracker.hovering_position().is_none());
}

#[test]
fn second_drop_yields_second_independent_request() {
    let mut tracker = DropGestureTracker::default();
    let first = tracker
        .apply_event(
            DropGestureEvent::Dropped {
                position: None,
                items: items(&["/one.mkv"]),
            },
            split_target,
        )
        .expect("first request");
    let second = tracker
        .apply_event(
            DropGestureEvent::Dropped {
                position: Some(position(10.0, 0.0)),
                items: items(&["/two.mkv"]),
            },
            split_target,
        )
        .expect("second request");

    assert_eq!(first.items, items(&["/one.mkv"]));
    assert_eq!(first.target, DropTarget::Video, "нет позиции — видео");
    assert_eq!(second.items, items(&["/two.mkv"]));
    assert_eq!(second.target, DropTarget::Playlist);
}

#[test]
fn drop_without_own_position_uses_last_hover_position() {
    let mut tracker = DropGestureTracker::default();
    tracker.apply_event(
        DropGestureEvent::Moved {
            position: position(5.0, 5.0),
        },
        split_target,
    );
    let request = tracker
        .apply_event(
            DropGestureEvent::Dropped {
                position: None,
                items: items(&["/x.mkv"]),
            },
            split_target,
        )
        .expect("request");
    assert_eq!(request.target, DropTarget::Playlist);
}
