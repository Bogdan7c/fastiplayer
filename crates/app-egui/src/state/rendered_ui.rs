//! Выходные типы `AppState::render_ui`: то, что UI-кадр отдаёт shell-у после egui closure.
//!
//! Вынесено из `state.rs` без изменения поведения, чтобы корневой модуль `AppState` оставался
//! в пределах лимита размера модуля.

use std::time::Duration;

use crate::settings_ui::SettingsUiAction;
use crate::ui::sidebar::SidebarWidthChange;
use crate::ui::window_chrome::WindowChromeAction;

/// CPU timing внутренних частей `AppState::render_ui`.
///
/// Структура остаётся internal API `app-egui`: `frame_prepare` получает только
/// длительности и не знает деталей хранения UI-состояния.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AppUiRenderTimings {
    /// Полная длительность вызова `render_ui`.
    pub(crate) total: Duration,

    /// Подготовка snapshot-derived значений до входа в egui closure.
    pub(crate) pre_ui_setup: Duration,

    /// Полный `egui_ctx.run_ui`, включая все панели и layout.
    pub(crate) egui_run: Duration,

    /// Рендер верхней панели.
    pub(crate) top_bar: Duration,

    /// Рендер нижних controls и timeline.
    pub(crate) bottom_controls: Duration,

    /// Рендер telemetry панели, если она включена в config.
    pub(crate) telemetry_panel: Duration,

    /// Рендер центрального overlay.
    pub(crate) center_overlay: Duration,

    /// Применение UI actions после egui closure.
    pub(crate) post_ui_actions: Duration,

    /// Была ли telemetry панель включена в этом кадре.
    pub(crate) telemetry_panel_visible: bool,
}

/// Согласованные Playlist snapshots одного egui frame-а.
pub(crate) struct PlaylistUiFrameModels<'a> {
    /// S08 staged import preview показывается до queue/sensitive confirmation.
    pub(crate) import_preview: Option<&'a crate::playlist_runtime::PlaylistImportPreview>,
    /// Единственная process-lifetime confirmation entity текущего кадра.
    pub(crate) confirmation: Option<&'a crate::playlist_runtime::PendingPlaylistConfirmation>,
    /// Immutable toolbar/forms/progress snapshot authoritative runtime-а.
    pub(crate) interaction: &'a crate::playlist_runtime::PlaylistInteractionModel,
    /// Global transport controls используют согласованный snapshot того же кадра.
    pub(crate) transport: &'a crate::playlist_runtime::PlaylistTransportUiModel,
    /// Toolbar Undo получает отдельный read-only snapshot и runtime deadline.
    pub(crate) undo: &'a crate::playlist_runtime::PlaylistUndoUiSnapshot,
}

/// Результат app-owned UI подготовки до platform output и tessellation.
pub(crate) struct RenderedAppUi {
    /// Полный output egui за текущий кадр.
    pub(crate) full_output: egui::FullOutput,

    /// Visual settings actions, которые shell передаст authoritative runtime owner-у.
    pub(crate) settings_actions: Vec<SettingsUiAction>,

    /// Fully-open drag-resize общего sidebar; persistence остаётся у SettingsRuntime.
    pub(crate) sidebar_width_change: Option<SidebarWidthChange>,

    /// Typed transport intents применяются только после завершения egui closure.
    pub(crate) transport_actions: Vec<crate::ui::player_controls::TransportControlAction>,

    /// Window chrome actions, которые shell применит через winit boundary.
    pub(crate) window_chrome_actions: Vec<WindowChromeAction>,

    /// Typed response central confirmation entity; authoritative intent остаётся в runtime.
    pub(crate) playlist_confirmation_action:
        Option<crate::playlist_runtime::PlaylistConfirmationAction>,

    /// Playlist toolbar/form actions применяются shell-ом после egui closure.
    pub(crate) playlist_actions: Vec<crate::ui::playlist::PlaylistAction>,

    /// Typed URL candidate intent применяется после egui closure без queue mutation.
    pub(crate) url_sidebar_action: Option<crate::web_media_stream_model::UrlSidebarAction>,

    /// Bounded read-only visibility hint для demand metadata refresh.
    pub(crate) playlist_visible_items_hint: Option<crate::ui::playlist::PlaylistVisibleItemsHint>,

    /// Область video underlay в egui points; overlay-панели её не уменьшают.
    pub(crate) video_viewport_rect: egui::Rect,

    /// UI-области, под которыми video pass не должен рисовать кадр.
    pub(crate) video_exclusion_rects: Vec<egui::Rect>,

    /// Контур кадра, разрешённый из committed config и текущего состояния окна.
    pub(crate) window_corner_mask: render_wgpu_shell::WindowCornerMask,

    /// Timing внутренних участков `render_ui`.
    pub(crate) timings: AppUiRenderTimings,
}
