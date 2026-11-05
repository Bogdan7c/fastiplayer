//! Живой стенд одной командой: настоящий экстеншен, посредник и плеер на живом сайте.
//!
//! Браузер запускается в частном окружении со свежим профилем: автоматика стенда не трогает
//! профиль и шину владельца. Страница открывается, робот раз в секунду зовёт передачу вкладки, пока
//! она не состоится (там, где сайт ждёт клика по play, это успевает сделать человек). Дальше помощник
//! печатает строки журнала плеера про рекламу и концы сеанса. С `--press auto` он сам жмёт `Next`
//! (MPRIS), как только плашка предложит кнопку.
//!
//! ```text
//! cargo run --release -p browser-bridge-bench --example live_handoff -- \
//!   --browser firefox --display virtual --url <адрес> --seconds 90 --press auto
//! ```
//!
//! Экстеншен должен быть собран (`npm run build` в `extensions/browser`), плеер берётся из
//! `target/release`. Адреса и журнал остаются только в выводе: в отчёты их не копировать.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use browser_bridge_bench::browsers::{
    BrowserKind, ExtensionSource, HandOffOutcome, LaunchOptions, launch,
};
use browser_bridge_bench::environment::{
    BenchEnvironment, DisplayTarget, EnvironmentOptions, PlayerConfigProfile,
};
use browser_bridge_bench::player_control::PlayerControl;

/// Строки журнала плеера, которые интересны человеку.
const INTERESTING_LINES: &[&str] = &[
    "Ad plate",
    "Ad skip",
    "refused: an ad",
    "Browser session end notice",
    "stream_rejected",
    "ERROR",
];
/// Подстрока журнала: плашка предлагает нашу кнопку «Skip» (подсказку `on_page` робот не жмёт).
const SKIP_OFFER: &str = "skip=skip";
/// Как часто просим передачу, пока видео нет.
const HAND_OFF_RETRY: Duration = Duration::from_secs(1);
/// Сколько даём странице прогрева.
const WARM_UP: Duration = Duration::from_secs(5);
/// Шаг чтения журнала.
const LOG_POLL: Duration = Duration::from_millis(200);

/// Кто жмёт кнопку плашки.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Press {
    /// Никто: смотрит или жмёт человек.
    Nobody,
    /// Робот, сразу как только кнопка появилась.
    Robot,
}

struct Arguments {
    browser: BrowserKind,
    display: DisplayTarget,
    url: String,
    /// Что открыть перед видео, чтобы сайт завёл свежему профилю куки.
    warm_up_url: Option<String>,
    seconds: u64,
    /// Сколько ждать между открытием страницы и первой передачей.
    wait_seconds: u64,
    press: Press,
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut browser = BrowserKind::Firefox;
    let mut display = DisplayTarget::Virtual;
    let mut url = None;
    let mut warm_up_url = None;
    let mut seconds = 60;
    let mut wait_seconds = 0;
    let mut press = Press::Nobody;
    let mut arguments = std::env::args().skip(1);
    while let Some(name) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("{name} needs a value"))?;
        match (name.as_str(), value.as_str()) {
            ("--browser", "firefox") => browser = BrowserKind::Firefox,
            ("--browser", "chromium") => browser = BrowserKind::Chromium,
            ("--display", "virtual") => display = DisplayTarget::Virtual,
            ("--display", "real") => display = DisplayTarget::Real,
            ("--press", "auto") => press = Press::Robot,
            ("--press", "none") => press = Press::Nobody,
            ("--url", _) => url = Some(value),
            ("--warm-up-url", _) => warm_up_url = Some(value),
            ("--seconds", _) => {
                seconds = value
                    .parse()
                    .map_err(|error| format!("--seconds: {error}"))?;
            }
            ("--wait", _) => {
                wait_seconds = value.parse().map_err(|error| format!("--wait: {error}"))?;
            }
            _ => return Err(format!("unknown argument {name} {value}")),
        }
    }
    Ok(Arguments {
        browser,
        display,
        url: url.ok_or("--url is required")?,
        warm_up_url,
        seconds,
        wait_seconds,
        press,
    })
}

