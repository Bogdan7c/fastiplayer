//! Entrypoint приложения.
//!
//! Модуль отвечает только за запуск процесса:
//! - инициализацию tracing;
//! - загрузку пользовательского config;
//! - разбор initial media из CLI;
//! - создание winit event loop;
//! - запуск `AppShell`.

mod app_instance;
mod app_shell;
mod app_wake;
mod dma_buf_runtime_fallback;
mod frame_prepare;
mod local_file_open;
mod local_media;
mod media_open;
mod playlist_action_runtime;
mod playlist_runtime;
mod process_shutdown;
mod redraw_pacing;
mod render_settings;
mod renderer_recreation;
mod settings_runtime;
pub mod settings_ui;
mod startup_media;
mod state;
mod system_capabilities;
mod telemetry;
mod transport_runtime;
mod ui;
mod url_service_adapter;
mod url_topology_drafts;
mod video_pipeline_candidate;
mod video_pipeline_selector;
mod web_media_demux_registry;
mod web_media_hls_open;
mod web_media_hls_subtitles;
mod web_media_open;
mod web_media_quality;
mod web_media_stream_model;

use anyhow::{Context, Result};
use tracing::info;
use winit::event_loop::{ControlFlow, EventLoop};

use crate::app_instance::{ProcessBootstrap, bootstrap_process};
use crate::app_shell::AppShell;
use crate::app_wake::{AppWakeEvent, AppWakeProxy};
use crate::startup_media::InitialMedia;

/// Точка входа приложения.
///
/// Shell lifecycle живёт в `app_shell`; здесь остаётся только процессный bootstrap.
fn main() -> Result<()> {
    let ProcessBootstrap {
        config_paths,
        instance_lease,
        loaded_config,
        initial_media,
        startup_error: cli_startup_error,
    } = bootstrap_process().context("Process bootstrap rustiplayer завершился ошибкой")?;

    // Инициализируем tracing.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    info!("=== rustiplayer ===");
    info!("Запуск приложения");

    info!(created = loaded_config.created, "Config rustiplayer готов");

    // Один typed event loop принимает только лёгкие owner wake events.
    let event_loop = EventLoop::<AppWakeEvent>::with_user_event()
        .build()
        .context("Не удалось создать event loop")?;
    // Ровно один process proxy передаётся shell-у через cloneable owner ports.
    let wake_proxy = AppWakeProxy::new(event_loop.create_proxy());

    // Idle default — Wait; playback включает Poll только на активном render loop-е.
    event_loop.set_control_flow(ControlFlow::Wait);

    if matches!(initial_media, Some(InitialMedia::File(_))) {
        info!("CLI аргумент: локальный файл для воспроизведения");
    }

    let mut app = AppShell::new(
        initial_media,
        cli_startup_error,
        loaded_config,
        wake_proxy,
        config_paths,
        instance_lease,
    )
    .context("Не удалось создать settings runtime app shell")?;
    let event_loop_result = event_loop.run_app(&mut app);
    // `exiting` обычно уже выполнил этот path; явный idempotent вызов также
    // защищает error-return event loop-а от обычного Drop незавершённых owners.
    app.finish_process_shutdown();
    event_loop_result?;

    info!("Приложение завершено");
    Ok(())
}
