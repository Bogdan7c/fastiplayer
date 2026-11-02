//! `extension-status`: НАСТОЯЩИЙ экстеншен 07 видит плеер и сообщает об этом в попапе.
//!
//! Это не проверка воспроизведения (её делают `smoke` и `capture`), а проверка,
//! что собранный `extensions/browser/dist/<браузер>` разговаривает с настоящим посредником
//! и плеером: попап показывает «Connected to Fastiplayer».

use std::time::Instant;

use super::context::{ScenarioError, Stand};
use super::handoff_run::real_extension_dir;
use super::outcome::{StepResult, Verdict};
use crate::browsers::{BrowserKind, ExtensionPage, ExtensionSource};
use crate::observe::read_host_evidence;

/// Текст попапа при живой связи (контракт интерфейса экстеншена 07).
const CONNECTED_TEXT: &str = "Connected to Fastiplayer";

pub(super) fn run(stand: &Stand, browser: BrowserKind) -> StepResult {
    let started = Instant::now();
    let verdict = stand.with_environment(|environment| {
        let dist = real_extension_dir(stand, browser);
        if !dist.join("manifest.json").is_file() {
            return Err(ScenarioError::RealExtensionNotBuilt { path: dist });
        }
        environment.connect_browser(browser)?;
        let mut session = stand.launch_browser(
            browser,
            environment,
            vec![ExtensionSource::UnpackedDir(dist)],
        )?;
        let popup = session.open_extension_page(ExtensionPage::popup_with_text(CONNECTED_TEXT))?;
        let exit = session.shutdown()?;
        let host = read_host_evidence(&exit.stderr_log)?;
        let mut failures = Vec::new();
        if !popup.contains(CONNECTED_TEXT) {
            failures.push(format!("popup does not say '{CONNECTED_TEXT}'"));
        }
        if !host.caller_accepted {
            failures.push("the host never saw the extension call it".to_owned());
        }
        let evidence = format!(
            "popup text: {:?}; host caller_accepted={}",
            popup.split_whitespace().collect::<Vec<_>>().join(" "),
            host.caller_accepted
        );
        Ok(Verdict::from_failures(&failures, evidence))
    });
    StepResult::new(
        "extension-status",
        browser,
        "popup",
        verdict,
        started.elapsed(),
    )
}
