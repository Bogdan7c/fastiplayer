//! Точка входа стенда: `browser-bridge-bench [ОПЦИИ] <pages|smoke|extension-status|all>`.
//!
//! Здесь только ввод-вывод и код выхода: сценарии (`scenarios`) возвращают типизированные
//! результаты, таблицу печатает `report`, аргументы разбирает `cli`.

mod cli;
mod report;

use std::process::ExitCode;
use std::time::Instant;

use browser_bridge_bench::environment::install_interrupt_cleanup;
use browser_bridge_bench::scenarios::{self, Stand, StepResult};

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
        settings,
    } = invocation;
    let stand = match Stand::start(settings) {
        Ok(stand) => stand,
        Err(error) => {
            eprintln!("error: cannot prepare the stand: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let setup = started.elapsed();
    let mut results: Vec<StepResult> = Vec::new();
    for steps in plan(command) {
        for &browser in &browsers {
            eprintln!("running {steps:?} in {browser:?} ...");
            match steps {
                Command::Pages => results.extend(scenarios::run_pages(&stand, browser)),
                Command::Smoke => results.push(scenarios::run_smoke(&stand, browser)),
                Command::ExtensionStatus => {
                    results.push(scenarios::run_extension_status(&stand, browser));
                }
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

/// Какие шаги входят в команду (`all` = все три по очереди; страницы идут первыми).
fn plan(command: Command) -> Vec<Command> {
    match command {
        Command::All => vec![Command::Pages, Command::Smoke, Command::ExtensionStatus],
        single => vec![single],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_runs_pages_first_then_smoke_then_extension_status() {
        assert_eq!(
            plan(Command::All),
            vec![Command::Pages, Command::Smoke, Command::ExtensionStatus]
        );
        assert_eq!(plan(Command::Smoke), vec![Command::Smoke]);
    }
}
