//! Живой дымок стенда (запускается руками, не в CI).
//!
//! `cargo test -p browser-bridge-bench --test live_smoke -- --ignored --nocapture`
//!
//! Нужны сборки `cargo build -p app-egui -p browser-bridge-host`, Chromium/Firefox,
//! `kwin_wayland`, `ffmpeg`. Критерий — тот же, что у команды `smoke`: страница играет,
//! стендовый перехватчик через настоящего посредника будит настоящий плеер и тот показывает
//! не меньше 50 кадров; провал любого звена роняет тест с подробностями.

use std::path::PathBuf;

use browser_bridge_bench::browsers::BrowserKind;
use browser_bridge_bench::environment::{DisplayTarget, PlayerConfigProfile};
use browser_bridge_bench::scenarios::{BrowserExecutables, Outcome, RunSettings, Stand, run_smoke};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn smoke_in(kind: BrowserKind) -> Result<(), Box<dyn std::error::Error>> {
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
    let result = run_smoke(&stand, kind);
    println!(
        "[{kind:?}] {:?} in {:?}: {}",
        result.outcome, result.elapsed, result.detail
    );
    assert_eq!(result.outcome, Outcome::Pass, "{}", result.detail);
    Ok(())
}

#[test]
#[ignore = "live: starts kwin, Chromium and the real player"]
fn live_smoke_chromium() -> Result<(), Box<dyn std::error::Error>> {
    smoke_in(BrowserKind::Chromium)
}

#[test]
#[ignore = "live: starts kwin, Firefox and the real player"]
fn live_smoke_firefox() -> Result<(), Box<dyn std::error::Error>> {
    smoke_in(BrowserKind::Firefox)
}
