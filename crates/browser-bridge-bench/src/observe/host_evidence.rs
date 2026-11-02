//! Свидетельства посредника в stderr браузера.
//!
//! Браузер пересылает stderr посредника в свой stderr, поэтому строки посредника лежат в
//! `<work_dir>/<браузер>.stderr.log`: `caller accepted browser=…`,
//! `answered request=hello … reply=player:hello_accepted`,
//! `answered request=open_session … reply=player:session_granted`.

use std::path::Path;

use super::ObserveError;

/// Что посредник сообщил о ходе сеанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HostLogEvidence {
    /// Браузер-вызывающий признан («caller accepted»).
    pub caller_accepted: bool,
    /// Плеер принял приветствие (`reply=player:hello_accepted`).
    pub hello_accepted: bool,
    /// Плеер выдал пропуск на сеанс (`reply=player:session_granted`).
    pub session_granted: bool,
}

/// Разбирает текст журнала.
pub fn parse_host_evidence(text: &str) -> HostLogEvidence {
    let mut evidence = HostLogEvidence::default();
    for line in text
        .lines()
        .filter(|line| line.contains("fastiplayer-browser-host"))
    {
        evidence.caller_accepted |= line.contains("caller accepted");
        evidence.hello_accepted |= line.contains("reply=player:hello_accepted");
        evidence.session_granted |= line.contains("reply=player:session_granted");
    }
    evidence
}

/// Читает stderr браузера и собирает свидетельства посредника.
pub fn read_host_evidence(browser_stderr: &Path) -> Result<HostLogEvidence, ObserveError> {
    let bytes = std::fs::read(browser_stderr).map_err(|source| ObserveError::Read {
        path: browser_stderr.to_path_buf(),
        source,
    })?;
    Ok(parse_host_evidence(&String::from_utf8_lossy(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_session_lines_set_all_three_flags() {
        let text = "[1:1:ERROR:noise] unrelated\n\
            fastiplayer-browser-host: caller accepted browser=chromium\n\
            fastiplayer-browser-host: answered request=hello bytes=94 reply=player:hello_accepted\n\
            fastiplayer-browser-host: answered request=open_session bytes=240 reply=player:session_granted\n";
        assert_eq!(
            parse_host_evidence(text),
            HostLogEvidence {
                caller_accepted: true,
                hello_accepted: true,
                session_granted: true
            }
        );
    }

    #[test]
    fn refused_session_leaves_the_flag_false() {
        let text = "fastiplayer-browser-host: caller accepted browser=firefox\n\
            fastiplayer-browser-host: answered request=hello bytes=94 reply=player:hello_accepted\n\
            fastiplayer-browser-host: answered request=open_session bytes=240 reply=player:refused\n";
        let evidence = parse_host_evidence(text);
        assert!(evidence.hello_accepted && !evidence.session_granted);
    }

    #[test]
    fn missing_file_is_an_error_not_an_empty_result() {
        let error = read_host_evidence(Path::new("/nonexistent/browser.stderr.log"))
            .expect_err("no such file");
        assert!(matches!(error, ObserveError::Read { .. }));
    }
}
