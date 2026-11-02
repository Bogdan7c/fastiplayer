//! Журнал плеера: инкрементальное чтение и разбор сводок воспроизведения.
//!
//! Плеер раз в 10 с (и в конце) пишет строку
//! `Browser playback summary kind=periodic|final presented_frames=N … position_regressions=N …`.
//! `presented_frames` — кадры, дошедшие до показа в окне: критерий «видео реально играет».

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::ObserveError;

/// Начало строки сводки.
const SUMMARY_MARKER: &str = "Browser playback summary";
/// Шаг опроса файла.
const POLL_STEP: Duration = Duration::from_millis(200);

/// Показания одной сводки воспроизведения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameEvidence {
    /// Сколько кадров показано в окне.
    pub presented_frames: u64,
    /// Сколько раз позиция воспроизведения откатывалась назад (должно быть 0).
    pub position_regressions: u64,
    /// Строка журнала целиком.
    pub summary_line: String,
}

/// Читает файл журнала по мере роста; помнит, до какой строки уже «дочитал» ожидающий.
pub struct PlayerLog {
    path: PathBuf,
    /// Сколько байт файла уже забрано.
    offset: u64,
    /// Хвост последней строки без перевода строки (запись могла прерваться посередине).
    partial_line: String,
    /// Все целые строки, прочитанные с начала файла.
    lines: Vec<String>,
    /// Индекс первой строки, которую ещё не «потребил» `wait_*`.
    cursor: usize,
}

