//! wgpu-вход кадра и буфер swapchain wgpu.
//!
//! Нейтральный словарь кадра (исходы, причины пропуска, тайминги, интерфейс) живёт в
//! `render-shell-api` и реэкспортируется из корня crate-а без изменения путей для `app-egui`.
//! Здесь остаются типы, в которых пока зашиты wgpu/winit: вход кадра с окном и
//! `WgpuRenderableFrame`, билет с `wgpu::SurfaceTexture`. Их нейтральные версии —
//! `render_shell_api::{RenderFrameInput, AcquiredSurfaceFrame}`; переход — сессия 02 плана
//! `user/gles2-renderer/`.

use std::time::Duration;

use render_core::RenderViewport;
use render_wgpu_video::WgpuRenderableFrame;
use winit::window::Window;

use crate::{
    EguiFrameContent, RenderFrameDropReason, RenderScreenDescriptor, SubtitleOverlayFrame,
    WindowCornerMask,
};

/// Входные данные одного полного кадра shell renderer-а.
///
/// App layer отвечает за egui tessellation и сбор video frame lease, а shell layer
/// получает уже готовый пакет данных для записи swapchain кадра.
pub struct RenderFrameInput<'frame> {
    /// Окно, для которого выполняется present notification.
    pub window: &'frame Window,

    /// Буфер swapchain, уже полученный `Renderer::acquire_surface_frame` ДО выбора
    /// видеокадра (сессия 06a): ожидание FIFO не делает выбранный кадр старше на vsync.
    pub surface: AcquiredSurfaceFrame,

    /// Video frame boundary; `None` означает, что target нужно очистить в чёрный.
    pub video_frame: Option<&'frame WgpuRenderableFrame<'frame>>,

    /// Свежие egui-примитивы либо просьба повторить прежние.
    pub egui: EguiFrameContent,

    /// Размер target-а и UI scale для egui-wgpu.
    pub screen: RenderScreenDescriptor,

    /// Renderer-neutral content viewport, по которому video pass сохраняет aspect ratio.
    pub video_viewport: RenderViewport,

    /// Renderer-neutral области, где video pass не должен рисовать кадр.
    pub video_exclusion_rects: Vec<RenderViewport>,

    /// Разрешённая app-слоем форма текущего desktop-кадра.
    pub window_corner_mask: WindowCornerMask,

    /// Субтитры между video и egui; `None` — рисовать нечего.
    pub subtitle_overlay: Option<SubtitleOverlayFrame<'frame>>,
}

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

/// Буфер swapchain, в который будет записан и показан текущий кадр (cpu-parity, сессия 06a).
///
/// Получается отдельно от записи кадра: под FIFO получение блокируется до свободного места
/// в очереди показа (под XWayland — почти весь интервал vsync). App берёт буфер первым и
/// только потом выбирает видеокадр и субтитры, поэтому на экран уходит кадр, выбранный
/// непосредственно перед показом. Не использованный буфер возвращается swapchain-у при
/// drop-е (wgpu `SurfaceTexture::drop` → discard).
pub struct AcquiredSurfaceFrame {
    /// Текстура swapchain текущего кадра.
    pub(crate) surface_texture: wgpu::SurfaceTexture,

    /// Сколько заняло получение (включая ожидание FIFO); идёт в стадию `surface_acquire`.
    pub(crate) acquire_elapsed: Duration,
}

impl AcquiredSurfaceFrame {
    /// Сколько главный поток ждал буфер swapchain.
    #[must_use]
    pub const fn acquire_elapsed(&self) -> Duration {
        self.acquire_elapsed
    }
}

/// Итог получения буфера swapchain.
#[must_use = "без буфера кадр не показан; причину пропуска обязан учесть app"]
pub enum SurfaceFrameAcquire {
    /// Буфер получен: можно выбирать видеокадр и вызывать `Renderer::render_frame`.
    Acquired(AcquiredSurfaceFrame),
    /// Буфер не получен (окно скрыто, surface потерян/устарел, таймаут): кадр пропущен
    /// до выбора видеокадра, кадр плеера остаётся в передаче для следующего показа.
    Dropped(RenderFrameDropReason),
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
