//! Окно браузерного режима (мост, 04a): тот же записанный поток, но видео рисует окно.
//!
//! Процесс будится частной шиной без тестовой переменной «без окна» и открывает
//! настоящее окно с рендером обычного плеера на ЧАСТНОМ виртуальном дисплее
//! (`kwin_wayland --virtual` со своим каталогом сеанса и своим сокетом): рабочий стол
//! разработчика не трогается. Проверяется результат, а не вызов: окно поднялось,
//! страница получила часы до конца ролика, в журнале каждый кадр ролика отдан на показ
//! рендеру окна, процесс вышел штатно.
//!
//! Закрытие окна человеком (04b) изображает НАСТОЯЩИЙ композитор: у частного kwin своя
//! частная шина D-Bus, тест загружает в него KWin-скрипт `closeWindow()` (интерфейс
//! `org.kde.kwin.Scripting`), и kwin шлёт окну обычный `xdg_toplevel.close` — тот же путь,
//! что крестик или Alt+F4. Нужен `busctl` (systemd), как и `dbus-daemon`.
//!
//! Нужен `kwin_wayland`; без него тест честно пропускается с сообщением. Запуск:
//! `cargo test -p app-egui --test browser_session_process window -- --nocapture`
//! (`FASTIPLAYER_TEST_ECHO_PROCESS_LOG=1` — весь журнал процесса в вывод).

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::*;

/// Сколько ждать сокет виртуального дисплея.
const DISPLAY_START_WAIT: Duration = Duration::from_secs(15);

/// Частный виртуальный дисплей Wayland (`kwin_wayland --virtual`).
pub(super) struct VirtualWaylandDisplay {
    compositor: Child,
    /// Частная шина композитора: через неё тест закрывает окна скриптом KWin.
    compositor_bus: Child,
    compositor_bus_address: String,
    /// Номер следующего скрипта: KWin молча не запускает повторно загруженное имя.
    next_script: std::cell::Cell<u32>,
    /// Каталог сеанса: короткий путь в `/tmp` — путь сокета Wayland ограничен 108 байтами.
    runtime_dir: PathBuf,
    /// Дом и XDG композитора: он тоже ничего не пишет в домашний каталог.
    state_dir: PathBuf,
    socket_name: String,
}

