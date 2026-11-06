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
//! С `--hand-off person` робот ничего не передаёт: браузер просто открыт, значок нажимает человек
//! (или настоящая мышь снаружи), а помощник печатает журнал плеера. С `--stand-page <страница>`
//! вместо живого сайта открывается страница стенда, сервер стенда живёт весь прогон:
//!
//! ```text
//! cargo run --release -p browser-bridge-bench --example live_handoff -- \
//!   --display real --hand-off person --stand-page banner-with-main --seconds 300
//! ```
//!
//! `--stand-page` можно повторить: каждая страница откроется своей вкладкой, активной останется
//! последняя. `--autoplay browser-default` запускает браузер с его обычными настройками автозапуска
//! (по умолчанию автозапуск разрешён, как на стенде).
//!
//! Экстеншен должен быть собран (`npm run build` в `extensions/browser`), плеер берётся из
//! `target/release`. Адреса и журнал остаются только в выводе: в отчёты их не копировать.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use browser_bridge_bench::browsers::{
    BrowserKind, BrowserUnderTest, ExtensionSource, HandOffOutcome, LaunchOptions, TabHandle,
    launch,
};
use browser_bridge_bench::environment::{
    BenchEnvironment, DesktopSetup, DisplayTarget, EnvironmentOptions, PlayerConfigProfile,
};
use browser_bridge_bench::media::ensure_media;
use browser_bridge_bench::player_control::PlayerControl;
use browser_bridge_bench::site::{StandSite, StandSiteConfig};

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

/// Параметр адреса: страница стенда ждёт команд и не завершается сама.
const STAND_HANDOFF_QUERY: &str = "&mode=handoff";

/// Что открыть в браузере.
enum Target {
    /// Живой сайт.
    Url(String),
    /// Страницы стенда по их именам в `site/pages`, каждая в своей вкладке.
    StandPages(Vec<String>),
}

/// Кто отдаёт видео в плеер.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HandOff {
    /// Робот раз в секунду зовёт команду передачи.
    Robot,
    /// Человек жмёт значок экстеншена, робот только показывает журнал.
    Person,
}

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
    target: Target,
    hand_off: HandOff,
    /// Что открыть перед видео, чтобы сайт завёл свежему профилю куки.
    warm_up_url: Option<String>,
    seconds: u64,
    /// Сколько ждать между открытием страницы и первой передачей.
    wait_seconds: u64,
    press: Press,
    /// Разрешить автозапуск без жеста, как на стенде (`false` — обычные настройки браузера).
    autoplay_without_gesture: bool,
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut browser = BrowserKind::Firefox;
    let mut display = DisplayTarget::Virtual;
    let mut target = None;
    let mut stand_pages = Vec::new();
    let mut autoplay_without_gesture = true;
    let mut hand_off = HandOff::Robot;
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
            ("--hand-off", "robot") => hand_off = HandOff::Robot,
            ("--hand-off", "person") => hand_off = HandOff::Person,
            ("--url", _) => target = Some(Target::Url(value)),
            ("--stand-page", _) => stand_pages.push(value),
            ("--autoplay", "allowed") => autoplay_without_gesture = true,
            ("--autoplay", "browser-default") => autoplay_without_gesture = false,
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
    if !stand_pages.is_empty() {
        target = Some(Target::StandPages(stand_pages));
    }
    Ok(Arguments {
        browser,
        display,
        target: target.ok_or("--url or --stand-page is required")?,
        hand_off,
        warm_up_url,
        seconds,
        wait_seconds,
        press,
        autoplay_without_gesture,
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

/// Поднимает сервер стенда: он должен жить, пока открыта страница.
fn start_stand_site(repository: &Path) -> Result<StandSite, String> {
    let media = ensure_media(&repository.join("target/browser-bridge-bench/media"))
        .map_err(|error| error.to_string())?;
    StandSite::start(StandSiteConfig {
        site_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("site"),
        media,
    })
    .map_err(|error| error.to_string())
}

/// Адрес страницы стенда в режиме передачи.
fn stand_page_url(site: &StandSite, page: &str) -> String {
    let main_origin = site.main_origin();
    let other_origin = site.other_origin();
    format!(
        "{main_origin}/site/pages/{page}.html?run=live-{page}&report_origin={main_origin}&other_origin={other_origin}{STAND_HANDOFF_QUERY}"
    )
}

/// Зовёт передачу, пока она не состоится или не выйдет время. Возвращает, состоялась ли она.
fn hand_off_by_robot(
    browser: &mut dyn BrowserUnderTest,
    tab: &TabHandle,
    url: &str,
    deadline: Instant,
) -> bool {
    while Instant::now() < deadline {
        match browser.hand_off_tab(tab, url) {
            Ok(HandOffOutcome::Started) => return true,
            Ok(other) => println!("hand-off: {other:?}"),
            Err(error) => println!("hand-off failed: {error}"),
        }
        sleep(HAND_OFF_RETRY);
    }
    false
}

fn run(arguments: &Arguments) -> Result<(), String> {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    // Сервер стенда держим до конца функции, иначе страница потеряет файлы.
    let stand_site = match &arguments.target {
        Target::StandPages(_) => Some(start_stand_site(&repository)?),
        Target::Url(_) => None,
    };
    let urls: Vec<String> = match (&arguments.target, &stand_site) {
        (Target::Url(url), _) => vec![url.clone()],
        (Target::StandPages(pages), Some(site)) => pages
            .iter()
            .map(|page| stand_page_url(site, page))
            .collect(),
        (Target::StandPages(_), None) => return Err("the stand site did not start".to_owned()),
    };
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
        desktop: DesktopSetup::default(),
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
            autoplay_without_gesture: arguments.autoplay_without_gesture,
            loopback_host_aliases: Vec::new(),
            runtime_extension_install: false,
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
    // Передачу робот зовёт на последней вкладке: она и остаётся активной.
    let mut opened = None;
    for url in &urls {
        let tab = browser.open_tab(url).map_err(|error| error.to_string())?;
        opened = Some((tab, url));
    }
    let (tab, url) = opened.ok_or("nothing to open")?;
    // Пауза до передачи: реклама в начале ролика должна успеть начаться.
    sleep(Duration::from_secs(arguments.wait_seconds));

    let deadline = Instant::now() + Duration::from_secs(arguments.seconds);
    let started = match arguments.hand_off {
        HandOff::Robot => hand_off_by_robot(browser.as_mut(), &tab, url, deadline),
        HandOff::Person => true,
    };
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