/// Новые строки журнала после `offset`; неполная последняя строка ждёт следующего чтения.
fn read_new_lines(path: &Path, offset: &mut usize) -> Vec<String> {
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    let Some(fresh) = bytes.get(*offset..) else {
        return Vec::new();
    };
    let Some(end) = fresh.iter().rposition(|byte| *byte == b'\n') else {
        return Vec::new();
    };
    let lines = String::from_utf8_lossy(&fresh[..end])
        .lines()
        .map(str::to_owned)
        .collect();
    *offset += end + 1;
    lines
}

fn run(arguments: &Arguments) -> Result<(), String> {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let environment = BenchEnvironment::create(EnvironmentOptions {
        display: arguments.display,
        player_bin_dir: repository.join("target/release"),
        // На настоящем экране — настройки владельца, в невидимом kwin — чистые.
        player_config: if arguments.display == DisplayTarget::Real {
            PlayerConfigProfile::Gles2Vaapi
        } else {
            PlayerConfigProfile::Clean
        },
        player_rust_log: "info".to_owned(),
        work_root: std::env::temp_dir(),
    })
    .map_err(|error| error.to_string())?;
    environment
        .connect_browser(arguments.browser)
        .map_err(|error| error.to_string())?;
    let flavour = match arguments.browser {
        BrowserKind::Chromium => "chrome",
        BrowserKind::Firefox => "firefox",
    };
    let mut browser = launch(
        arguments.browser,
        &environment,
        LaunchOptions {
            extensions: vec![ExtensionSource::UnpackedDir(
                // `FASTIPLAYER_LIVE_EXTENSION_DIST`: другая сборка экстеншена, для сравнения.
                std::env::var_os("FASTIPLAYER_LIVE_EXTENSION_DIST")
                    .map_or_else(|| repository.join("extensions/browser/dist"), PathBuf::from)
                    .join(flavour),
            )],
            autoplay_without_gesture: true,
            loopback_host_aliases: Vec::new(),
        },
    )
    .map_err(|error| error.to_string())?;
    // Свежий профиль на первом же адресе рекламы почти не видит: сайту нужен предыдущий заход.
    if let Some(warm_up_url) = &arguments.warm_up_url {
        browser
            .open_tab(warm_up_url)
            .map_err(|error| error.to_string())?;
        sleep(WARM_UP);
    }
    let tab = browser
        .open_tab(&arguments.url)
        .map_err(|error| error.to_string())?;
    // Пауза до передачи: реклама в начале ролика должна успеть начаться.
    sleep(Duration::from_secs(arguments.wait_seconds));

    let deadline = Instant::now() + Duration::from_secs(arguments.seconds);
    let mut started = false;
    while !started && Instant::now() < deadline {
        match browser.hand_off_tab(&tab, &arguments.url) {
            Ok(HandOffOutcome::Started) => started = true,
            Ok(other) => println!("hand-off: {other:?}"),
            Err(error) => println!("hand-off failed: {error}"),
        }
        if !started {
            sleep(HAND_OFF_RETRY);
        }
    }
    println!("hand-off started: {started}");

    let control = PlayerControl::on_bus(
        environment
            .bus_address()
            .map_err(|error| error.to_string())?,
    );
    let verbose = std::env::var_os("FASTIPLAYER_LIVE_ALL_LINES").is_some();
    let mut offset = 0;
    let mut offer_waiting = false;
    while started && Instant::now() < deadline {
        for line in read_new_lines(environment.player_log(), &mut offset) {
            // `FASTIPLAYER_LIVE_ALL_LINES=1`: весь журнал плеера, для разбора.
            if verbose || INTERESTING_LINES.iter().any(|needle| line.contains(needle)) {
                println!("{line}");
            }
            if line.contains("Ad plate") {
                offer_waiting = line.contains(SKIP_OFFER);
            }
        }
        if offer_waiting && arguments.press == Press::Robot {
            match control.next() {
                Ok(()) => println!("robot pressed Next"),
                Err(error) => println!("Next failed: {error}"),
            }
            offer_waiting = false;
        }
        sleep(LOG_POLL);
    }
    browser.shutdown().map_err(|error| error.to_string())?;
    Ok(())
}

fn main() {
    let outcome = parse_arguments().and_then(|arguments| run(&arguments));
    if let Err(message) = outcome {
        eprintln!("live_handoff: {message}");
        std::process::exit(1);
    }
}
