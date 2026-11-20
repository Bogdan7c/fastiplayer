//! Заметки экстеншена для разработчика в stderr браузера (16b3c4).
//!
//! Экстеншен пишет заметки в консоль: `console.debug("[fastiplayer] …")` (фон — `noteForDeveloper`,
//! в том числе память выбора; контент-скрипты — сбои адаптеров). Где они лежат —
//! `BrowserKind::extension_notes_log_path`: Firefox стенда дублирует консоль в stdout процесса
//! (`devtools.console.stdout.*` в `user.js`, тот же журнал, что stderr), консоль служебного воркера
//! Chromium стенд пишет сам (`browsers::worker_console`). Здесь из журнала выбираются только заметки,
//! чтобы приговор строки мог их показать.
//!
//! Строки посредника (`fastiplayer-browser-host: …`) — не заметки экстеншена: их разбирает
//! `host_evidence`.

use std::path::Path;

use super::ObserveError;

/// Метка заметки экстеншена (общая для фона и контент-скриптов).
pub const EXTENSION_NOTE_MARKER: &str = "[fastiplayer] ";

/// Заметки экстеншена из текста журнала в порядке появления: от метки до конца строки, без кавычки,
/// которой консоль Firefox закрывает строку.
pub fn parse_extension_notes(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let start = line.find(EXTENSION_NOTE_MARKER)?;
            let note = line[start..].trim_end();
            Some(note.strip_suffix('"').unwrap_or(note).to_owned())
        })
        .collect()
}

/// Читает журнал запуска браузера и выбирает заметки экстеншена.
pub fn read_extension_notes(browser_stderr: &Path) -> Result<Vec<String>, ObserveError> {
    let bytes = std::fs::read(browser_stderr).map_err(|source| ObserveError::Read {
        path: browser_stderr.to_path_buf(),
        source,
    })?;
    Ok(parse_extension_notes(&String::from_utf8_lossy(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_extension_notes_are_taken_in_order() {
        let text = "fastiplayer-browser-host: caller accepted browser=firefox\n\
            console.debug: \"[fastiplayer] variant adapter bench-series@1 describes the page\"\n\
            unrelated noise [fastiplayer\n\
            console.debug: \"[fastiplayer] choice remembered (audio chosen on the page)\"  \n";
        assert_eq!(
            parse_extension_notes(text),
            vec![
                "[fastiplayer] variant adapter bench-series@1 describes the page".to_owned(),
                "[fastiplayer] choice remembered (audio chosen on the page)".to_owned(),
            ]
        );
    }

    #[test]
    fn journal_without_notes_gives_nothing() {
        assert!(
            parse_extension_notes("fastiplayer-browser-host: answered request=hello\n").is_empty()
        );
    }

    #[test]
    fn missing_journal_is_an_error_not_silence() {
        let error = read_extension_notes(Path::new("/nonexistent/firefox.stderr.log"))
            .expect_err("no file");
        assert!(matches!(error, ObserveError::Read { .. }));
    }
}
