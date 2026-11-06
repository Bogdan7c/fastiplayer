//! Живая проверка настоящей мыши и клавиатуры на невидимом экране (запускается руками).
//!
//! `cargo test -p browser-bridge-bench --test live_mouse -- --ignored --nocapture --test-threads=1`
//!
//! Нужны сборки `cargo build -p app-egui -p browser-bridge-host`, собранный экстеншен
//! (`extensions/browser/dist`), Chromium, Firefox, `kwin_wayland`, `spectacle`, `ffmpeg`.
//! Критерий: НАСТОЯЩИЙ клик по найденному на снимке значку открывает попап экстеншена, и это
//! подтверждено независимо от снимка (цель DevTools у Chromium, chrome-контекст у Firefox).
//! Снимки копируются в каталог из `BRIDGE_BENCH_SHOTS_DIR`, если переменная задана.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use browser_bridge_bench::browsers::{
    BrowserKind, BrowserUnderTest, ExtensionSource, LaunchOptions, launch,
};
use browser_bridge_bench::compositor_windows::CompositorWindows;
use browser_bridge_bench::desktop_input::{DesktopInput, Key, Point};
use browser_bridge_bench::environment::{
    BenchEnvironment, DesktopSetup, DisplayTarget, EnvironmentOptions, PlayerConfigProfile,
};
use browser_bridge_bench::screen_capture::{ScreenCapture, ScreenError};

/// Сколько ждать значок на панели после запуска (экстеншен грузится не мгновенно).
const ICON_WAIT: Duration = Duration::from_secs(40);
/// Сколько ждать попап после клика.
const POPUP_WAIT: Duration = Duration::from_secs(8);

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn dist_name(kind: BrowserKind) -> &'static str {
    match kind {
        BrowserKind::Chromium => "chrome",
        BrowserKind::Firefox => "firefox",
    }
}

/// Копирует снимок туда, где его можно посмотреть после теста.
fn keep_shot(shot: &Path, name: &str) -> TestResult {
    if let Some(directory) = std::env::var_os("BRIDGE_BENCH_SHOTS_DIR") {
        let target = PathBuf::from(directory).join(format!("{name}.png"));
        std::fs::create_dir_all(directory_of(&target))?;
        std::fs::copy(shot, &target)?;
        println!("screenshot: {}", target.display());
    }
    Ok(())
}

fn directory_of(file: &Path) -> &Path {
    file.parent().unwrap_or(Path::new("."))
}

/// Страница, которая пишет в заголовок всё, что до неё дошло: клик мыши и нажатые клавиши.
const KEY_RECORDER_PAGE: &str = "data:text/html,<title>keys:</title><body style='height:100vh'><button>focus target</button><script>\
document.addEventListener('mousedown',()=>{document.title+='click,'});\
document.addEventListener('keydown',e=>{document.title+=e.key+','});</script>";

