//! Точка входа стенда: `browser-bridge-bench [ОПЦИИ] <pages|smoke|capture|page-ends|player-commands|extension-status|codecs|attacks|local-address|hls-live|entries|blocker-matrix|all>`.
//!
//! Здесь только ввод-вывод и код выхода: сценарии (`scenarios`) возвращают типизированные
//! результаты, таблицу печатает `report`, аргументы разбирает `cli`.

mod cli;
mod generate;
mod report;

use std::process::ExitCode;
use std::time::Instant;

use browser_bridge_bench::environment::install_interrupt_cleanup;
use browser_bridge_bench::scenarios::{self, PageFilter, Stand, StepResult};

use cli::{Command, Invocation, Parsed};

/// Коды выхода: успех, провал проверки, ошибка использования.
const EXIT_FAILURE: u8 = 1;
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    let invocation = match cli::parse(std::env::args().skip(1)) {
        Ok(Parsed::Help) => {
            println!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Run(invocation)) => invocation,
        Err(error) => {
            eprintln!("error: {error}\n\n{}", cli::USAGE);
            return ExitCode::from(EXIT_USAGE);
        }
    };
    run(*invocation)
}

fn run(invocation: Invocation) -> ExitCode {
    // До запуска любых процессов: по Ctrl-C/SIGTERM обработчик убьёт группы стенда
    // (иначе kwin, шина, плеер и браузер остаются сиротами).
    if let Err(error) = install_interrupt_cleanup() {
        eprintln!("error: cannot install the signal cleanup: {error}");
        return ExitCode::from(EXIT_FAILURE);
    }
    let started = Instant::now();
    let Invocation {
        command,
        browsers,
        pages,
        settings,
        replay,
        generation,
    } = invocation;
    // Генератору нужны только ролики: сайт, окружение и браузеры не поднимаются.
    if let (Command::GenerateRecording, Some(options)) = (command, &generation) {
        return match generate::generate_recording(&settings, options) {
            Ok(summary) => {
                println!("{summary}");
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("error: {message}");
                ExitCode::from(EXIT_FAILURE)
            }
        };
    }
    let stand = match Stand::start(settings) {
        Ok(stand) => stand,
        Err(error) => {
            eprintln!("error: cannot prepare the stand: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let setup = started.elapsed();
    let mut results: Vec<StepResult> = Vec::new();
    for steps in plan(command, &pages) {
        for &browser in &browsers {
            eprintln!("running {steps:?} in {browser:?} ...");
            match steps {
                Command::Pages => results.extend(scenarios::run_pages(&stand, browser, &pages)),
                Command::Smoke => results.push(scenarios::run_smoke(&stand, browser)),
                Command::Capture => {
                    results.extend(scenarios::run_capture(&stand, browser, &pages));
                }
                Command::PageEnds => {
                    results.extend(scenarios::run_page_ends(&stand, browser, &pages));
                }
                Command::PlayerCommands => {
                    results.extend(scenarios::run_player_commands(&stand, browser, &pages));
                }
                Command::ExtensionStatus => {
                    results.push(scenarios::run_extension_status(&stand, browser));
                }
                Command::Codecs => results.extend(scenarios::run_codecs(&stand, browser)),
                Command::Attacks => {
                    results.extend(scenarios::run_attacks(&stand, browser, &pages));
                }
                Command::LocalAddress => {
                    results.extend(scenarios::run_local_address(&stand, browser));
                }
                Command::HlsLive => results.push(scenarios::run_hls_live(&stand, browser)),
                Command::Entries => {
                    results.extend(scenarios::run_entries(&stand, browser, &pages));
                }
                Command::BlockerMatrix => {
                    results.extend(scenarios::run_blocker_matrix(&stand, browser));
                }
                Command::Replay => {
                    if let Some(options) = &replay {
                        results.push(scenarios::run_replay(&stand, browser, options));
                    }
                }
                Command::GenerateRecording => {}
                Command::All => {}
            }
        }
    }
    print!(
        "{}",
        report::format_table(&results, setup, started.elapsed())
    );
    if report::Totals::of(&results).is_success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_FAILURE)
    }
}

/// Какие шаги входят в команду (`all`: страницы первыми — если сами страницы плохи, мост
/// проверять бессмысленно; `smoke` не входит, это подмножество `capture`). С `--page` в `all`
/// остаются только шаги со страницами: `player-commands` и `extension-status` одну страницу
/// не выбирают. Шаг без выбранных страниц просто ничего не запускает (`cli` не пускает прогон,
/// в котором нет ни одной выбранной страницы).
fn plan(command: Command, pages: &PageFilter) -> Vec<Command> {
    match command {
        Command::All if pages.selects_everything() => vec![
            Command::Pages,
            Command::Capture,
            Command::PageEnds,
            Command::PlayerCommands,
            Command::ExtensionStatus,
            Command::Codecs,
            Command::Attacks,
            Command::LocalAddress,
            Command::HlsLive,
            Command::Entries,
        ],
        Command::All => vec![
            Command::Pages,
            Command::Capture,
            Command::PageEnds,
            Command::Attacks,
        ],
        single => vec![single],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_runs_pages_first_then_capture_page_ends_commands_and_extension_status() {
        assert_eq!(
            plan(Command::All, &PageFilter::all()),
            vec![
                Command::Pages,
                Command::Capture,
                Command::PageEnds,
                Command::PlayerCommands,
                Command::ExtensionStatus,
                Command::Codecs,
                Command::Attacks,
                Command::LocalAddress,
                Command::HlsLive,
                Command::Entries
            ]
        );
        // Матрица блокировщиков качает файлы из сети: в `all` её нет.
        assert!(!plan(Command::All, &PageFilter::all()).contains(&Command::BlockerMatrix));
        assert_eq!(
            plan(Command::Smoke, &PageFilter::all()),
            vec![Command::Smoke]
        );
    }

    #[test]
    fn all_with_a_page_filter_keeps_only_the_steps_that_have_pages() {
        let one_page = PageFilter::only(["page-freeze".to_owned()]);
        assert_eq!(
            plan(Command::All, &one_page),
            vec![
                Command::Pages,
                Command::Capture,
                Command::PageEnds,
                Command::Attacks
            ]
        );
    }
}
