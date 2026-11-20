//! Точка входа стенда: `browser-bridge-bench [ОПЦИИ] <pages|smoke|capture|page-ends|player-commands|extension-status|codecs|attacks|local-address|hls-live|live-ends|page-subtitles|quality|series|episodes|entries|page-return|tab-return|page-reload|auto-handoff|blocker-matrix|all>`.
//!
//! Здесь только ввод-вывод и код выхода: сценарии (`scenarios`) возвращают типизированные
//! результаты, таблицу печатает `report`, аргументы разбирает `cli`.

mod cli;
mod fan_out;
mod generate;
mod report;

use std::process::ExitCode;
use std::time::{Duration, Instant};

use browser_bridge_bench::browsers::BrowserKind;
use browser_bridge_bench::environment::install_interrupt_cleanup;
use browser_bridge_bench::scenarios::{self, PageFilter, ReplayOptions, Stand, StepResult};

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
        jobs: invocation_jobs,
        results_file,
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
    let in_parts = command == Command::Quick || invocation_jobs.is_parallel();
    let run = if in_parts {
        let parts = if command == Command::Quick {
            fan_out::quick_parts(&browsers)
        } else {
            fan_out::plan_parts(&plan(command, &pages), &browsers, &pages)
        };
        fan_out::run_in_parts(&parts, invocation_jobs, &settings)
            .map(|run| (run.results, run.setup))
    } else {
        run_in_this_process(settings, command, &browsers, &pages, replay.as_ref())
    };
    let (results, setup) = match run {
        Ok(run) => run,
        Err(message) => {
            eprintln!("error: cannot prepare the stand: {message}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    // Ребёнок параллельного прогона отдаёт строки родителю файлом (таблица — для человека).
    if let Some(path) = &results_file
        && let Err(error) = fan_out::write_results(path, &results)
    {
        eprintln!("error: {error}");
        return ExitCode::from(EXIT_FAILURE);
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

/// Прогон одним процессом, шаг за шагом (как до 16b3c2; так же гоняет свою часть ребёнок).
/// Возвращает строки и время подготовки стенда.
fn run_in_this_process(
    settings: scenarios::RunSettings,
    command: Command,
    browsers: &[BrowserKind],
    pages: &PageFilter,
    replay: Option<&ReplayOptions>,
) -> Result<(Vec<StepResult>, Duration), String> {
    let started = Instant::now();
    let stand = Stand::start(settings).map_err(|error| error.to_string())?;
    let setup = started.elapsed();
    let mut results: Vec<StepResult> = Vec::new();
    for steps in plan(command, pages) {
        for &browser in browsers {
            eprintln!("running {steps:?} in {browser:?} ...");
            results.extend(run_step(&stand, steps, browser, pages, replay));
        }
    }
    Ok((results, setup))
}

/// Один шаг плана в одном браузере.
fn run_step(
    stand: &Stand,
    steps: Command,
    browser: BrowserKind,
    pages: &PageFilter,
    replay: Option<&ReplayOptions>,
) -> Vec<StepResult> {
    match steps {
        Command::Pages => scenarios::run_pages(stand, browser, pages),
        Command::Smoke => vec![scenarios::run_smoke(stand, browser)],
        Command::Capture => scenarios::run_capture(stand, browser, pages),
        Command::PageEnds => scenarios::run_page_ends(stand, browser, pages),
        Command::PlayerCommands => scenarios::run_player_commands(stand, browser, pages),
        Command::ExtensionStatus => vec![scenarios::run_extension_status(stand, browser)],
        Command::Codecs => scenarios::run_codecs(stand, browser),
        Command::Attacks => scenarios::run_attacks(stand, browser, pages),
        Command::LocalAddress => scenarios::run_local_address(stand, browser),
        Command::HlsLive => vec![scenarios::run_hls_live(stand, browser)],
        Command::LiveEnds => scenarios::run_live_ends(stand, browser),
        Command::HlsLiveSleep => vec![scenarios::run_hls_live_sleep(stand, browser)],
        Command::PageSubtitles => scenarios::run_page_subtitles(stand, browser, pages),
        Command::Quality => scenarios::run_quality(stand, browser, pages),
        Command::Series => scenarios::run_series(stand, browser, pages),
        Command::SeriesMemory => scenarios::run_series_memory(stand, browser, pages),
        Command::Episodes => scenarios::run_episodes(stand, browser, pages),
        Command::Entries => scenarios::run_entries(stand, browser, pages),
        Command::PageReturn => scenarios::run_page_return(stand, browser, pages),
        Command::TabReturn => scenarios::run_tab_return(stand, browser, pages),
        Command::PageReload => scenarios::run_page_reload(stand, browser, pages),
        Command::AutoHandoff => scenarios::run_auto_handoff(stand, browser, pages),
        Command::BlockerMatrix => scenarios::run_blocker_matrix(stand, browser),
        Command::Replay => replay
            .map(|options| vec![scenarios::run_replay(stand, browser, options)])
            .unwrap_or_default(),
        // Раскладываются раньше: `all` — планом, `quick` — частями, генерация — в `run`.
        Command::GenerateRecording | Command::Quick | Command::All => Vec::new(),
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
            Command::LiveEnds,
            Command::HlsLiveSleep,
            Command::PageSubtitles,
            Command::Quality,
            Command::Series,
            Command::SeriesMemory,
            Command::Episodes,
            Command::Entries,
            Command::PageReturn,
            Command::TabReturn,
            Command::PageReload,
            Command::AutoHandoff,
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
                Command::LiveEnds,
                Command::HlsLiveSleep,
                Command::PageSubtitles,
                Command::Quality,
                Command::Series,
                Command::SeriesMemory,
                Command::Episodes,
                Command::Entries,
                Command::PageReturn,
                Command::TabReturn,
                Command::PageReload,
                Command::AutoHandoff
            ]
        );
        // Матрица блокировщиков качает файлы из сети: в `all` её нет.
        assert!(!plan(Command::All, &PageFilter::all()).contains(&Command::BlockerMatrix));
        assert_eq!(
            plan(Command::Smoke, &PageFilter::all()),
            vec![Command::Smoke]
        );
    }

    /// Команда `hls-live-sleep` (16d4) разбирается, не берёт `--page` и есть в справке (тест здесь,
    /// а не в `cli.rs`: тот у лимита размера модуля).
    #[test]
    fn hls_live_sleep_is_a_command_of_its_own_listed_in_the_help() {
        let words = |list: &[&str]| cli::parse(list.iter().map(|word| (*word).to_owned()));
        match words(&["hls-live-sleep"]).expect("parses") {
            cli::Parsed::Run(invocation) => assert_eq!(invocation.command, Command::HlsLiveSleep),
            cli::Parsed::Help => panic!("expected a run"),
        }
        assert!(words(&["hls-live-sleep", "--page", "hls-live-sleep"]).is_err());
        assert!(cli::USAGE.contains("  hls-live-sleep "));
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
