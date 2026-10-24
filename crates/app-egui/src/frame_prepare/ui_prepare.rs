//! Подготовка egui output до входа в renderer/surface critical path.
//!
//! С сессии 04 проход делится на две части: [`advance_ui_timers`] (анимации сайдбара и панелей
//! фуллскрина, опрос dynamic options) выполняется на КАЖДОМ пробуждении, а [`prepare_ui_frame`]
//! (сам `run_ui`, platform output, tessellate) — только когда план кадра требует egui-проход.
//! Для показа видео с прежним интерфейсом кадр собирается из удержанной геометрии
//! ([`PreparedUiFrame::reusing_retained`]).

use std::time::{Duration, Instant};

use render_core::RenderViewport;
use render_shell_api::EguiFrameContent;
use winit::window::Window;

use super::geometry::{video_exclusion_from_ui_rect, video_viewport_from_ui_rect};
use crate::settings_runtime::SettingsRuntime;
use crate::state::{AppFrameContext, AppState, AppUiRenderTimings, RetainedUiGeometry};
use crate::ui::window_chrome::WindowChromeAction;

/// Результат UI stage, полностью готовый для renderer submit.
pub(super) struct PreparedUiFrame {
    /// Свежие примитивы egui или просьба повторить удержанные shell-ом.
    pub(super) egui_content: EguiFrameContent,
    pub(super) screen: render_shell_api::RenderScreenDescriptor,
    pub(super) video_viewport: RenderViewport,
    pub(super) video_exclusion_rects: Vec<RenderViewport>,
    pub(super) window_corner_mask: render_shell_api::WindowCornerMask,
    pub(super) settings_actions: Vec<crate::settings_ui::SettingsUiAction>,
    pub(super) sidebar_width_change: Option<crate::ui::sidebar::SidebarWidthChange>,
    pub(super) transport_actions: Vec<crate::ui::player_controls::TransportControlAction>,
    pub(super) window_chrome_actions: Vec<WindowChromeAction>,
    pub(super) playlist_confirmation_action:
        Option<crate::playlist_runtime::PlaylistConfirmationAction>,
    pub(super) playlist_actions: Vec<crate::ui::playlist::PlaylistAction>,
    pub(super) url_sidebar_action: Option<crate::web_media_stream_model::UrlSidebarAction>,
    pub(super) playlist_visible_items_hint: Option<crate::ui::playlist::PlaylistVisibleItemsHint>,
    pub(super) timings: UiPrepareTimings,
}

impl PreparedUiFrame {
    /// Кадр без egui-прохода: интерфейс повторяется из удержанных примитивов shell-а,
    /// геометрия — с последнего полного прохода. Действий панелей в таком кадре нет.
    pub(super) fn reusing_retained(geometry: &RetainedUiGeometry) -> Self {
        Self {
            egui_content: EguiFrameContent::ReusePrevious,
            screen: geometry.screen,
            video_viewport: geometry.video_viewport,
            video_exclusion_rects: geometry.video_exclusion_rects.clone(),
            window_corner_mask: geometry.window_corner_mask,
            settings_actions: Vec::new(),
            sidebar_width_change: None,
            transport_actions: Vec::new(),
            window_chrome_actions: Vec::new(),
            playlist_confirmation_action: None,
            playlist_actions: Vec::new(),
            url_sidebar_action: None,
            playlist_visible_items_hint: None,
            timings: UiPrepareTimings::default(),
        }
    }

