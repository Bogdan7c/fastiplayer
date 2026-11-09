//! Живая проверка окружения, установщика и браузеров стенда (запускается руками).
//!
//! `cargo test -p browser-bridge-bench --test live_extension_status -- --ignored --nocapture`
//!
//! Нужны сборки `cargo build -p app-egui -p browser-bridge-host`, экстеншен
//! `cd extensions/browser && npm run build`, Chromium, Firefox, `kwin_wayland`.
//! Критерий: НАСТОЯЩИЙ экстеншен 07 в браузере, запущенном в невидимом kwin с временным
//! HOME, через ПОСТАВЛЕННОГО настоящим установщиком посредника видит плеер, и попап
//! показывает «Connected to Fastiplayer». Ничего вне каталога прогона не пишется.

use std::path::PathBuf;
use std::time::Instant;

use browser_bridge_bench::browsers::{
    BrowserKind, ExtensionPage, ExtensionSource, LaunchOptions, launch,
};
use browser_bridge_bench::environment::{
    BenchEnvironment, DesktopSetup, DisplayTarget, EnvironmentOptions, PlayerConfigProfile,
    PlayerTestVariables,
};
use browser_bridge_bench::observe::read_host_evidence;

/// Корень репозитория (две ступени вверх от crate-а).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Ошибки стенда пробрасываем `?`: тест падает с понятным текстом ошибки.
fn popup_text_of_real_extension(
    kind: BrowserKind,
    dist_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let repo = repo_root();
    let environment = BenchEnvironment::create(EnvironmentOptions {
        display: DisplayTarget::Virtual,
        player_bin_dir: repo.join("target/debug"),
        player_config: PlayerConfigProfile::Gles2Vaapi,
        player_rust_log: "info".to_owned(),
        work_root: repo.join("target/browser-bridge-bench/work"),
        desktop: DesktopSetup::default(),
        player_test_variables: PlayerTestVariables::default(),
    })?;
    environment.connect_browser(kind)?;

    let mut browser = launch(
        kind,
        &environment,
        LaunchOptions {
            window_privacy: browser_bridge_bench::browsers::WindowPrivacy::Normal,
            extensions: vec![ExtensionSource::UnpackedDir(
                repo.join("extensions/browser/dist").join(dist_name),
            )],
            autoplay_without_gesture: true,
            loopback_host_aliases: Vec::new(),
            runtime_extension_install: false,
        },
    )?;
    let text =
        browser.open_extension_page(ExtensionPage::popup_with_text("Connected to Fastiplayer"))?;
    println!("[{kind:?}] popup text: {text:?}");
    let exit = browser.shutdown()?;
    println!("[{kind:?}] exit: {exit:?}");
    let evidence = read_host_evidence(&exit.stderr_log)?;
    println!("[{kind:?}] host evidence: {evidence:?}");
    // Плеер разбудила частная шина (по `.service` установщика) и он пишет в player.log.
    let player_log = std::fs::read_to_string(environment.player_log()).unwrap_or_default();
    println!("[{kind:?}] player.log:\n{player_log}");
    println!("[{kind:?}] wall time: {:?}", started.elapsed());

    if !evidence.caller_accepted {
        // Помогает разобрать сбой: каталог прогона исчезнет при выходе из функции.
        let stderr = std::fs::read_to_string(&exit.stderr_log).unwrap_or_default();
        println!("[{kind:?}] browser stderr:\n{stderr}");
    }
    assert!(
        text.contains("Connected to Fastiplayer"),
        "popup said: {text}"
    );
    assert!(evidence.caller_accepted, "the host saw the browser call it");
    Ok(())
}

#[test]
#[ignore = "live: starts kwin, Chromium and the real player"]
fn live_chromium_popup_reports_connected() -> Result<(), Box<dyn std::error::Error>> {
    popup_text_of_real_extension(BrowserKind::Chromium, "chrome")
}

#[test]
#[ignore = "live: starts kwin, Firefox and the real player"]
fn live_firefox_popup_reports_connected() -> Result<(), Box<dyn std::error::Error>> {
    popup_text_of_real_extension(BrowserKind::Firefox, "firefox")
}
