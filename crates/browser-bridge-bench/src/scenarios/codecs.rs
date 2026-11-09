//! `codecs`: плеер сайта спрашивает кодеки AV1 → VP9 → H.264, и экстеншен (решения 43 и 64)
//! отвечает за плеер. Страница `codec-choice` играет первый поддержанный кодек по-настоящему.
//!
//! Путь каждого случая (см. `codec_cases`): окружение с нужными настройками плеера → браузер
//! с настоящим экстеншеном → (настройка, записанная со страницы настроек) → попап будит плеера,
//! hello приносит метку «дешёвых» кодеков → ждём, пока фон зарегистрирует курьера → ТОЛЬКО ПОТОМ
//! открываем страницу (ответы должны быть готовы до первого вопроса плеера сайта).

use std::time::{Duration, Instant};

use super::codec_cases::{
    CARRIER_SCRIPT_ID, CASES, CODEC_SETTING_KEY, CodecCase, HelloTrigger, judge_report,
};
use super::context::{ScenarioError, Stand};
use super::handoff_run::real_extension_dir;
use super::outcome::{StepResult, Verdict};
use super::pages::page_url;
use crate::browsers::{BrowserKind, BrowserUnderTest, ExtensionPage, ExtensionSource};
use crate::site::RunId;

/// Текст попапа при живой связи: значит, hello принят и метка плеера дошла до фона.
const CONNECTED_TEXT: &str = "Connected to Fastiplayer";
/// Сколько ждать отчёт страницы (она играет видео и ждёт вложенный фрейм).
const PAGE_REPORT_TIMEOUT: Duration = Duration::from_secs(60);
/// Сколько ждать, пока фон приведёт регистрацию курьера к нужной.
const CARRIER_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(15);
/// Сколько ждать курьера, когда hello делает сам фон: холодный старт плеера по D-Bus
/// и браузера с экстеншеном идут параллельно, запас больше, чем при открытом попапе.
const BACKGROUND_HELLO_TIMEOUT: Duration = Duration::from_secs(40);
/// Шаг опроса регистрации.
const CARRIER_POLL_STEP: Duration = Duration::from_millis(250);
/// Пауза, когда регистрации быть НЕ должно: у «ничего не произошло» нет признака, подождём,
/// чтобы ошибочная регистрация (если фон её делает) успела появиться и была поймана.
const NO_CARRIER_SETTLE: Duration = Duration::from_secs(2);

pub(super) fn run(stand: &Stand, browser: BrowserKind) -> Vec<StepResult> {
    CASES
        .iter()
        .map(|case| {
            let started = Instant::now();
            let verdict =
                stand.with_environment_for_player_config(case.player_config, |environment| {
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
                    let verdict = drive_case(stand, session.as_mut(), case);
                    session.shutdown()?;
                    verdict
                });
            StepResult::new("codecs", browser, case.name, verdict, started.elapsed())
        })
        .collect()
}

/// Ход одного случая внутри запущенного браузера.
fn drive_case(
    stand: &Stand,
    session: &mut dyn BrowserUnderTest,
    case: &CodecCase,
) -> Result<Verdict, ScenarioError> {
    if let Some(setting) = case.setting {
        let saved = session.run_in_extension_page(&write_setting_function(setting))?;
        if saved != "saved" {
            return Ok(Verdict::fail(format!(
                "could not write the setting: {saved}"
            )));
        }
    }
    let carrier_timeout = match case.hello {
        HelloTrigger::PopupOpened => {
            // Попап открывает связь с плеером: hello_accepted приносит метку, фон кэширует её
            // и приводит курьера к политике.
            let popup =
                session.open_extension_page(ExtensionPage::popup_with_text(CONNECTED_TEXT))?;
            if !popup.contains(CONNECTED_TEXT) {
                return Ok(Verdict::fail(format!(
                    "the popup never said '{CONNECTED_TEXT}': {popup:?}"
                )));
            }
            CARRIER_REGISTRATION_TIMEOUT
        }
        // Ни попапа, ни передачи: только ждём, пока фон сам сходит с hello к плееру.
        HelloTrigger::BackgroundOnInstall => BACKGROUND_HELLO_TIMEOUT,
    };
    let carrier = match wait_for_carrier(session, case.expected_carrier_file, carrier_timeout)? {
        Ok(registered) => registered,
        Err(problem) => return Ok(Verdict::fail(problem)),
    };

    let run = RunId::new_unique("codec-choice");
    let url = page_url(
        &stand.site.main_origin(),
        &stand.site.other_origin(),
        "codec-choice",
        &run,
    );
    let tab = session.open_tab(&url)?;
    let report = stand.site.wait_report(&run, PAGE_REPORT_TIMEOUT);
    // Вкладку закрываем при любом исходе, как в `pages`.
    session.close_tab(tab)?;
    let report = report.map_err(ScenarioError::from)?;
    let failures = judge_report(case, &report);
    let evidence = format!(
        "carrier registered: {carrier}; checks: {}",
        report
            .checks
            .iter()
            .filter(|check| matches!(check.name.as_str(), "chosen-codec" | "frame-same-choice"))
            .map(|check| format!("{}={}", check.name, check.detail))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(Verdict::from_failures(&failures, evidence))
}

/// Ждёт, пока регистрация курьера станет такой, как нужно; `Err(текст)` — не дождались.
fn wait_for_carrier(
    session: &mut dyn BrowserUnderTest,
    expected_file: Option<&str>,
    timeout: Duration,
) -> Result<Result<String, String>, ScenarioError> {
    let expected = expected_file.map_or_else(|| "[]".to_owned(), |file| format!("[[\"{file}\"]]"));
    if expected_file.is_none() {
        std::thread::sleep(NO_CARRIER_SETTLE);
    }
    let deadline = Instant::now() + timeout;
    loop {
        let registered = session.run_in_extension_page(&read_registration_function())?;
        if registered == expected {
            return Ok(Ok(registered));
        }
        if Instant::now() >= deadline {
            return Ok(Err(format!(
                "the dynamic content script is {registered}, expected {expected}"
            )));
        }
        std::thread::sleep(CARRIER_POLL_STEP);
    }
}

/// Скрипт страницы настроек: записать значение настройки в `storage.local`.
fn write_setting_function(setting: &str) -> String {
    format!(
        "async (api) => {{ await api.storage.local.set({{ {CODEC_SETTING_KEY}: {setting:?} }}); return 'saved'; }}"
    )
}

/// Скрипт страницы настроек: файлы зарегистрированного курьера, JSON-строкой (`[]` — нет курьера).
fn read_registration_function() -> String {
    format!(
        "async (api) => JSON.stringify((await api.scripting.getRegisteredContentScripts({{ ids: [{CARRIER_SCRIPT_ID:?}] }})).map((script) => script.js))"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_script_writes_the_exact_key_and_value() {
        let script = write_setting_function("always_h264");
        assert!(script.contains("codecSetting: \"always_h264\""), "{script}");
        assert!(script.contains("storage.local.set"));
    }

    #[test]
    fn the_registration_script_asks_for_the_carrier_id_only() {
        let script = read_registration_function();
        assert!(
            script.contains("ids: [\"fastiplayer-codec-policy\"]"),
            "{script}"
        );
        assert!(script.contains("JSON.stringify"));
    }
}
