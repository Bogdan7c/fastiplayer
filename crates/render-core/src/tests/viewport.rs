use super::*;
#[test]
fn render_viewport_full_surface_covers_target() {
    let viewport = RenderViewport::full_surface(1920, 1080);

    assert_eq!(viewport, RenderViewport::new(0, 0, 1920, 1080));
    assert_eq!(viewport.size(), (1920, 1080));
    assert!(!viewport.is_empty());
}

#[test]
fn render_viewport_clamps_partial_overflow_to_surface() {
    let viewport = RenderViewport::new(100, 50, 1000, 800).clamp_to_surface(640, 360);

    assert_eq!(viewport, RenderViewport::new(100, 50, 540, 310));
}

#[test]
fn render_viewport_invalid_request_defaults_to_full_surface() {
    let full_surface = RenderViewport::full_surface(640, 360);

    assert_eq!(
        RenderViewport::new(10, 10, 0, 200).clamp_to_surface(640, 360),
        full_surface
    );
    assert_eq!(
        RenderViewport::new(640, 10, 100, 100).clamp_to_surface(640, 360),
        full_surface
    );
}

#[test]
fn render_viewport_subtracts_left_sidebar_without_changing_content_viewport() {
    let viewport = RenderViewport::full_surface(1280, 720);
    let sidebar = RenderViewport::new(0, 64, 420, 576);

    let visible_rects = viewport.subtract(sidebar);

    assert_eq!(
        visible_rects,
        vec![
            RenderViewport::new(0, 0, 1280, 64),
            RenderViewport::new(0, 640, 1280, 80),
            RenderViewport::new(420, 64, 860, 576),
        ]
    );
    assert_eq!(viewport.size(), (1280, 720));
}

#[test]
fn render_viewport_subtract_keeps_original_when_exclusion_is_outside() {
    let viewport = RenderViewport::full_surface(1280, 720);
    let outside = RenderViewport::new(1400, 0, 100, 100);

    assert_eq!(viewport.subtract(outside), vec![viewport]);
}

#[test]
fn render_viewport_subtract_returns_no_rects_when_fully_excluded() {
    let viewport = RenderViewport::full_surface(1280, 720);

    assert!(viewport.subtract(viewport).is_empty());
}

#[test]
fn visible_parts_subtract_every_exclusion_and_ignore_outside_ones() {
    let viewport = RenderViewport::new(0, 0, 100, 50);
    let sidebar = RenderViewport::new(0, 0, 30, 50);
    let outside = RenderViewport::new(200, 200, 10, 10);

    assert_eq!(viewport.visible_parts(&[]), vec![viewport]);
    assert_eq!(viewport.visible_parts(&[outside]), vec![viewport]);
    assert_eq!(
        viewport.visible_parts(&[sidebar, outside]),
        vec![RenderViewport::new(30, 0, 70, 50)]
    );
    // Второе исключение режет уже оставшуюся часть: сверху справа квадрат 20×10.
    let corner = RenderViewport::new(80, 0, 20, 10);
    assert_eq!(
        viewport.visible_parts(&[sidebar, corner]),
        vec![
            RenderViewport::new(30, 10, 70, 40),
            RenderViewport::new(30, 0, 50, 10),
        ]
    );
    // Исключение на весь viewport — рисовать нечего.
    assert_eq!(viewport.visible_parts(&[viewport]), Vec::new());
}
