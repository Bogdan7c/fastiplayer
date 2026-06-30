//! Entrypoint приложения.
//!
//! Модуль отвечает только за запуск процесса:
//! - инициализацию tracing;
//! - загрузку пользовательского config;
//! - разбор initial media из CLI;
//! - создание winit event loop;
//! - запуск `AppShell`.

mod app_shell;
mod frame_prepare;
mod hover_software_session;
mod local_file_open;
mod local_media;
mod redraw_pacing;
mod render_settings;
mod settings_runtime;
pub mod settings_ui;
mod startup_media;
mod state;
mod system_capabilities;
mod telemetry;
mod timeline_hover_intent;
mod timeline_hover_prepare;
mod timeline_hover_source;
mod ui;
mod video_pipeline_selector;

use anyhow::{Context, Result};
use tracing::info;
use winit::event_loop::{ControlFlow, EventLoop};

use crate::app_shell::AppShell;
use crate::startup_media::{InitialMedia, resolve_initial_media_from_cli};

/// Точка входа приложения.
///
/// Shell lifecycle живёт в `app_shell`; здесь остаётся только процессный bootstrap.
fn main() -> Result<()> {
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

    let loaded_config =
        rustiplayer_config::load_or_create().context("Не удалось загрузить config rustiplayer")?;
    info!(
        path = %loaded_config.path.display(),
        created = loaded_config.created,
        "Config rustiplayer готов"
    );

    // Создаём event loop.
    let event_loop = EventLoop::builder()
        .build()
        .context("Не удалось создать event loop")?;

    // Idle default — Wait; playback включает Poll только на активном render loop-е.
    event_loop.set_control_flow(ControlFlow::Wait);

    let (initial_media, cli_startup_error) = resolve_initial_media_from_cli(&loaded_config.config);
    if let Some(InitialMedia::File(path)) = &initial_media {
        info!(path = %path.display(), "CLI аргумент: файл для воспроизведения");
    }

    let mut app = AppShell::new(initial_media, cli_startup_error, loaded_config)
        .context("Не удалось создать settings runtime app shell")?;
    event_loop.run_app(&mut app)?;

    info!("Приложение завершено");
    Ok(())
}
