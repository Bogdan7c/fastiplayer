//! Параллельный прогон стенда частями (сессия 16b3c2, решение 386).
//!
//! Раньше все строки шли строго по очереди в одном процессе, хотя каждая уже жила в своей
//! песочнице. Теперь родитель раскладывает прогон на части ([`plan_parts`], быстрый набор —
//! [`quick_parts`]), гонит до `--jobs` частей одновременно дочерними процессами стенда
//! ([`pool`]) и сводит их строки в одну таблицу. Родитель сам ролики не генерирует в
//! параллели: кэш медиа он готовит один раз до запуска детей.

mod part_outcome;
mod plan;
mod pool;
mod quick_suite;
mod result_file;

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use browser_bridge_bench::media::ensure_media;
use browser_bridge_bench::scenarios::{RunSettings, StepResult};

pub use plan::plan_parts;
pub use quick_suite::quick_parts;
pub use result_file::write_results;

use crate::cli::JobCount;
use plan::RunPart;
use pool::{PartRunner, run_parts};

/// Итог прогона частями.
pub struct PartsRun {
    /// Строки всех частей в порядке частей.
    pub results: Vec<StepResult>,
    /// Подготовка (кэш медиа и каталог частей).
    pub setup: Duration,
}

/// Готовит кэш медиа и каталог частей, гонит части и возвращает строки.
pub fn run_in_parts(
    parts: &[RunPart],
    jobs: JobCount,
    settings: &RunSettings,
) -> Result<PartsRun, String> {
    let started = Instant::now();
    // Один раз до детей: иначе несколько детей разом начали бы генерировать один кэш.
    ensure_media(&settings.media_cache).map_err(|error| format!("media cache: {error}"))?;
    let jobs_dir = jobs_directory(settings)?;
    let executable =
        std::env::current_exe().map_err(|error| format!("the bench executable: {error}"))?;
    let setup = started.elapsed();
    eprintln!(
        "running {} parts, {} at a time (part logs: {})",
        parts.len(),
        jobs.get(),
        jobs_dir.display()
    );
    let runner = PartRunner {
        executable,
        settings,
        jobs_dir: jobs_dir.clone(),
    };
    let results = run_parts(parts, jobs, &runner);
    // Каталог пуст, если все части прошли (журналы провалов остаются в нём).
    if std::fs::remove_dir(&jobs_dir).is_ok() {
        eprintln!("all parts passed, part logs removed");
    }
    Ok(PartsRun { results, setup })
}

/// Каталог журналов частей: `<work-root>/jobs-<pid>-<наносекунды>`.
fn jobs_directory(settings: &RunSettings) -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    let directory = settings
        .work_root
        .join(format!("jobs-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    Ok(directory)
}