/// Ждёт, пока условие даст `Some`; иначе падает с описанием.
fn wait_for<T>(what: &str, timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Окно плеера на невидимом экране: оно забирает фокус у браузера, как в настоящем сеансе.
struct PlayerWindow(std::process::Child);

impl PlayerWindow {
    fn open(
        environment: &BenchEnvironment,
        repo: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let endpoint = environment.desktop_endpoint()?;
        let child = std::process::Command::new(repo.join("target/debug/fastiplayer"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", environment.home())
            .env("XDG_CONFIG_HOME", environment.xdg_config_home())
            .env("XDG_RUNTIME_DIR", endpoint.runtime_dir())
            .env("WAYLAND_DISPLAY", endpoint.wayland_display_name())
            .env("DBUS_SESSION_BUS_ADDRESS", environment.bus_address()?)
            .env("FASTIPLAYER_TEST_BROWSER_AUDIO", "simulated")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        Ok(Self(child))
    }
}

impl Drop for PlayerWindow {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn real_mouse_and_keyboard_drive_the_browser(kind: BrowserKind) -> TestResult {
    let repo = repo_root();
    let environment = BenchEnvironment::create(EnvironmentOptions {
        display: DisplayTarget::Virtual,
        player_bin_dir: repo.join("target/debug"),
        player_config: PlayerConfigProfile::Clean,
        player_rust_log: "info".to_owned(),
        work_root: repo.join("target/browser-bridge-bench/work"),
        desktop: DesktopSetup::pointer_stand(),
    })?;
    environment.connect_browser(kind)?;
    let mut browser: Box<dyn BrowserUnderTest> = launch(
        kind,
        &environment,
        LaunchOptions {
            extensions: vec![ExtensionSource::UnpackedDir(
                repo.join("extensions/browser/dist").join(dist_name(kind)),
            )],
            autoplay_without_gesture: true,
            loopback_host_aliases: Vec::new(),
        },
    )?;
    let input = DesktopInput::connect(&environment.desktop_endpoint()?)?;
    let screen = ScreenCapture::new(&environment, &ScreenCapture::icons_dir(&repo, kind))?;
    let windows = CompositorWindows::new(&environment)?;
    let name = kind.catalog_name();
    let browser_class = name; // resourceClass браузера содержит его имя

    // 1. Значок появляется, когда браузер дорисует панель и загрузит экстеншен.
    let deadline = Instant::now() + ICON_WAIT;
    let icon = loop {
        match screen.find_extension_icon() {
            Ok(found) => break found,
            Err(ScreenError::IconNotFound { .. }) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(error) => return Err(error.into()),
        }
    };
    println!("[{kind:?}] icon at {icon:?}");

    // 2. Настоящий клик по значку открывает попап; независимая улика — цель браузера.
    windows.bring_browser_forward(kind)?;
    input.mouse().click_at(icon.center())?;
    let popup_url = wait_for("the popup after the click", POPUP_WAIT, || {
        browser.open_popup_url().ok().flatten()
    });
    println!("[{kind:?}] popup target: {popup_url}");
    assert!(popup_url.ends_with("popup.html"), "{popup_url}");
    keep_shot(
        &screen.take_screenshot("popup-open")?,
        &format!("{name}-popup-open"),
    )?;

    // 3. Escape доходит до попапа и закрывает его: клавиша настоящая, попап реагирует.
    input.keyboard().press(Key::Escape)?;
    wait_for("the popup to close on Escape", POPUP_WAIT, || {
        browser.open_popup_url().ok().filter(Option::is_none)
    });

    // 4. Клик, Tab и Enter доходят до страницы: страница записала их в заголовок.
    let page = browser.open_tab(KEY_RECORDER_PAGE)?;
    windows.bring_browser_forward(kind)?;
    let center = Point::new(960, 700);
    input.mouse().click_at(center)?;
    input.keyboard().press(Key::Tab)?;
    input.keyboard().press(Key::Enter)?;
    let title = browser.tab_title(&page)?;
    println!("[{kind:?}] page title after the robot input: {title}");
    assert_eq!(title, "keys:click,Tab,Enter,");

    // 5. Окно плеера поверх браузера забирает фокус; робот возвращает браузер вперёд.
    let player = PlayerWindow::open(&environment, &repo)?;
    wait_for(
        "the player window to take focus",
        Duration::from_secs(30),
        || {
            windows
                .active_window_class()
                .ok()
                .filter(|class| class.contains("fastiplayer"))
        },
    );
    keep_shot(
        &screen.take_screenshot("player-on-top")?,
        &format!("{name}-player-on-top"),
    )?;
    windows.bring_browser_forward(kind)?;
    assert!(windows.active_window_class()?.contains(browser_class));
    let icon = screen.find_extension_icon()?;
    input.mouse().click_at(icon.center())?;
    wait_for("the popup after the player was on top", POPUP_WAIT, || {
        browser.open_popup_url().ok().flatten()
    });
    keep_shot(
        &screen.take_screenshot("popup-after-player")?,
        &format!("{name}-popup-after-player"),
    )?;
    drop(player);
    browser.shutdown()?;
    Ok(())
}

#[test]
#[ignore = "live: starts kwin and Chromium"]
fn chromium_real_input_drives_the_browser() -> TestResult {
    real_mouse_and_keyboard_drive_the_browser(BrowserKind::Chromium)
}

#[test]
#[ignore = "live: starts kwin and Firefox"]
fn firefox_real_input_drives_the_browser() -> TestResult {
    real_mouse_and_keyboard_drive_the_browser(BrowserKind::Firefox)
}
