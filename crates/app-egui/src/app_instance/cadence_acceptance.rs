//! Ручной Linux acceptance: настоящий AppShell, worker, decoder и swapchain.
//! Барьеры находятся только в test subscriber, production scheduler не подменяется.

use std::time::{Duration, Instant};

use fastiplayer_config::ConfigPaths;
use tracing_subscriber::prelude::*;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::x11::EventLoopBuilderExtX11;
use winit::window::WindowId;

use super::{NativeAppInstanceLeasePlatform, bootstrap_with};
use crate::app_shell::AppShell;
use crate::app_wake::{AppWakeEvent, AppWakeProxy};
use crate::startup_media::resolve_initial_media_argument;

#[path = "cadence_acceptance/gate.rs"]
mod gate;
use gate::{CadenceGate, PublicationWindow};

/// Wrapper только завершает bounded эксперимент; каждый callback передаётся AppShell.
struct AcceptanceApp {
    app: AppShell,
    gate: CadenceGate,
    deadline: Instant,
}

impl AcceptanceApp {
    fn exit_if_finished(&self, event_loop: &ActiveEventLoop) {
        if Instant::now() >= self.deadline {
            self.gate
                .abort("startup/render acceptance deadline exceeded");
        }
        if self.gate.finished() {
            event_loop.exit();
        }
    }
}

impl ApplicationHandler<AppWakeEvent> for AcceptanceApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.app.resumed(event_loop);
        self.exit_if_finished(event_loop);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.app.suspended(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppWakeEvent) {
        self.app.user_event(event_loop, event);
        self.exit_if_finished(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.app.window_event(event_loop, id, event);
        self.exit_if_finished(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.app.about_to_wait(event_loop);
        self.exit_if_finished(event_loop);
        // Даже ошибка startup без redraw/wake должна закончиться проверяемым timeout.
        if matches!(event_loop.control_flow(), ControlFlow::Wait) {
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.deadline));
        }
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        // Сначала отпускаем все test barriers, затем штатный shutdown завершает owners.
        self.gate.release_worker();
        self.app.exiting(event_loop);
    }
}

#[test]
#[ignore = "manual X11/VA-API acceptance; run alone with CADENCE_MEDIA and CADENCE_CONFIG"]
fn published_during_acquisition_reaches_same_surface_handoff() {
    run_acceptance(PublicationWindow::DuringAcquisition);
}

#[test]
#[ignore = "manual X11/VA-API control; run alone with CADENCE_MEDIA and CADENCE_CONFIG"]
fn published_before_preparation_reaches_same_surface_handoff() {
    run_acceptance(PublicationWindow::BeforePreparation);
}

fn run_acceptance(window: PublicationWindow) {
    let media =
        std::env::var_os("CADENCE_MEDIA").expect("CADENCE_MEDIA: local H.264 60fps fixture");
    let config =
        std::env::var_os("CADENCE_CONFIG").expect("CADENCE_CONFIG: isolated baseline TOML");
    assert!(
        std::path::Path::new(&config).is_file(),
        "CADENCE_CONFIG must already exist"
    );
    let directory = tempfile::tempdir().expect("isolated config directory");
    let paths = ConfigPaths::from_config_dir(directory.path().join("fastiplayer"));
    let started_at = Instant::now();
    let gate = CadenceGate::new(window);
    // Отдельный test process: global subscriber нужен настоящему worker thread.
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            "info,fastiplayer::frame_cadence=trace,fastiplayer::video_render_acceptance=trace",
        ))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(std::io::stderr),
        )
        .with(gate.clone())
        .try_init()
        .expect("acceptance must run alone in its own test process");

    let bootstrap = bootstrap_with(
        [media],
        || Ok(paths),
        &NativeAppInstanceLeasePlatform,
        |paths| {
            // Config input только читается; все последующие записи принадлежат tempdir.
            let mut loaded = fastiplayer_config::load_or_create_at(&config)?;
            loaded.path = paths.config_file.clone();
            Ok(loaded)
        },
        |args, _paths, loaded| {
            resolve_initial_media_argument(args.take_initial_media(), &loaded.config)
        },
    )
    .expect("real process bootstrap");
    assert!(
        bootstrap.prepared.1.is_none(),
        "fixture classification failed"
    );
    let mut builder = EventLoop::<AppWakeEvent>::with_user_event();
    builder.with_x11().with_any_thread(true); // any_thread: Rust test runner thread.
    let event_loop = builder.build().expect("X11 event loop");
    let wake_proxy = AppWakeProxy::new(event_loop.create_proxy());
    let app = AppShell::new(
        started_at,
        bootstrap.prepared.0,
        None,
        bootstrap.config,
        wake_proxy,
        bootstrap.paths,
        bootstrap.lease,
    )
    .expect("real AppShell");
    let mut acceptance = AcceptanceApp {
        app,
        gate: gate.clone(),
        deadline: started_at + Duration::from_secs(25),
    };
    let result = event_loop.run_app(&mut acceptance);
    gate.release_worker();
    acceptance.app.finish_process_shutdown();
    result.expect("run production event loop");
    // Ошибка среды/barrier отличается от воспроизведённого freshness assertion.
    gate.assert_fresh_handoff();
}
