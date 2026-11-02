//! Стенд живой проверки установщика экстеншена (сессия 07a).
//!
//! ```text
//! cargo run -p browser-bridge-install --example extension_install -- survey
//! cargo run -p browser-bridge-install --example extension_install -- \
//!     --chrome-web-store-id <32 буквы a-p> --firefox-add-ons-slug <slug> install chromium
//! cargo run -p browser-bridge-install --example extension_install -- \
//!     --chrome-web-store-id <id> cancel chromium
//! ```
//!
//! Пока наш экстеншен не опубликован, флаги подставляют ID/slug чужого экстеншена
//! из настоящего магазина: так проверяется весь путь «подсказка → браузер ставит».
//! Окружение берётся из текущего процесса, трогаются настоящие профили браузеров.

use std::process::ExitCode;

use browser_bridge_install::InstallEnvironment;
use browser_bridge_install::{
    BrowserTargetId, ExtensionInstaller, ExtensionListings, ExtensionReport, Listing,
    RealBrowserLauncher, RealFlatpakTool,
};

/// Что просили сделать.
enum Command {
    Survey,
    Install(BrowserTargetId),
    /// Отмена ещё не случившейся установки (удаляет только нашу подсказку).
    Cancel(BrowserTargetId),
}

/// Разобранные аргументы.
struct Arguments {
    chrome_web_store_id: Option<String>,
    firefox_add_ons_slug: Option<String>,
    command: Command,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let arguments = parse_arguments(std::env::args().skip(1))?;
    let environment = InstallEnvironment::from_process_env().map_err(|error| error.to_string())?;
    let listings = listings_from(&arguments)?;
    let installer = ExtensionInstaller::new(
        environment,
        listings,
        Box::new(RealFlatpakTool::new()),
        Box::new(RealBrowserLauncher::new()),
    );
    match arguments.command {
        Command::Survey => {
            let survey = installer.survey().map_err(|error| error.to_string())?;
            if let Some(problem) = &survey.flatpak_problem {
                println!("flatpak: {problem}");
            }
            if survey.browsers.is_empty() {
                println!("no supported browsers found");
            }
            survey.browsers.iter().for_each(print_report);
        }
        Command::Install(target) => {
            let outcome = installer
                .install(&target)
                .map_err(|error| error.to_string())?;
            println!("{}: {outcome:?}", target.as_str());
        }
        Command::Cancel(target) => {
            let outcome = installer
                .cancel_pending_install(&target)
                .map_err(|error| error.to_string())?;
            println!("{}: {outcome:?}", target.as_str());
        }
    }
    Ok(())
}

fn print_report(report: &ExtensionReport) {
    println!(
        "{:<28} {:?}  action={:?}  hint={:?}",
        report.target.as_str(),
        report.state,
        report.install_action,
        report.hint
    );
}

fn listings_from(arguments: &Arguments) -> Result<ExtensionListings, String> {
    let published = |value: &Option<String>| match value {
        Some(value) => Listing::Published(value.clone()),
        None => Listing::NotPublished,
    };
    if arguments.chrome_web_store_id.is_none() && arguments.firefox_add_ons_slug.is_none() {
        return Ok(ExtensionListings::published());
    }
    ExtensionListings::new(
        published(&arguments.chrome_web_store_id),
        published(&arguments.firefox_add_ons_slug),
    )
    .map_err(|error| error.to_string())
}

fn parse_arguments(mut raw: impl Iterator<Item = String>) -> Result<Arguments, String> {
    let mut chrome_web_store_id = None;
    let mut firefox_add_ons_slug = None;
    let mut positional = Vec::new();
    while let Some(argument) = raw.next() {
        match argument.as_str() {
            "--chrome-web-store-id" => {
                chrome_web_store_id =
                    Some(raw.next().ok_or("--chrome-web-store-id needs a value")?);
            }
            "--firefox-add-ons-slug" => {
                firefox_add_ons_slug =
                    Some(raw.next().ok_or("--firefox-add-ons-slug needs a value")?);
            }
            _ => positional.push(argument),
        }
    }
    let command = match positional.as_slice() {
        [verb] if verb == "survey" => Command::Survey,
        [verb, target] if verb == "install" => Command::Install(BrowserTargetId::new(target)),
        [verb, target] if verb == "cancel" => Command::Cancel(BrowserTargetId::new(target)),
        _ => return Err("usage: [--chrome-web-store-id ID] [--firefox-add-ons-slug SLUG] survey | install <target> | cancel <target>".to_owned()),
    };
    Ok(Arguments {
        chrome_web_store_id,
        firefox_add_ons_slug,
        command,
    })
}
