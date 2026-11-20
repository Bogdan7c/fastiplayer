//! Пул частей: до `--jobs` дочерних процессов стенда одновременно.
//!
//! Рабочий поток берёт следующую часть, запускает ребёнка (тот же бинарь с аргументами
//! части), ждёт его и читает файл итогов. Ребёнок — лидер своей группы процессов: Ctrl-C
//! терминала достаётся только родителю, а родитель пересылает ребёнку SIGTERM
//! ([`ForwardedChild`]) — ребёнок убирает свои KWin, шину и браузер сам.

use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitStatus, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use browser_bridge_bench::environment::ForwardedChild;
use browser_bridge_bench::scenarios::{Outcome, RunSettings, StepResult};

use super::part_outcome::{ChildExit, ChildReport, rows_of_part};
use super::plan::RunPart;
use super::result_file::read_results;
use crate::cli::{ChildRequest, JobCount, child_arguments};

/// Как запускать части.
pub struct PartRunner<'a> {
    /// Бинарь стенда (этот же процесс).
    pub executable: PathBuf,
    /// Настройки родителя: ребёнок получает те же.
    pub settings: &'a RunSettings,
    /// Каталог журналов и файлов итогов частей этого прогона.
    pub jobs_dir: PathBuf,
}

/// Гонит все части по `jobs` сразу и возвращает строки в порядке частей.
pub fn run_parts(parts: &[RunPart], jobs: JobCount, runner: &PartRunner<'_>) -> Vec<StepResult> {
    let next_part = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    let rows_by_part: Mutex<Vec<Option<Vec<StepResult>>>> = Mutex::new(vec![None; parts.len()]);
    let workers = jobs.get().min(parts.len()).max(1);
    let launch_order = launch_order(parts);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let turn = next_part.fetch_add(1, Ordering::Relaxed);
                    let Some(&index) = launch_order.get(turn) else {
                        return;
                    };
                    let part = &parts[index];
                    let rows = run_part(index, part, runner);
                    let done = finished.fetch_add(1, Ordering::Relaxed) + 1;
                    report_progress(done, parts.len(), part, &rows);
                    if let Ok(mut slots) = rows_by_part.lock() {
                        slots[index] = Some(rows);
                    }
                }
            });
        }
    });
    let slots = rows_by_part
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    slots.into_iter().flatten().flatten().collect()
}

/// Порядок запуска: долгие части первыми, внутри одного вида — порядок таблицы.
fn launch_order(parts: &[RunPart]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by_key(|&index| parts[index].start);
    order
}

/// Одна часть: ребёнок от запуска до строк.
fn run_part(index: usize, part: &RunPart, runner: &PartRunner<'_>) -> Vec<StepResult> {
    let started = Instant::now();
    let stem = format!("{index:03}-{}", file_safe(&part.label()));
    let log = runner.jobs_dir.join(format!("{stem}.log"));
    let results_file = runner.jobs_dir.join(format!("{stem}.json"));
    let exit = spawn_and_wait(part, runner, &log, &results_file);
    let rows = match exit {
        ChildExit::NotStarted(_) => Err("the child did not start".to_owned()),
        _ => read_results(&results_file).map_err(|error| error.to_string()),
    };
    let rows = rows_of_part(
        part,
        ChildReport {
            exit,
            rows,
            log: &log,
            elapsed: started.elapsed(),
        },
    );
    // Журнал части нужен только для разбора провала (и по `--keep-work`).
    let failed = rows.iter().any(|row| row.outcome == Outcome::Fail);
    if !failed && !runner.settings.keep_work {
        remove_quietly(&log);
        remove_quietly(&results_file);
    }
    rows
}

/// Запускает ребёнка и ждёт его конца.
fn spawn_and_wait(
    part: &RunPart,
    runner: &PartRunner<'_>,
    log: &Path,
    results_file: &Path,
) -> ChildExit {
    let request = ChildRequest {
        command: part.command,
        browser: part.browser,
        page: part.page,
        results_file,
    };
    let arguments = match child_arguments(&request, runner.settings) {
        Ok(arguments) => arguments,
        Err(error) => return ChildExit::NotStarted(error.to_string()),
    };
    let (stdout, stderr) = match File::create(log).and_then(|file| Ok((file.try_clone()?, file))) {
        Ok(files) => files,
        Err(error) => return ChildExit::NotStarted(format!("part log {}: {error}", log.display())),
    };
    let mut command = ProcessCommand::new(&runner.executable);
    command
        .args(&arguments)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        // Своя группа: Ctrl-C терминала не бьёт ребёнка дважды (сам и пересылкой родителя).
        .process_group(0);
    if let Some(row) = part.row {
        let (variable, value) = row.env_pair();
        command.env(variable, value);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return ChildExit::NotStarted(error.to_string()),
    };
    // Если реестр полон, ребёнок всё равно идёт: он не осиротеет, просто при Ctrl-C
    // родителя доработает свою часть сам. Об этом — строка в журнале части.
    let forwarding = ForwardedChild::register(child.id());
    if let Err(error) = &forwarding {
        eprintln!(
            "warning: {} will not get Ctrl-C from the parent: {error}",
            part.label()
        );
    }
    let exit = match child.wait() {
        Ok(status) => exit_of(status),
        Err(error) => ChildExit::NotStarted(format!("waiting for the child: {error}")),
    };
    drop(forwarding);
    exit
}

fn exit_of(status: ExitStatus) -> ChildExit {
    status.code().map_or(ChildExit::Killed, ChildExit::Code)
}

/// Строка прогресса в stderr: таблица печатается в конце, а прогон идёт минуты.
fn report_progress(done: usize, total: usize, part: &RunPart, rows: &[StepResult]) {
    let verdict = if rows.iter().any(|row| row.outcome == Outcome::Fail) {
        "FAIL"
    } else {
        "done"
    };
    let seconds: f64 = rows.iter().map(|row| row.elapsed.as_secs_f64()).sum();
    eprintln!(
        "[{done}/{total}] {verdict} {} ({seconds:.1}s)",
        part.label()
    );
}

/// Имя части для имени файла: только буквы, цифры и `-`.
fn file_safe(label: &str) -> String {
    label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

/// Убирает служебный файл части; если не вышло, это не провал прогона, но и не тайна.
fn remove_quietly(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("warning: cannot remove {}: {error}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::super::plan::StartOrder;
    use super::*;

    #[test]
    fn long_parts_launch_first_and_keep_their_table_order() {
        let part = |page: &'static str, start: StartOrder| RunPart {
            command: crate::cli::Command::Capture,
            browser: browser_bridge_bench::browsers::BrowserKind::Chromium,
            page: Some(page),
            row: None,
            expected: None,
            start,
        };
        let parts = [
            part("a", StartOrder::Normal),
            part("b", StartOrder::Early),
            part("c", StartOrder::Normal),
            part("d", StartOrder::Early),
        ];
        assert_eq!(launch_order(&parts), [1, 3, 0, 2]);
    }

    #[test]
    fn part_labels_become_plain_file_names() {
        assert_eq!(
            file_safe("tab-return chromium tab-return =return-mse x"),
            "tab-return-chromium-tab-return--return-mse-x"
        );
    }
}