    /// Геометрия этого кадра — запоминается после полного прохода для повторных показов.
    pub(super) fn retained_geometry(&self) -> RetainedUiGeometry {
        RetainedUiGeometry {
            screen: self.screen,
            video_viewport: self.video_viewport,
            video_exclusion_rects: self.video_exclusion_rects.clone(),
            window_corner_mask: self.window_corner_mask,
        }
    }
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

/// Продвигает таймеры интерфейса, не зависящие от egui-прохода: анимацию сайдбара, автоскрытие
/// панелей фуллскрина, опрос dynamic options. Вызывается на каждом пробуждении до решения о плане.
pub(super) fn advance_ui_timers(
    window: &Window,
    app_state: &mut AppState,
    settings_runtime: &mut SettingsRuntime,
    egui_input: &egui::RawInput,
    now: Instant,
) {
    settings_runtime.poll_dynamic_options_refresh();

    let settings_panel_open = settings_runtime.is_settings_window_open();
    app_state.advance_sidebar_slide(settings_panel_open, now);
    app_state.advance_fullscreen_chrome(window, egui_input, now);
    settings_runtime
        .set_visual_hold(!settings_panel_open && app_state.sidebar_slide_is_animating());
}

/// Готовит UI-кадр под план: полный egui-проход либо повтор удержанного интерфейса.
///
/// Возвращает и фактический план: если план не требовал прохода, а удержанной геометрии нет
/// (защитный случай, решение её проверяет), кадр честно идёт полным проходом.
pub(super) fn prepare_ui_frame_for_plan(
    plan: super::FrameWorkPlan,
    window: &Window,
    app_state: &mut AppState,
    settings_runtime: &mut SettingsRuntime,
    egui_input: egui::RawInput,
    frame_context: &AppFrameContext,
    playlist_models: crate::state::PlaylistUiFrameModels<'_>,
) -> (PreparedUiFrame, super::FrameWorkPlan) {
    if !plan.runs_ui_pass() {
        if let Some(geometry) = app_state.retained_ui_geometry() {
            return (PreparedUiFrame::reusing_retained(geometry), plan);
        }
        tracing::warn!(
            plan = plan.metric_name(),
            "No retained UI geometry for a frame without a UI pass; running a full pass instead"
        );
    }
    // Проход сам повторит запрос egui, если он ещё актуален; старый сбрасываем.
    let _consumed_request = app_state.take_egui_repaint_request();
    let prepared = prepare_ui_frame(
        window,
        app_state,
        settings_runtime,
        egui_input,
        frame_context,
        playlist_models,
    );
    (prepared, super::FrameWorkPlan::FullPass)
}

/// Готовит egui output, сохраняя lifecycle order нативной интеграции.
///
/// Запрос egui на перерисовку здесь не читается: его принимает callback контекста
/// (`AppState::take_egui_repaint_request`), который различает «сейчас» и «не позже момента».
pub(super) fn prepare_ui_frame(
    window: &Window,
    app_state: &mut AppState,
    settings_runtime: &mut SettingsRuntime,
    egui_input: egui::RawInput,
    frame_context: &AppFrameContext,
    playlist_models: crate::state::PlaylistUiFrameModels<'_>,
) -> PreparedUiFrame {
    let ui_prepare_started_at = Instant::now();

    let settings_ui_model = settings_runtime.ui_model();
    let rendered_app_ui = app_state.render_ui(
        window,
        egui_input,
        frame_context,
        settings_ui_model,
        playlist_models,
    );
    let crate::state::RenderedAppUi {
        full_output: egui_full_output,
        settings_actions,
        sidebar_width_change,
        transport_actions,
        window_chrome_actions,
        playlist_confirmation_action,
        playlist_actions,
        url_sidebar_action,
        playlist_visible_items_hint,
        video_viewport_rect,
        video_exclusion_rects,
        window_corner_mask,
        timings: app_ui_timings,
    } = rendered_app_ui;

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
        egui_content: EguiFrameContent::Fresh {
            paint_jobs,
            textures_delta: egui_full_output.textures_delta,
        },
        screen: render_shell_api::RenderScreenDescriptor {
            size_in_pixels: screen_size_in_pixels,
            pixels_per_point,
        },
        video_viewport,
        video_exclusion_rects,
        window_corner_mask,
        settings_actions,
        sidebar_width_change,
        transport_actions,
        window_chrome_actions,
        playlist_confirmation_action,
        playlist_actions,
        url_sidebar_action,
        playlist_visible_items_hint,
        timings: UiPrepareTimings {
            total: ui_prepare_started_at.elapsed(),
            app_ui: app_ui_timings,
            repaint_query: Duration::ZERO,
            platform_output: platform_output_elapsed,
            tessellate: tessellate_elapsed,
            screen_descriptor: screen_descriptor_elapsed,
        },
    }
}
