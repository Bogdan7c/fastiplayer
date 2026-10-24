//! Зажим геометрии видео из приложения к текущей цели показа.
//!
//! Нейтральный словарь кадра (вход кадра, билет буфера, исходы, причины пропуска, тайминги)
//! живёт в `render-shell-api`. С сессии 02 плана `user/gles2-renderer/` wgpu-версий входа
//! кадра и билета больше нет: здесь остались только чистые функции зажима viewport-ов.

use render_core::RenderViewport;

use crate::RenderScreenDescriptor;

/// Зажимает app-computed video viewport к текущему swapchain target-у.
#[must_use]
pub(crate) fn clamp_video_viewport_to_screen(
    video_viewport: RenderViewport,
    screen: &RenderScreenDescriptor,
) -> RenderViewport {
    video_viewport.clamp_to_surface(screen.size_in_pixels[0], screen.size_in_pixels[1])
}

/// Зажимает app-computed video exclusion rects к текущему swapchain target-у.
#[must_use]
pub(crate) fn clamp_video_exclusion_rects_to_screen(
    video_exclusion_rects: Vec<RenderViewport>,
    screen: &RenderScreenDescriptor,
) -> Vec<RenderViewport> {
    let full_surface =
        RenderViewport::full_surface(screen.size_in_pixels[0], screen.size_in_pixels[1]);

    video_exclusion_rects
        .into_iter()
        .filter_map(|exclusion_rect| exclusion_rect.intersection(full_surface))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_viewport_clamp_uses_screen_descriptor_without_touching_egui_contract() {
        let screen = RenderScreenDescriptor {
            size_in_pixels: [800, 600],
            pixels_per_point: 1.5,
        };

        let viewport =
            clamp_video_viewport_to_screen(RenderViewport::new(300, 200, 800, 500), &screen);

        assert_eq!(viewport, RenderViewport::new(300, 200, 500, 400));
        assert_eq!(screen.pixels_per_point, 1.5);
    }

    #[test]
    fn invalid_video_viewport_clamp_defaults_to_full_screen() {
        let screen = RenderScreenDescriptor {
            size_in_pixels: [800, 600],
            pixels_per_point: 2.0,
        };

        let viewport = clamp_video_viewport_to_screen(RenderViewport::new(0, 0, 0, 300), &screen);

        assert_eq!(viewport, RenderViewport::full_surface(800, 600));
        assert_eq!(screen.pixels_per_point, 2.0);
    }

    #[test]
    fn video_exclusion_clamp_intersects_screen_without_fullscreen_fallback() {
        let screen = RenderScreenDescriptor {
            size_in_pixels: [800, 600],
            pixels_per_point: 1.0,
        };

        let exclusions = clamp_video_exclusion_rects_to_screen(
            vec![
                RenderViewport::new(0, 50, 420, 700),
                RenderViewport::new(900, 0, 100, 100),
            ],
            &screen,
        );

        assert_eq!(exclusions, vec![RenderViewport::new(0, 50, 420, 550)]);
    }
}
