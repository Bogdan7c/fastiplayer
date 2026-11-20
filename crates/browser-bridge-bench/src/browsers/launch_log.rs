//! Журнал одного запуска браузера (16b3c4).
//!
//! Зачем. Строки стенда, которые перезапускают браузер на том же профиле (`series-memory-restart`,
//! приватное окно после обычного), пишут stderr каждого запуска по ОДНОМУ пути
//! (`BrowserKind::stderr_log_path`). Раньше новый запуск открывал файл через `File::create` и затирал
//! журнал прошлого запуска — а в нём как раз заметки экстеншена о записи памяти выбора.
//!
//! Решение — «отложить, а не дописать»: прошлый журнал переименовывается в
//! `<браузер>.stderr.launch-<N>.log`, а текущий путь остаётся за новым запуском.
//! Дописывать в тот же файл нельзя: улики посредника (`read_host_evidence`,
//! `wait_for_session_granted`) читают файл целиком, и пропуск прошлого запуска засчитался бы новому.

use std::io;
use std::path::{Path, PathBuf};

/// Сколько отложенных журналов допускаем у одного пути. Строки запускают браузер единицы раз;
/// потолок только защищает от бесконечного перебора имён, если каталог чем-то забит.
const MAX_SET_ASIDE_LAUNCHES: u32 = 1_000;

/// Если по пути журнала уже лежит журнал прошлого запуска — переименовывает его в первое свободное
/// `<основа>.launch-<N>.<расширение>` и возвращает новое имя. Журнала нет — `Ok(None)`, ничего не трогает.
pub(crate) fn set_aside_previous_launch_log(log_path: &Path) -> io::Result<Option<PathBuf>> {
    if !log_path.exists() {
        return Ok(None);
    }
    for launch in 1..=MAX_SET_ASIDE_LAUNCHES {
        let candidate = set_aside_name(log_path, launch);
        if candidate.exists() {
            continue;
        }
        std::fs::rename(log_path, &candidate)?;
        return Ok(Some(candidate));
    }
    Err(io::Error::other(format!(
        "{} already has {MAX_SET_ASIDE_LAUNCHES} set-aside launch logs",
        log_path.display()
    )))
}

/// Имя отложенного журнала: `firefox.stderr.log` → `firefox.stderr.launch-1.log`.
fn set_aside_name(log_path: &Path, launch: u32) -> PathBuf {
    let stem = log_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match log_path.extension() {
        Some(extension) => format!("{stem}.launch-{launch}.{}", extension.to_string_lossy()),
        None => format!("{stem}.launch-{launch}"),
    };
    log_path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_log_is_left_alone() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let log = dir.path().join("firefox.stderr.log");
        assert_eq!(set_aside_previous_launch_log(&log)?, None);
        assert_eq!(
            std::fs::read_dir(dir.path())?.count(),
            0,
            "nothing was created"
        );
        Ok(())
    }

    #[test]
    fn each_previous_launch_keeps_its_own_text() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let log = dir.path().join("firefox.stderr.log");
        std::fs::write(&log, "first launch\n")?;
        let first = set_aside_previous_launch_log(&log)?;
        std::fs::write(&log, "second launch\n")?;
        let second = set_aside_previous_launch_log(&log)?;
        assert_eq!(first, Some(dir.path().join("firefox.stderr.launch-1.log")));
        assert_eq!(second, Some(dir.path().join("firefox.stderr.launch-2.log")));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("firefox.stderr.launch-1.log"))?,
            "first launch\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("firefox.stderr.launch-2.log"))?,
            "second launch\n"
        );
        assert!(!log.exists(), "the current path is free for the new launch");
        Ok(())
    }
}
