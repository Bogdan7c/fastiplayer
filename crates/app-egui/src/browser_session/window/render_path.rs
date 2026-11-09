//! Рендер окна браузерного режима глазами процесса до появления окна (сессия 13a).
//!
//! Две вещи, общие для окна и для метки кодеков:
//! - метка незавершённого старта рендера обычного плеера — только чтение
//!   (`ReadOnlyRendererStartupMarker`, своей метки окно моста не ставит, ограничение 04a);
//! - прогноз «на какой рендер встанет окно» для метки `cheap_codec.*` в `hello`.
//!
//! Прогноз нужен потому, что метка уходит экстеншену раньше, чем создаётся окно, а профиль
//! `Auto` на машине без аппаратного Vulkan молча встаёт на GLES2 (тянет дёшево только H.264).

use std::time::Instant;

use crate::app_shell::ReadOnlyRendererStartupMarker;
use crate::browser_session::codec_label::VideoRenderPath;
use crate::browser_session::session_config::BrowserSessionSettings;
use crate::render_backend::{UnfinishedStartupTracker, forecast_window_renderer};

/// Метка старта рендера обычного плеера — только для чтения.
pub(super) fn renderer_startup_marker() -> ReadOnlyRendererStartupMarker {
    match fastiplayer_config::ConfigPaths::discover() {
        Ok(paths) => ReadOnlyRendererStartupMarker::read_from(paths.renderer_startup_marker_file()),
        // Каталога config-а нет — нет и метки: выбор идёт как на чистой машине.
        Err(_) => ReadOnlyRendererStartupMarker::absent(),
    }
}

/// Путь рендера для метки кодеков: прогноз того же выбора, что сделает окно.
///
/// Пробы настоящие (Vulkan instance, при необходимости EGL), поэтому длительность пишется в
/// журнал: она ложится на время активации процесса до ответа на первый `hello`.
pub(super) fn forecast_video_render_path(settings: &BrowserSessionSettings) -> VideoRenderPath {
    let started = Instant::now();
    let forecast = forecast_window_renderer(
        settings.render_profile(),
        renderer_startup_marker().unfinished_startup(),
    );
    tracing::info!(
        ?forecast,
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "Forecast the browser window renderer for the codec label"
    );
    VideoRenderPath::from_window_forecast(forecast)
}
