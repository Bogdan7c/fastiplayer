use std::time::Duration;

use render_core::RenderViewport;
use render_wgpu_video::WgpuRenderableFrame;
use winit::window::Window;

/// Итог одного render-frame вызова.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderFrameOutcome {
    /// Кадр был отправлен в swapchain и представлен.
    Presented(RenderFrameTiming),

    /// Кадр был пропущен из-за состояния surface/window.
    Dropped(RenderFrameDropReason),

    /// Video render path failed; caller must treat this as fatal media error.
    Failed(RenderFrameFailure),
}

/// Timing внутренних стадий одного `Renderer::render_frame`.
///
/// Эти значения остаются на shell boundary: app/player layer видит только
/// длительности стадий, но не получает доступ к wgpu surface/device/queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderFrameStageTimings {
    /// Обновление egui texture atlas-а перед созданием command buffer-а.
    pub egui_texture_update: Duration,

    /// Создание command encoder-а для текущего кадра.
    pub encoder_creation: Duration,

    /// Обновление egui vertex/index buffers.
    pub egui_buffer_update: Duration,

    /// Получение swapchain texture через `surface.get_current_texture()`.
    pub surface_acquire: Duration,

    /// Создание view поверх полученной surface texture.
    pub surface_view_creation: Duration,

    /// Video pass или очистка target-а.
    pub video_render: Duration,

    /// Egui overlay pass поверх video output.
    pub egui_render: Duration,

    /// Синхронная часть `queue.submit(...)`.
    pub queue_submit: Duration,

    /// Неблокирующий `device.poll(wgpu::PollType::Poll)`.
    pub device_poll: Duration,

    /// Уведомление winit перед present.
    pub pre_present_notify: Duration,

    /// Синхронная часть `surface_texture.present()`.
    pub surface_present: Duration,
}

impl RenderFrameStageTimings {
    /// Возвращает сумму стадий submit/poll/present для старого aggregate counter-а.
    #[must_use]
    pub fn submit_present_elapsed(self) -> Duration {
        self.queue_submit + self.device_poll + self.pre_present_notify + self.surface_present
    }

    /// Возвращает самую дорогую стадию внутри renderer-owned части кадра.
    #[must_use]
    pub fn slowest_stage(self) -> RenderFrameSlowestStage {
        let mut slowest_stage = RenderFrameSlowestStage {
            name: "egui_texture_update",
            elapsed: self.egui_texture_update,
        };

        for candidate in [
            ("encoder_creation", self.encoder_creation),
            ("egui_buffer_update", self.egui_buffer_update),
            ("surface_acquire", self.surface_acquire),
            ("surface_view_creation", self.surface_view_creation),
            ("video_render", self.video_render),
            ("egui_render", self.egui_render),
            ("queue_submit", self.queue_submit),
            ("device_poll", self.device_poll),
            ("pre_present_notify", self.pre_present_notify),
            ("surface_present", self.surface_present),
        ] {
            if candidate.1 > slowest_stage.elapsed {
                slowest_stage = RenderFrameSlowestStage {
                    name: candidate.0,
                    elapsed: candidate.1,
                };
            }
        }

        slowest_stage
    }
}

/// Самая дорогая renderer-owned стадия кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderFrameSlowestStage {
    /// Стабильное имя стадии для logs/diagnostics.
    pub name: &'static str,

    /// Измеренная длительность стадии.
    pub elapsed: Duration,
}

/// Timing одного успешного render loop-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderFrameTiming {
    /// Время от отправки command buffer-а до возврата из `surface_texture.present()`.
    pub submit_present_elapsed: Duration,

    /// Полная длительность renderer-owned части от входа в shell до present.
    pub renderer_elapsed: Duration,

    /// Разбивка renderer-owned path по стадиям.
    pub stages: RenderFrameStageTimings,
}

impl RenderFrameTiming {
    /// Собирает итоговый timing и сохраняет старый aggregate submit/present counter.
    #[must_use]
    pub fn new(stages: RenderFrameStageTimings, renderer_elapsed: Duration) -> Self {
        Self {
            submit_present_elapsed: stages.submit_present_elapsed(),
            renderer_elapsed,
            stages,
        }
    }
}

/// Размер target-а и UI scale без раскрытия egui-wgpu типа наружу.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderScreenDescriptor {
    /// Размер swapchain target-а в пикселях.
    pub size_in_pixels: [u32; 2],

    /// UI scale, полученный от egui context.
    pub pixels_per_point: f32,
}

/// Входные данные одного полного кадра shell renderer-а.
///
/// App layer отвечает за egui tessellation и сбор video frame lease, а shell layer
/// получает уже готовый пакет данных для записи swapchain кадра.
pub struct RenderFrameInput<'frame> {
    /// Окно, для которого выполняется present notification.
    pub window: &'frame Window,

    /// Video frame boundary; `None` означает, что target нужно очистить в чёрный.
    pub video_frame: Option<&'frame WgpuRenderableFrame<'frame>>,

    /// Уже tessellated egui primitives.
    pub egui_paint_jobs: Vec<egui::epaint::ClippedPrimitive>,

    /// Изменения egui textures для текущего кадра.
    pub egui_textures_delta: egui::TexturesDelta,

    /// Размер target-а и UI scale для egui-wgpu.
    pub screen: RenderScreenDescriptor,

    /// Renderer-neutral content viewport, по которому video pass сохраняет aspect ratio.
    pub video_viewport: RenderViewport,

    /// Renderer-neutral области, где video pass не должен рисовать кадр.
    pub video_exclusion_rects: Vec<RenderViewport>,
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

/// Ошибка video render path, которую app/player layer не должен превращать в fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderFrameFailure {
    /// Сообщение renderer-а для логов и UI.
    pub message: String,
}

impl RenderFrameFailure {
    /// Создаёт failure из renderer error.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Причина пропуска кадра renderer backend-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFrameDropReason {
    /// Surface acquisition не успел завершиться.
    SurfaceTimeout,

    /// Окно occluded, compositor не принимает кадр.
    SurfaceOccluded,

    /// Surface потерян; renderer выполнил reconfigure и ждёт следующий redraw.
    SurfaceLost,

    /// Surface validation error при acquisition.
    SurfaceValidation,

    /// Повторный acquisition после Outdated/Reconfigure тоже не дал frame.
    SurfaceOutdatedRecoveryFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что старый aggregate counter остаётся суммой submit/poll/present стадий.
    #[test]
    fn render_timing_keeps_submit_present_aggregate() {
        let stages = RenderFrameStageTimings {
            queue_submit: Duration::from_millis(1),
            device_poll: Duration::from_millis(2),
            pre_present_notify: Duration::from_millis(3),
            surface_present: Duration::from_millis(4),
            ..RenderFrameStageTimings::default()
        };

        let timing = RenderFrameTiming::new(stages, Duration::from_millis(20));

        assert_eq!(timing.submit_present_elapsed, Duration::from_millis(10));
        assert_eq!(timing.renderer_elapsed, Duration::from_millis(20));
    }

    /// Проверяет, что диагностика указывает самую дорогую renderer-owned стадию.
    #[test]
    fn render_stage_timing_reports_slowest_stage() {
        let stages = RenderFrameStageTimings {
            surface_acquire: Duration::from_millis(7),
            device_poll: Duration::from_millis(11),
            surface_present: Duration::from_millis(5),
            ..RenderFrameStageTimings::default()
        };

        let slowest_stage = stages.slowest_stage();

        assert_eq!(slowest_stage.name, "device_poll");
        assert_eq!(slowest_stage.elapsed, Duration::from_millis(11));
    }

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
