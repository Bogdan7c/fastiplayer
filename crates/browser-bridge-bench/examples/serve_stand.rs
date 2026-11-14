//! Помощник разработчика: поднимает сайт стенда и печатает ссылки на все страницы.
//!
//! Использование: `cargo run -p browser-bridge-bench --example serve_stand [префикс-прогона] [--bind <ip:порт>]`.
//!
//! `--bind` (16d4) — главный origin слушает этот адрес локальной сети, чтобы страницы открывал браузер
//! на ДРУГОЙ машине (стенд не спит, пока спит ноутбук с браузером). Без флага — только петля, как у
//! автоматики. «Чужой» origin остаётся на петле: `iframe-cross-origin` и `cdn-guard` так не проверить.
//! Страницы нужно открывать с параметром `run=<префикс>-<сценарий>` (ссылки печатаются готовые);
//! отчёты и журнал печатаются по мере прихода. Остановка — Ctrl+C.
//!
//! Пробел API: `StandSite` не умеет отдавать «все отчёты подряд», поэтому опрашиваем
//! заранее известные `run`-ы (`<префикс>-<сценарий>`).

use std::collections::HashSet;
use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use browser_bridge_bench::media::ensure_media;
use browser_bridge_bench::site::{
    MainListenAddress, MediaRequest, RunId, StandSite, StandSiteConfig,
};

/// Сценарии страниц стенда (имена файлов в `site/pages`).
const SCENARIOS: &[&str] = &[
    "mse-fmp4",
    "mse-webm",
    "mse-av1",
    "mse-muxed",
    "mse-worker",
    "mse-abr",
    "mse-stall-reload",
    "mse-generation-swap",
    "video-src",
    "native-hls",
    "m3u8-in-src",
    "hls-fmp4-abr",
    "hls-aes128",
    "hls-vod-long",
    "hls-live",
    "hls-live-ends",
    "hls-live-sleep",
    "shadow-dom",
    "iframe-cross-origin",
    "cdn-guard",
    "ad-preroll",
    "banner-autoplay",
    "drm-probe",
    "drm-clearkey",
    "series",
    "spa-navigation",
    "pip",
];
/// Период опроса отчётов.
const POLL_PERIOD: Duration = Duration::from_millis(300);
/// Запросы к этим маршрутам печатаются по мере прихода (16d4: журнал «сервера эфира» для настоящего
/// сна, где браузер на другой машине и журнала стенда у сценария нет).
const PRINTED_REQUEST_PREFIX: &str = "/cdn/hls-live";
/// Короткое ожидание одного отчёта за опрос.
const POLL_WAIT: Duration = Duration::from_millis(5);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Каталог crate-а можно подменить (`BENCH_CRATE_ROOT`) — удобно, если запускаем из копии.
    let crate_root = std::env::var_os("BENCH_CRATE_ROOT")
        .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from);
    let workspace_root = crate_root.join("../..");
    let arguments = Arguments::parse(std::env::args().skip(1))?;
    let prefix = arguments.prefix;
    let media = ensure_media(&workspace_root.join("target/browser-bridge-bench/media"))?;
    let site = StandSite::start_listening(
        StandSiteConfig {
            site_root: crate_root.join("site"),
            media,
        },
        arguments.main_address,
    )?;
    let (main_origin, other_origin) = (site.main_origin(), site.other_origin());
    println!("main origin:  {main_origin}\nother origin: {other_origin}");
    println!("index: {main_origin}/");
    print_page_urls(&main_origin, &other_origin, &prefix);
    poll_forever(&site, &prefix)
}

/// Разобранные аргументы помощника.
struct Arguments {
    /// Префикс прогонов страниц (`run=<префикс>-<сценарий>`).
    prefix: String,
    /// Где слушает главный origin.
    main_address: MainListenAddress,
}

impl Arguments {
    /// `[префикс] [--bind <ip:порт>]` в любом порядке; неизвестный флаг — ошибка, а не молчание.
    fn parse(words: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut prefix: Option<String> = None;
        let mut main_address = MainListenAddress::Loopback;
        let mut words = words;
        while let Some(word) = words.next() {
            if word == "--bind" {
                let value = words
                    .next()
                    .ok_or_else(|| "--bind needs <ip:port>".to_owned())?;
                let address: SocketAddrV4 = value
                    .parse()
                    .map_err(|error| format!("--bind {value}: {error} (expected ip:port)"))?;
                main_address = MainListenAddress::Exact(address);
            } else if word.starts_with("--") {
                return Err(format!("unknown option {word}"));
            } else if prefix.replace(word).is_some() {
                return Err("only one run prefix is accepted".to_owned());
            }
        }
        Ok(Self {
            prefix: prefix.unwrap_or_else(|| "dev".to_owned()),
            main_address,
        })
    }
}

fn print_page_urls(main_origin: &str, other_origin: &str, prefix: &str) {
    for scenario in SCENARIOS {
        let mut url = format!(
            "{main_origin}/site/pages/{scenario}.html?run={prefix}-{scenario}&report_origin={main_origin}"
        );
        if *scenario == "iframe-cross-origin" {
            url.push_str(&format!("&other_origin={other_origin}"));
        }
        println!("{url}");
    }
}

fn poll_forever(site: &StandSite, prefix: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut reported: HashSet<&str> = HashSet::new();
    let mut printed_until: Option<Instant> = None;
    loop {
        printed_until = print_new_live_requests(site, printed_until);
        for scenario in SCENARIOS {
            let run = RunId::new(&format!("{prefix}-{scenario}"))?;
            for line in site.take_logs(&run) {
                println!("[log {scenario}] {:?}: {}", line.source, line.line);
            }
            if reported.contains(scenario) {
                continue;
            }
            if let Ok(report) = site.wait_report(&run, POLL_WAIT) {
                reported.insert(scenario);
                print_report(scenario, &report);
            }
        }
        sleep(POLL_PERIOD);
    }
}

/// Печатает запросы к живым маршрутам, пришедшие позже `printed_until`; возвращает новую отметку.
///
/// Время — настенное (мс Unix), пересчитанное из монотонного времени прихода: так строки стенда
/// сверяются с журналом плеера и ядра на другой машине (часы машин сверяются отдельно).
fn print_new_live_requests(site: &StandSite, printed_until: Option<Instant>) -> Option<Instant> {
    let now_instant = Instant::now();
    let now_wall_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    let fresh: Vec<MediaRequest> = site
        .media_requests()
        .into_iter()
        .filter(|request| request.path.starts_with(PRINTED_REQUEST_PREFIX))
        .filter(|request| printed_until.is_none_or(|until| request.at > until))
        .collect();
    for request in &fresh {
        let age_ms = now_instant
            .saturating_duration_since(request.at)
            .as_millis();
        println!(
            "REQUEST wall_ms={} status={} by={:?} {}",
            now_wall_ms.saturating_sub(age_ms),
            request.status,
            request.requester(),
            request.path
        );
    }
    fresh
        .iter()
        .map(|request| request.at)
        .max()
        .or(printed_until)
}

fn print_report(scenario: &str, report: &browser_bridge_bench::site::PageReport) {
    println!(
        "REPORT {scenario} status={:?} elapsed_ms={} ua={}",
        report.status, report.elapsed_ms, report.user_agent
    );
    for check in &report.checks {
        println!(
            "    [{}] {}: {}",
            if check.ok { "ok" } else { "FAIL" },
            check.name,
            check.detail
        );
    }
}
