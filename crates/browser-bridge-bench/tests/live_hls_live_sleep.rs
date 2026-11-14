//! Живой прогон `hls-live-sleep` (запускается руками, не в CI).
//!
//! `cargo test -p browser-bridge-bench --test live_hls_live_sleep -- --ignored --nocapture`
//!
//! Нужны те же сборки и инструменты, что у `live_smoke`. Критерий — тот же, что у команды
//! `hls-live-sleep`: сервер эфира 20 с отвечает на плейлист 503, окно уезжает; фон экстеншена
//! повторяет, плеер дожидается сети, склеивает новое окно и играет дальше, поколение не отвергнуто.
//! Для Firefox строка честно `unsupported` (родного HLS нет).

use std::path::PathBuf;

use browser_bridge_bench::browsers::BrowserKind;
use browser_bridge_bench::environment::{DisplayTarget, PlayerConfigProfile};
use browser_bridge_bench::scenarios::{
    BrowserExecutables, Outcome, RunSettings, Stand, run_hls_live_sleep,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn hls_live_sleep_in(kind: BrowserKind) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repo_root();
    let stand = Stand::start(RunSettings {
        repo_root: repo.clone(),
        work_root: repo.join("target/browser-bridge-bench/work"),
        media_cache: repo.join("target/browser-bridge-bench/media"),
        player_bin_dir: repo.join("target/debug"),
        display: DisplayTarget::Virtual,
        player_config: PlayerConfigProfile::Clean,
        extra_extensions: Vec::new(),
        keep_work: false,
        browser_executables: BrowserExecutables::default(),
    })?;
    let row = run_hls_live_sleep(&stand, kind);
    println!(
        "[{kind:?}] {} {:?} in {:?}: {}",
        row.subject, row.outcome, row.elapsed, row.detail
    );
    // Единственное допустимое «не умею»: родной HLS в Firefox.
    let expected = match kind {
        BrowserKind::Chromium => Outcome::Pass,
        BrowserKind::Firefox => Outcome::Unsupported,
    };
    assert_eq!(row.outcome, expected, "{}", row.detail);
    Ok(())
}

#[test]
#[ignore = "live: starts kwin, Chromium and the real player"]
fn hls_live_sleep_chromium() -> Result<(), Box<dyn std::error::Error>> {
    hls_live_sleep_in(BrowserKind::Chromium)
}

#[test]
#[ignore = "live: starts kwin, Firefox and the real player"]
fn hls_live_sleep_firefox() -> Result<(), Box<dyn std::error::Error>> {
    hls_live_sleep_in(BrowserKind::Firefox)
}