/// Достаёт `name=<число>` из строки журнала.
fn number_field(line: &str, name: &str) -> Option<u64> {
    let pattern = format!("{name}=");
    // Граница слева: поле не должно быть хвостом другого имени (`taken_frames` ≠ `frames`).
    let (start, _) = line
        .match_indices(&pattern)
        .find(|(index, _)| *index == 0 || line.as_bytes()[index - 1] == b' ')?;
    let digits: String = line[start + pattern.len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Разбор строки сводки; `None`, если строка не сводка или поля нет.
pub fn parse_summary(line: &str) -> Option<FrameEvidence> {
    if !line.contains(SUMMARY_MARKER) {
        return None;
    }
    Some(FrameEvidence {
        presented_frames: number_field(line, "presented_frames")?,
        position_regressions: number_field(line, "position_regressions")?,
        summary_line: line.to_owned(),
    })
}

impl PlayerLog {
    /// Открывает журнал; файла может ещё не быть (его создаёт шина при старте).
    pub fn open(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            offset: 0,
            partial_line: String::new(),
            lines: Vec::new(),
            cursor: 0,
        }
    }

    /// Подтягивает новые байты файла и режет их на строки.
    fn refresh(&mut self) -> Result<(), ObserveError> {
        let read_error = |source| ObserveError::Read {
            path: self.path.clone(),
            source,
        };
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            // Файла ещё нет — новых строк тоже.
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(read_error(source)),
        };
        file.seek(SeekFrom::Start(self.offset))
            .map_err(read_error)?;
        let mut fresh = Vec::new();
        file.read_to_end(&mut fresh).map_err(read_error)?;
        self.offset += fresh.len() as u64;
        self.partial_line.push_str(&String::from_utf8_lossy(&fresh));
        while let Some(end) = self.partial_line.find('\n') {
            let line: String = self.partial_line.drain(..=end).collect();
            self.lines
                .push(line.trim_end_matches(['\n', '\r']).to_owned());
        }
        Ok(())
    }

    /// Ждёт сводку с `presented_frames >= at_least`; промежуточные сводки пропускает.
    pub fn wait_for_presented_frames(
        &mut self,
        at_least: u64,
        timeout: Duration,
    ) -> Result<FrameEvidence, ObserveError> {
        let deadline = Instant::now() + timeout;
        let mut last_summary: Option<String> = None;
        loop {
            self.refresh()?;
            while self.cursor < self.lines.len() {
                let line = &self.lines[self.cursor];
                self.cursor += 1;
                if let Some(evidence) = parse_summary(line) {
                    if evidence.presented_frames >= at_least {
                        return Ok(evidence);
                    }
                    last_summary = Some(evidence.summary_line);
                }
            }
            if Instant::now() >= deadline {
                return Err(ObserveError::Timeout {
                    what: format!("a playback summary with presented_frames >= {at_least}"),
                    waited: timeout,
                    last_seen: last_summary,
                });
            }
            std::thread::sleep(POLL_STEP);
        }
    }

    /// Ждёт строку, содержащую `needle`, среди ещё не потреблённых; возвращает её.
    pub fn wait_for_line(
        &mut self,
        needle: &str,
        timeout: Duration,
    ) -> Result<String, ObserveError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.refresh()?;
            while self.cursor < self.lines.len() {
                let line = self.lines[self.cursor].clone();
                self.cursor += 1;
                if line.contains(needle) {
                    return Ok(line);
                }
            }
            if Instant::now() >= deadline {
                return Err(ObserveError::Timeout {
                    what: format!("log line containing {needle:?}"),
                    waited: timeout,
                    last_seen: None,
                });
            }
            std::thread::sleep(POLL_STEP);
        }
    }

    /// Все прочитанные строки, содержащие любую из `needles` (паника, ERROR плеера).
    /// Читает только то, что уже подтянуто; перед проверкой вызовите `snapshot_forbidden`.
    pub fn forbidden_lines(&self, needles: &[&str]) -> Vec<String> {
        self.lines
            .iter()
            .filter(|line| needles.iter().any(|needle| line.contains(needle)))
            .cloned()
            .collect()
    }

    /// Дочитывает файл до конца и возвращает запрещённые строки за ВСЁ время.
    pub fn snapshot_forbidden(&mut self, needles: &[&str]) -> Result<Vec<String>, ObserveError> {
        self.refresh()?;
        Ok(self.forbidden_lines(needles))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const PERIODIC: &str = "2026-11-02T05:20:50.021506Z  INFO Browser playback summary kind=periodic presented_frames=247 taken_frames=250 audio_written_ms=10111 audio_played_ms=9900 audio_output=simulated position_regressions=2 loads=1 position_ms=9900 state=playing";

    #[test]
    fn parses_presented_frames_and_regressions_exactly() {
        let evidence = parse_summary(PERIODIC).expect("a summary line");
        assert_eq!(evidence.presented_frames, 247);
        assert_eq!(evidence.position_regressions, 2);
        assert_eq!(evidence.summary_line, PERIODIC);
    }

    #[test]
    fn taken_frames_is_not_mistaken_for_presented_frames() {
        assert_eq!(
            number_field("x taken_frames=9 presented_frames=5", "frames"),
            None
        );
        assert_eq!(
            number_field("x taken_frames=9 presented_frames=5", "presented_frames"),
            Some(5)
        );
    }

    #[test]
    fn non_summary_lines_are_ignored() {
        assert_eq!(parse_summary("INFO Browser session opened"), None);
        assert_eq!(parse_summary("Browser playback summary kind=final"), None);
    }

    fn append(path: &Path, text: &str) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open log");
        file.write_all(text.as_bytes()).expect("append");
    }

    #[test]
    fn waits_for_a_big_enough_summary_written_later() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("player.log");
        append(&path, &format!("{}\n", PERIODIC.replace("247", "10")));
        let mut log = PlayerLog::open(&path);
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            append(
                &writer_path,
                &format!("{}\n", PERIODIC.replace("247", "300")),
            );
        });
        let evidence = log
            .wait_for_presented_frames(50, Duration::from_secs(5))
            .expect("the later summary satisfies the wait");
        writer.join().expect("writer thread");
        assert_eq!(evidence.presented_frames, 300);
    }

    #[test]
    fn times_out_and_reports_the_last_small_summary() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("player.log");
        append(&path, &format!("{}\n", PERIODIC.replace("247", "7")));
        let mut log = PlayerLog::open(&path);
        let error = log
            .wait_for_presented_frames(50, Duration::from_millis(500))
            .expect_err("only 7 frames were presented");
        let ObserveError::Timeout { last_seen, .. } = error else {
            panic!("expected a timeout, got {error:?}");
        };
        assert!(
            last_seen
                .expect("last summary")
                .contains("presented_frames=7")
        );
    }

    #[test]
    fn half_written_line_is_not_parsed_until_it_ends() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("player.log");
        append(
            &path,
            "INFO Browser playback summary kind=periodic presented_frames=99 position_reg",
        );
        let mut log = PlayerLog::open(&path);
        assert!(
            log.wait_for_line("summary", Duration::from_millis(300))
                .is_err()
        );
        append(&path, "ressions=0\n");
        let line = log
            .wait_for_line("summary", Duration::from_secs(2))
            .expect("the finished line");
        assert!(parse_summary(&line).is_some_and(|e| e.presented_frames == 99));
    }

    #[test]
    fn forbidden_lines_finds_panics_and_errors_only() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("player.log");
        append(
            &path,
            "INFO fine\nERROR renderer lost\nthread 'main' panicked at x\n",
        );
        let mut log = PlayerLog::open(&path);
        let found = log
            .snapshot_forbidden(&["ERROR", "panicked"])
            .expect("readable");
        assert_eq!(
            found,
            vec![
                "ERROR renderer lost".to_owned(),
                "thread 'main' panicked at x".to_owned()
            ]
        );
    }
}
