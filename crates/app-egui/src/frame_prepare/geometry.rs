//! Чистое преобразование геометрии egui в renderer-neutral physical viewport.

use render_core::RenderViewport;

/// Переводит egui coordinate в physical pixel с округлением вниз.
fn physical_pixel_floor(point: f32, pixels_per_point: f32) -> u32 {
    let scaled_point = point.max(0.0) * pixels_per_point;
    scaled_point.floor() as u32
}

/// Переводит верхнюю или правую границу в physical pixel с округлением вверх.
fn physical_pixel_ceil(point: f32, pixels_per_point: f32) -> u32 {
    let scaled_point = point.max(0.0) * pixels_per_point;
    scaled_point.ceil() as u32
}

/// Возвращает безопасный UI scale для конвертации egui points в physical pixels.
fn safe_pixels_per_point(pixels_per_point: f32) -> f32 {
    if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    }
}

/// Конвертирует egui rect в physical viewport без surface fallback-а.
fn raw_viewport_from_ui_rect(video_rect: egui::Rect, pixels_per_point: f32) -> RenderViewport {
    let safe_scale = safe_pixels_per_point(pixels_per_point);
    let min_x = physical_pixel_floor(video_rect.min.x, safe_scale);
    let min_y = physical_pixel_floor(video_rect.min.y, safe_scale);
    let max_x = physical_pixel_ceil(video_rect.max.x, safe_scale);
    let max_y = physical_pixel_ceil(video_rect.max.y, safe_scale);

    RenderViewport::new(
        min_x,
        min_y,
        max_x.saturating_sub(min_x),
        max_y.saturating_sub(min_y),
    )
}

/// Конвертирует app-owned video underlay rect в renderer-neutral physical viewport.
pub(super) fn video_viewport_from_ui_rect(
    video_rect: egui::Rect,
    screen_size_in_pixels: [u32; 2],
    pixels_per_point: f32,
) -> RenderViewport {
    raw_viewport_from_ui_rect(video_rect, pixels_per_point)
        .clamp_to_surface(screen_size_in_pixels[0], screen_size_in_pixels[1])
}

/// Конвертирует UI exclusion rect в physical viewport; пустой rect игнорируется.
pub(super) fn video_exclusion_from_ui_rect(
    exclusion_rect: egui::Rect,
    screen_size_in_pixels: [u32; 2],
    pixels_per_point: f32,
) -> Option<RenderViewport> {
    let full_surface =
        RenderViewport::full_surface(screen_size_in_pixels[0], screen_size_in_pixels[1]);

    raw_viewport_from_ui_rect(exclusion_rect, pixels_per_point).intersection(full_surface)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_viewport_converts_points_to_physical_pixels() {
        let viewport = video_viewport_from_ui_rect(
            egui::Rect::from_min_max(egui::pos2(12.25, 7.5), egui::pos2(112.75, 57.25)),
            [400, 300],
            2.0,
        );

        assert_eq!(viewport, RenderViewport::new(24, 15, 202, 100));
    }

    #[test]
    fn invalid_scale_defaults_to_one_for_video_viewport() {
        let viewport = video_viewport_from_ui_rect(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 50.0)),
            [100, 50],
            f32::NAN,
        );

        assert_eq!(viewport, RenderViewport::full_surface(100, 50));
    }

    #[test]
    fn exclusion_rect_is_clamped_to_surface() {
        let exclusion = video_exclusion_from_ui_rect(
            egui::Rect::from_min_max(egui::pos2(80.0, 10.0), egui::pos2(140.0, 40.0)),
            [100, 50],
            1.0,
        );

        assert_eq!(exclusion, Some(RenderViewport::new(80, 10, 20, 30)));
    }

    #[test]
    fn empty_exclusion_rect_is_ignored() {
        let exclusion = video_exclusion_from_ui_rect(
            egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::Vec2::ZERO),
            [100, 50],
            1.0,
        );

        assert_eq!(exclusion, None);
    }
}
