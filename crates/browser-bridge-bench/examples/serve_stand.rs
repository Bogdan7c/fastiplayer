//! Помощник разработчика: поднимает сайт стенда и печатает ссылки на все страницы.
//!
//! Использование: `cargo run -p browser-bridge-bench --example serve_stand [префикс-прогона]`.
//! Страницы нужно открывать с параметром `run=<префикс>-<сценарий>` (ссылки печатаются готовые);
//! отчёты и журнал печатаются по мере прихода. Остановка — Ctrl+C.
//!
//! Пробел API: `StandSite` не умеет отдавать «все отчёты подряд», поэтому опрашиваем
//! заранее известные `run`-ы (`<префикс>-<сценарий>`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use browser_bridge_bench::media::ensure_media;
use browser_bridge_bench::site::{RunId, StandSite, StandSiteConfig};

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
/// Короткое ожидание одного отчёта за опрос.
const POLL_WAIT: Duration = Duration::from_millis(5);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Каталог crate-а можно подменить (`BENCH_CRATE_ROOT`) — удобно, если запускаем из копии.
    let crate_root = std::env::var_os("BENCH_CRATE_ROOT")
        .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from);
    let workspace_root = crate_root.join("../..");
    let prefix = std::env::args().nth(1).unwrap_or_else(|| "dev".to_owned());
    let media = ensure_media(&workspace_root.join("target/browser-bridge-bench/media"))?;
    let site = StandSite::start(StandSiteConfig {
        site_root: crate_root.join("site"),
        media,
    })?;
    let (main_origin, other_origin) = (site.main_origin(), site.other_origin());
    println!("main origin:  {main_origin}\nother origin: {other_origin}");
    println!("index: {main_origin}/");
    print_page_urls(&main_origin, &other_origin, &prefix);
    poll_forever(&site, &prefix)
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
    loop {
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