impl VirtualWaylandDisplay {
    /// Поднимает дисплей; `None` — `kwin_wayland` нет (тест пропускается).
    pub(super) fn start(name: &str) -> Option<Self> {
        let unique = format!("fp04a-{}-{name}", std::process::id());
        let runtime_dir = PathBuf::from("/tmp").join(&unique);
        let state_dir = std::env::temp_dir().join(format!("{unique}-kwin"));
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&state_dir).expect("compositor state dir");
        restrict_to_owner(&runtime_dir);
        let socket_name = "wl-fp04a".to_owned();
        let (compositor_bus, compositor_bus_address) =
            start_compositor_bus(&runtime_dir, &state_dir);
        let spawned = Command::new("kwin_wayland")
            .args([
                "--virtual",
                "--no-lockscreen",
                "--width",
                "1280",
                "--height",
                "720",
            ])
            .arg(format!("--socket={socket_name}"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &state_dir)
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("XDG_CONFIG_HOME", state_dir.join("config"))
            .env("XDG_DATA_HOME", state_dir.join("data"))
            .env("XDG_CACHE_HOME", state_dir.join("cache"))
            .env("XDG_STATE_HOME", state_dir.join("state"))
            .env("DBUS_SESSION_BUS_ADDRESS", &compositor_bus_address)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let compositor = match spawned {
            Ok(child) => child,
            Err(error) => {
                eprintln!("SKIPPED: kwin_wayland is not available ({error})");
                let mut compositor_bus = compositor_bus;
                let _ = compositor_bus.kill();
                let _ = compositor_bus.wait();
                let _ = std::fs::remove_dir_all(&runtime_dir);
                let _ = std::fs::remove_dir_all(&state_dir);
                return None;
            }
        };
        let display = Self {
            compositor,
            compositor_bus,
            compositor_bus_address,
            next_script: std::cell::Cell::new(0),
            runtime_dir,
            state_dir,
            socket_name,
        };
        let socket = display.runtime_dir.join(&display.socket_name);
        let deadline = Instant::now() + DISPLAY_START_WAIT;
        while !socket.exists() {
            assert!(
                Instant::now() < deadline,
                "kwin_wayland did not open {socket:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Some(display)
    }

    pub(super) fn socket_name(&self) -> &str {
        &self.socket_name
    }

    pub(super) fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    /// Человек закрыл окно Fastiplayer: композитор шлёт окну `xdg_toplevel.close`.
    ///
    /// Настоящий путь kwin (`closeWindow()` скрипта KWin), а не тестовый шов в процессе.
    /// Окно ищется по классу (app id `io.github.Bogdan7c.Fastiplayer`); других клиентов на
    /// частном дисплее нет.
    pub(super) fn close_fastiplayer_window(&self) {
        let number = self.next_script.get();
        self.next_script.set(number + 1);
        let script = self.state_dir.join(format!("close-window-{number}.js"));
        std::fs::write(
            &script,
            r#"const windows = workspace.windowList();
for (let index = 0; index < windows.length; index++) {
    if (String(windows[index].resourceClass).toLowerCase().indexOf("fastiplayer") >= 0) {
        windows[index].closeWindow();
    }
}
"#,
        )
        .expect("KWin script is written");
        let script_path = script.to_str().expect("UTF-8 script path");
        let plugin = format!("fastiplayer-test-close-{number}");
        self.kwin_scripting(&["loadScript", "ss", script_path, &plugin]);
        // `start` запускает загруженные, но ещё не запущенные скрипты.
        self.kwin_scripting(&["start"]);
    }

    fn kwin_scripting(&self, call: &[&str]) {
        let output = Command::new("busctl")
            .arg(format!("--address={}", self.compositor_bus_address))
            .args([
                "call",
                "org.kde.KWin",
                "/Scripting",
                "org.kde.kwin.Scripting",
            ])
            .args(call)
            .output()
            .expect("busctl must be installed for the window close test");
        assert!(
            output.status.success(),
            "KWin scripting call {call:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Частная шина D-Bus композитора (без каталогов служб): kwin регистрирует на ней
/// `org.kde.KWin`, и только тест может к ней обратиться.
fn start_compositor_bus(runtime_dir: &Path, state_dir: &Path) -> (Child, String) {
    let address = format!("unix:path={}", runtime_dir.join("kwin-bus").display());
    let config = state_dir.join("kwin-bus.conf");
    std::fs::write(
        &config,
        format!(
            r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>{address}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#
        ),
    )
    .expect("compositor bus config");
    let mut daemon = Command::new("dbus-daemon")
        .arg(format!("--config-file={}", config.display()))
        .args(["--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("dbus-daemon must be installed for this test");
    let stdout = daemon.stdout.take().expect("address pipe");
    let printed = std::io::BufRead::lines(std::io::BufReader::new(stdout))
        .next()
        .expect("dbus-daemon prints its address")
        .expect("readable address");
    assert!(printed.starts_with("unix:"), "{printed}");
    (daemon, printed)
}

impl Drop for VirtualWaylandDisplay {
    fn drop(&mut self) {
        let _ = self.compositor.kill();
        let _ = self.compositor.wait();
        let _ = self.compositor_bus.kill();
        let _ = self.compositor_bus.wait();
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
        let _ = std::fs::remove_dir_all(&self.state_dir);
    }
}

/// Каталог сеанса Wayland обязан быть доступен только владельцу.
fn restrict_to_owner(directory: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
        .expect("runtime dir permissions");
}

#[test]
fn the_window_renders_every_frame_of_the_recorded_stream() {
    play_the_recorded_stream_in_a_window("frames", None);
}

/// Программный декодер (`[video] preferred_backend = "software"`): тот же поток рисует
/// окно через загрузку кадров в рендер, а не через VA-API.
#[test]
fn the_window_renders_every_frame_with_the_software_decoder() {
    let mut config = fastiplayer_config::AppConfig::default();
    config.video.preferred_backend = fastiplayer_config::VideoBackendPreference::Software;
    let config_toml = config.to_pretty_toml().expect("config text");
    play_the_recorded_stream_in_a_window("software", Some(&config_toml));
}

/// Будит процесс с окном, играет записанный поток и сверяет журнал.
fn play_the_recorded_stream_in_a_window(name: &str, config_toml: Option<&str>) {
    let Some(display) = VirtualWaylandDisplay::start(name) else {
        return;
    };
    // Дисплей живёт дольше шины и процесса (объявлен раньше — уничтожается позже).
    let video = TestVideo::Window(display);
    let bus =
        PrivateBusWithBrowserSession::start_with(&format!("window-{name}"), config_toml, &video);
    let mut socket = open_attached_session(&bus);
    // Окно появляется на первом сеансе: звонок двери уже получил ответ, пока оно
    // поднималось.
    let ready = bus.wait_for_log_line("Browser window ready");
    eprintln!("{ready}");

    let (steps, expected_bytes) = recorded_stream("two-source-buffers-mid-box-cuts");
    for step in steps {
        let message = match step {
            StreamStep::Text(text) => Message::text(text),
            StreamStep::Binary(bytes) => Message::binary(bytes),
            StreamStep::AwaitClock(_) => panic!("this recording has no pauses"),
        };
        socket.send(message).expect("stream step is sent");
    }
    let messages = read_player_messages_until(&mut socket, "the clock state ended", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Ended,
                ..
            }
        )
    });
    let clock_positions: Vec<u64> = messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Clock { position_us, .. } => Some(position_us.as_micros()),
            _ => None,
        })
        .collect();
    assert!(
        clock_positions.last().is_some_and(|last| *last >= 360_000),
        "the clock never reached the last frame: {clock_positions:?}"
    );
    bus.wait_for_log_line("Browser playback ended");

    send_tab_gone_and_expect_close(&mut socket);
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    eprintln!("{summary}");
    assert_eq!(
        log_number(&summary, "presented_frames"),
        FIXTURE_VIDEO_FRAMES,
        "{summary}"
    );
    assert_eq!(log_number(&summary, "position_regressions"), 0, "{summary}");
    assert!(log_number(&summary, "audio_played_ms") > 0, "{summary}");
    let totals = bus.wait_for_log_line("Browser session stopped");
    assert!(
        totals.contains(&format!("received_bytes={expected_bytes}")),
        "expected {expected_bytes} bytes in: {totals}"
    );
    let finished = bus.wait_for_log_line("Browser session process finished");
    eprintln!("{finished}");
}
