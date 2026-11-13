//! Живой прогон `live-ends` (запускается руками, не в CI).
//!
//! `cargo test -p browser-bridge-bench --test live_live_ends -- --ignored --nocapture`
//!
//! Нужны те же сборки и инструменты, что у `live_smoke`. Критерий — тот же, что у команды
//! `live-ends`: эфир (MSE и родной HLS) посреди игры становится записью, плеер доигрывает её до
//! конца, страница видит конечную длину и `ended`. Для Firefox строка `hls-live-ends` честно
//! `unsupported`.

use std::path::PathBuf;

use browser_bridge_bench::browsers::BrowserKind;
use browser_bridge_bench::environment::{DisplayTarget, PlayerConfigProfile};
use browser_bridge_bench::scenarios::{
    BrowserExecutables, Outcome, RunSettings, Stand, run_live_ends,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn live_ends_in(kind: BrowserKind) -> Result<(), Box<dyn std::error::Error>> {
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
    let rows = run_live_ends(&stand, kind);
    assert_eq!(rows.len(), 2, "the command has two rows per browser");
    for row in &rows {
        println!(
            "[{kind:?}] {} {:?} in {:?}: {}",
            row.subject, row.outcome, row.elapsed, row.detail
        );
        // Единственное допустимое «не умею»: родной HLS в Firefox.
        let allowed = row.outcome == Outcome::Pass
            || (row.outcome == Outcome::Unsupported
                && kind == BrowserKind::Firefox
                && row.subject == "hls-live-ends");
        assert!(allowed, "{} {:?}: {}", row.subject, row.outcome, row.detail);
    }
    Ok(())
}

#[test]
#[ignore = "live: starts kwin, Chromium and the real player"]
fn live_ends_chromium() -> Result<(), Box<dyn std::error::Error>> {
    live_ends_in(BrowserKind::Chromium)
}

#[test]
#[ignore = "live: starts kwin, Firefox and the real player"]
fn live_ends_firefox() -> Result<(), Box<dyn std::error::Error>> {
    live_ends_in(BrowserKind::Firefox)
}
