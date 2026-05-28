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
mod local_file_open;
mod local_media;
mod redraw_pacing;
mod startup_media;
mod state;
mod telemetry;
mod ui;

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

    info!("=== YouTube Player Stage 1: Render Shell ===");
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

    let mut app = AppShell::new(initial_media, cli_startup_error, loaded_config.config);
    event_loop.run_app(&mut app)?;

    info!("Приложение завершено");
    Ok(())
}
