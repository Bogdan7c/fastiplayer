//! Тесты hit-test места броска: пересчёт физических пикселей в точки egui и безопасный default.

use super::*;

/// Панель плейлиста 400×600 точек у левого края окна.
fn playlist_panel_rect() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 600.0))
}

fn hit_test_with_panel() -> PlaylistPanelHitTest {
    let mut hit_test = PlaylistPanelHitTest::default();
    hit_test.record_panel_rect(Some(playlist_panel_rect()));
    hit_test
}

fn physical(x: f64, y: f64) -> Option<PhysicalDropPosition> {
    Some(PhysicalDropPosition { x, y })
}

#[test]
fn drop_inside_playlist_panel_targets_playlist_after_scale_conversion() {
    let hit_test = hit_test_with_panel();

    // 450 физических px при масштабе 1.25 — это 360 точек: внутри панели шириной 400.
    assert_eq!(
        hit_test.resolve(physical(450.0, 100.0), 1.25),
        DropTarget::Playlist
    );
}

#[test]
fn drop_outside_panel_in_points_targets_video_even_if_inside_in_pixels() {
    let hit_test = hit_test_with_panel();

    // 300 физических px при масштабе 1.0 внутри панели, но 600 px при 1.25 = 480 точек — снаружи.
    assert_eq!(
        hit_test.resolve(physical(300.0, 100.0), 1.0),
        DropTarget::Playlist
    );
    assert_eq!(
        hit_test.resolve(physical(600.0, 100.0), 1.25),
        DropTarget::Video
    );
}

#[test]
fn missing_position_or_panel_falls_back_to_video() {
    let hit_test = hit_test_with_panel();
    assert_eq!(hit_test.resolve(None, 1.0), DropTarget::Video);

    let mut hidden_panel = hit_test_with_panel();
    hidden_panel.record_panel_rect(None);
    assert_eq!(hidden_panel.panel_rect(), None);
    assert_eq!(
        hidden_panel.resolve(physical(10.0, 10.0), 1.0),
        DropTarget::Video
    );
}

#[test]
fn invalid_scale_falls_back_to_video_instead_of_dividing() {
    let hit_test = hit_test_with_panel();

    for invalid_scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert_eq!(
            hit_test.resolve(physical(10.0, 10.0), invalid_scale),
            DropTarget::Video,
            "масштаб {invalid_scale} должен давать безопасный default"
        );
    }
}

#[test]
fn recorded_rect_is_returned_unchanged() {
    let hit_test = hit_test_with_panel();
    assert_eq!(hit_test.panel_rect(), Some(playlist_panel_rect()));
}
