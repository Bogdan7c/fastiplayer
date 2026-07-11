//! Подготовка egui output до входа в renderer/surface critical path.

use std::time::{Duration, Instant};

use render_core::RenderViewport;
use winit::window::Window;

use super::geometry::{video_exclusion_from_ui_rect, video_viewport_from_ui_rect};
use crate::settings_runtime::SettingsRuntime;
use crate::state::{AppFrameContext, AppState, AppUiRenderTimings};
use crate::ui::window_chrome::WindowChromeAction;

/// Результат UI stage, полностью готовый для renderer submit.
pub(super) struct PreparedUiFrame {
    pub(super) paint_jobs: Vec<egui::epaint::ClippedPrimitive>,
    pub(super) textures_delta: egui::TexturesDelta,
    pub(super) screen: render_wgpu_shell::RenderScreenDescriptor,
    pub(super) video_viewport: RenderViewport,
    pub(super) video_exclusion_rects: Vec<RenderViewport>,
    pub(super) requested_repaint: bool,
    pub(super) settings_actions: Vec<crate::settings_ui::SettingsUiAction>,
    pub(super) window_chrome_actions: Vec<WindowChromeAction>,
    pub(super) timings: UiPrepareTimings,
}

/// CPU-разбивка app-owned UI stage.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct UiPrepareTimings {
    pub(super) total: Duration,
    pub(super) app_ui: AppUiRenderTimings,
    pub(super) repaint_query: Duration,
    pub(super) platform_output: Duration,
    pub(super) tessellate: Duration,
    pub(super) screen_descriptor: Duration,
}

/// Готовит egui output, сохраняя lifecycle order нативной интеграции.
pub(super) fn prepare_ui_frame(
    window: &Window,
    app_state: &mut AppState,
    settings_runtime: &mut SettingsRuntime,
    egui_input: egui::RawInput,
    frame_context: &AppFrameContext,
) -> PreparedUiFrame {
    let ui_prepare_started_at = Instant::now();
    settings_runtime.poll_dynamic_options_refresh();

    let settings_panel_open = settings_runtime.is_settings_window_open();
    app_state.advance_sidebar_slide(settings_panel_open, Instant::now());
    settings_runtime
        .set_visual_hold(!settings_panel_open && app_state.sidebar_slide_is_animating());

    let settings_ui_model = settings_runtime.ui_model();
    let rendered_app_ui = app_state.render_ui(window, egui_input, frame_context, settings_ui_model);
    let crate::state::RenderedAppUi {
        full_output: egui_full_output,
        settings_actions,
        window_chrome_actions,
        video_viewport_rect,
        video_exclusion_rects,
        timings: app_ui_timings,
    } = rendered_app_ui;

    let stage_started_at = Instant::now();
    let requested_repaint = app_state.egui_ctx.has_requested_repaint();
    let repaint_query_elapsed = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    app_state
        .egui_winit_state
        .handle_platform_output(window, egui_full_output.platform_output);
    let platform_output_elapsed = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    let pixels_per_point = app_state.egui_ctx.pixels_per_point();
    let size = window.inner_size();
    let screen_size_in_pixels = [size.width.max(1), size.height.max(1)];
    let video_viewport =
        video_viewport_from_ui_rect(video_viewport_rect, screen_size_in_pixels, pixels_per_point);
    let video_exclusion_rects = video_exclusion_rects
        .into_iter()
        .filter_map(|exclusion_rect| {
            video_exclusion_from_ui_rect(exclusion_rect, screen_size_in_pixels, pixels_per_point)
        })
        .collect();
    let screen_descriptor_elapsed = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    let paint_jobs = app_state
        .egui_ctx
        .tessellate(egui_full_output.shapes, pixels_per_point);
    let tessellate_elapsed = stage_started_at.elapsed();

    PreparedUiFrame {
        paint_jobs,
        textures_delta: egui_full_output.textures_delta,
        screen: render_wgpu_shell::RenderScreenDescriptor {
            size_in_pixels: screen_size_in_pixels,
            pixels_per_point,
        },
        video_viewport,
        video_exclusion_rects,
        requested_repaint,
        settings_actions,
        window_chrome_actions,
        timings: UiPrepareTimings {
            total: ui_prepare_started_at.elapsed(),
            app_ui: app_ui_timings,
            repaint_query: repaint_query_elapsed,
            platform_output: platform_output_elapsed,
            tessellate: tessellate_elapsed,
            screen_descriptor: screen_descriptor_elapsed,
        },
    }
}
