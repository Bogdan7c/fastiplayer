//! Entrypoint приложения.
//!
//! Модуль отвечает только за запуск процесса:
//! - инициализацию tracing (первым делом, чтобы ошибки bootstrap попали в лог);
//! - загрузку пользовательского config;
//! - разбор initial media из CLI;
//! - создание winit event loop;
//! - запуск `AppShell`;
//! - показ фатальной ошибки запуска и код выхода (`fatal_startup`).

mod app_instance;
mod app_shell;
mod app_wake;
mod config_startup_notice;
mod dma_buf_runtime_fallback;
mod fatal_startup;
mod frame_prepare;
mod local_file_open;
mod local_open_message;
mod media_open;
mod playlist_action_runtime;
mod playlist_runtime;
mod playlist_skip_message;
mod process_shutdown;
mod redraw_pacing;
mod render_settings;
mod renderer_recreation;
mod settings_runtime;
pub mod settings_ui;
mod startup_media;
mod startup_readiness;
mod state;
mod system_capabilities;
mod telemetry;
mod transport_runtime;
mod ui;
mod url_service_adapter;
mod url_topology_drafts;
mod video_backend_constraint;
mod video_pipeline_candidate;
mod video_pipeline_selector;
mod web_media_catalog;
mod web_media_open;
mod web_media_stream_model;
mod web_open_message;
mod window_corner_policy;

// Архитектурные сторожа provider DTO yt-dlp по исходникам app-egui.
#[cfg(test)]
mod extractor_provider_dto_guard_tests;

use std::process::ExitCode;

use tracing::info;
use winit::event_loop::{ControlFlow, EventLoop};

use crate::app_instance::{ProcessBootstrap, bootstrap_process};
use crate::app_shell::AppShell;
use crate::app_wake::{AppWakeEvent, AppWakeProxy};
use crate::fatal_startup::{FatalStartupError, SystemFatalStartupPresenter, conclude_process};
use crate::startup_media::InitialMedia;

/// Точка входа приложения.
///
/// Shell lifecycle живёт в `app_shell`; здесь остаётся только процессный bootstrap.
/// Любая фатальная ошибка запуска показывается пользователю и даёт ненулевой код
/// выхода; штатное завершение — код 0.
fn main() -> ExitCode {
    let process_started_at = std::time::Instant::now();
    init_tracing();
    let run_outcome = run_application(process_started_at);
    conclude_process(run_outcome, &SystemFatalStartupPresenter).exit_code()
}

/// Инициализирует tracing до bootstrap: ошибки аргументов, lease и config
/// тоже попадают в лог. Фильтр берётся из `RUST_LOG`, по умолчанию `info`.
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}

/// Запускает приложение до закрытия окна или до первой фатальной ошибки запуска.
fn run_application(process_started_at: std::time::Instant) -> Result<(), FatalStartupError> {
    let ProcessBootstrap {
        config_paths,
        instance_lease,
        loaded_config,
        initial_media,
        startup_error: cli_startup_error,
    } = bootstrap_process().map_err(|error| FatalStartupError::from_bootstrap_error(&error))?;

    info!(
        process_elapsed_ms = process_started_at.elapsed().as_secs_f64() * 1_000.0,
        "=== fastiplayer ==="
    );
    info!("Запуск приложения");

    info!(
        process_elapsed_ms = process_started_at.elapsed().as_secs_f64() * 1_000.0,
        "Process config/instance bootstrap complete"
    );

    config_startup_notice::log_config_load_outcome(&loaded_config);

    // Один typed event loop принимает только лёгкие owner wake events.
    let event_loop = EventLoop::<AppWakeEvent>::with_user_event()
        .build()
        .map_err(|error| FatalStartupError::graphical_session_unavailable(&error))?;
    info!(
        process_elapsed_ms = process_started_at.elapsed().as_secs_f64() * 1_000.0,
        "Process event loop ready"
    );
    // Ровно один process proxy передаётся shell-у через cloneable owner ports.
    let wake_proxy = AppWakeProxy::new(event_loop.create_proxy());

    // Idle default — Wait; playback включает Poll только на активном render loop-е.
    event_loop.set_control_flow(ControlFlow::Wait);

    if matches!(initial_media, Some(InitialMedia::File(_))) {
        info!("CLI аргумент: локальный файл для воспроизведения");
    }

    let mut app = AppShell::new(
        process_started_at,
        initial_media,
        cli_startup_error,
        loaded_config,
        wake_proxy,
        config_paths,
        instance_lease,
    )
    .map_err(|error| {
        FatalStartupError::internal_failure("Не удалось создать settings runtime app shell", &error)
    })?;
    let event_loop_result = event_loop.run_app(&mut app);
    // `exiting` обычно уже выполнил этот path; явный idempotent вызов также
    // защищает error-return event loop-а от обычного Drop незавершённых owners.
    app.finish_process_shutdown();
    let fatal_error_inside_event_loop = app.take_fatal_startup_error();
    // Shell (и вместе с ним lease) освобождается до показа ошибки: окно ошибки ждёт
    // пользователя, и повторный запуск в это время не должен упираться в «уже запущен».
    drop(app);

    if let Some(fatal_error) = fatal_error_inside_event_loop {
        return Err(fatal_error);
    }
    event_loop_result.map_err(|error| {
        FatalStartupError::internal_failure("Event loop завершился ошибкой", &error)
    })?;

    info!("Приложение завершено");
    Ok(())
}
