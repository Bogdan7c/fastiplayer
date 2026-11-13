//! Журнал плеера: инкрементальное чтение и разбор сводок воспроизведения.
//!
//! Плеер раз в 10 с (и в конце) пишет строку
//! `Browser playback summary kind=periodic|final presented_frames=N … position_regressions=N …`.
//! `presented_frames` — кадры, дошедшие до показа в окне: критерий «видео реально играет».
//!
//! Конец сеанса окно пишет строкой `Browser session end notice shown notice=… cause=… end=…`
//! ([`parse_end_notice`]): по ней стенд узнаёт, ЧТО плеер сказал зрителю и почему.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::ObserveError;

/// Начало строки сводки.
const SUMMARY_MARKER: &str = "Browser playback summary";
/// Начало строки «окно показало сообщение о конце сеанса» (полоса `cleared` — другая строка).
const END_NOTICE_MARKER: &str = "Browser session end notice shown";
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

/// Сообщение о конце сеанса, которое окно плеера показало зрителю (поля строки журнала).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndNotice {
    /// Какое сообщение увидел зритель (`page_took_video_back`, `stream_error`…).
    pub notice: String,
    /// Причина конца сеанса (`page_took_video_back`, `page_stream_error`, `protocol_violation`…).
    pub cause: String,
    /// Чем кончилось доигрывание (`none`, если его не было).
    pub end: String,
    /// Строка журнала целиком.
    pub line: String,
}

/// Две сводки, между которыми показ кадров вырос на нужную величину.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameGrowth {
    /// Сводка-точка отсчёта.
    pub before: FrameEvidence,
    /// Сводка, в которой кадров уже достаточно больше.
    pub after: FrameEvidence,
}

impl FrameGrowth {
    /// На сколько выросло число показанных кадров.
    pub fn frames_gained(&self) -> u64 {
        self.after
            .presented_frames
            .saturating_sub(self.before.presented_frames)
    }
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
pub(super) fn number_field(line: &str, name: &str) -> Option<u64> {
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

/// Сколько раз плеер загружал источник за сеанс (`loads=N` сводки). Сводка накопительная, поэтому
/// для сеанса важен максимум по всем сводкам. `None` — строка не сводка или поля `loads` в ней нет.
pub fn parse_loads(line: &str) -> Option<u64> {
    if !line.contains(SUMMARY_MARKER) {
        return None;
    }
    number_field(line, "loads")
}

/// Достаёт `name=<слово>` (до пробела) из строки журнала; левая граница — как у чисел.
pub(super) fn word_field(line: &str, name: &str) -> Option<String> {
    let pattern = format!("{name}=");
    let (start, _) = line
        .match_indices(&pattern)
        .find(|(index, _)| *index == 0 || line.as_bytes()[index - 1] == b' ')?;
    let word: String = line[start + pattern.len()..]
        .chars()
        .take_while(|symbol| !symbol.is_whitespace())
        .collect();
    (!word.is_empty()).then_some(word)
}

/// Разбор строки `Browser session end notice shown notice=… cause=… end=…`; `None`, если
/// строка не такая или в ней нет одного из трёх полей.
pub fn parse_end_notice(line: &str) -> Option<EndNotice> {
    if !line.contains(END_NOTICE_MARKER) {
        return None;
    }
    Some(EndNotice {
        notice: word_field(line, "notice")?,
        cause: word_field(line, "cause")?,
        end: word_field(line, "end")?,
        line: line.to_owned(),
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

    /// Забывает всё, что в журнале уже есть: следующие `wait_*` увидят только новые строки.
    /// Нужно, когда один плеер переживает несколько страниц и старые сводки не должны
    /// засчитываться новой.
    pub fn skip_history(&mut self) -> Result<(), ObserveError> {
        self.refresh()?;
        self.cursor = self.lines.len();
        Ok(())
    }

    /// Доказывает, что показ кадров ИДЁТ ПРЯМО СЕЙЧАС: берёт первую новую сводку как точку
    /// отсчёта и ждёт следующую, в которой `presented_frames` выросло не меньше чем на
    /// `at_least_more`. Одной сводки мало: она накопительная и могла вобрать кадры,
    /// показанные до события, которое проверяем (перемотка, смена качества, смена поколения).
    pub fn wait_for_frame_growth(
        &mut self,
        at_least_more: u64,
        timeout: Duration,
    ) -> Result<FrameGrowth, ObserveError> {
        let deadline = Instant::now() + timeout;
        let mut baseline: Option<FrameEvidence> = None;
        loop {
            self.refresh()?;
            while self.cursor < self.lines.len() {
                let line = &self.lines[self.cursor];
                self.cursor += 1;
                let Some(evidence) = parse_summary(line) else {
                    continue;
                };
                match &baseline {
                    None => baseline = Some(evidence),
                    Some(first)
                        if evidence.presented_frames
                            >= first.presented_frames.saturating_add(at_least_more) =>
                    {
                        return Ok(FrameGrowth {
                            before: first.clone(),
                            after: evidence,
                        });
                    }
                    Some(_) => {}
                }
            }
            if Instant::now() >= deadline {
                return Err(ObserveError::Timeout {
                    what: format!(
                        "presented_frames to grow by at least {at_least_more} between two summaries"
                    ),
                    waited: timeout,
                    last_seen: baseline.map(|first| first.summary_line),
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

    /// Ждёт среди ещё не потреблённых строк ПЕРВУЮ, содержащую любую из `needles`; возвращает её.
    /// Нужна, когда честных концов несколько и неизвестно, какой выберет продукт.
    pub fn wait_for_any_line(
        &mut self,
        needles: &[&str],
        timeout: Duration,
    ) -> Result<String, ObserveError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.refresh()?;
            while self.cursor < self.lines.len() {
                let line = self.lines[self.cursor].clone();
                self.cursor += 1;
                if needles.iter().any(|needle| line.contains(needle)) {
                    return Ok(line);
                }
            }
            if Instant::now() >= deadline {
                return Err(ObserveError::Timeout {
                    what: format!("log line containing any of {needles:?}"),
                    waited: timeout,
                    last_seen: None,
                });
            }
            std::thread::sleep(POLL_STEP);
        }
    }

    /// Ждёт среди ещё не потреблённых строк сообщение о конце сеанса и возвращает его поля.
    /// Строка с маркером, но без полей — это уже не «не дождались», а сломанный формат журнала.
    pub fn wait_for_end_notice(&mut self, timeout: Duration) -> Result<EndNotice, ObserveError> {
        let line = self.wait_for_line(END_NOTICE_MARKER, timeout)?;
        parse_end_notice(&line).ok_or(ObserveError::MalformedLine {
            what: "session end notice",
            line,
        })
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

    /// Дочитывает файл до конца и возвращает все строки сводок воспроизведения за ВСЁ время
    /// (не расходуя курсор ожидания): по ним отчёт показывает, как шло воспроизведение.
    pub fn snapshot_summary_lines(&mut self) -> Result<Vec<String>, ObserveError> {
        self.refresh()?;
        Ok(self
            .lines
            .iter()
            .filter(|line| line.contains(SUMMARY_MARKER))
            .cloned()
            .collect())
    }

    /// Дочитывает файл до конца и возвращает все строки с подстрокой `needle` за ВСЁ время
    /// (курсор ожидания не трогает): по ним судят события, которые случились не по одному разу.
    pub fn snapshot_lines_containing(&mut self, needle: &str) -> Result<Vec<String>, ObserveError> {
        self.refresh()?;
        Ok(self
            .lines
            .iter()
            .filter(|line| line.contains(needle))
            .cloned()
            .collect())
    }

    /// Дочитывает файл до конца и возвращает В ПОРЯДКЕ ЖУРНАЛА все строки, содержащие любую из
    /// `needles`, за ВСЁ время (курсор ожидания не трогает). Порядок важен, когда судят, что
    /// случилось ДО события, а что ПОСЛЕ него (сводки кадров вокруг смены шкалы эфира).
    pub fn snapshot_lines_containing_any(
        &mut self,
        needles: &[&str],
    ) -> Result<Vec<String>, ObserveError> {
        self.refresh()?;
        Ok(self
            .lines
            .iter()
            .filter(|line| needles.iter().any(|needle| line.contains(needle)))
            .cloned()
            .collect())
    }

    /// Наибольшее `loads=` среди всех сводок воспроизведения за ВСЁ время (курсор ожидания не
    /// трогает). `None` — сводок с этим полем ещё не было.
    pub fn snapshot_max_loads(&mut self) -> Result<Option<u64>, ObserveError> {
        self.refresh()?;
        Ok(self.lines.iter().filter_map(|line| parse_loads(line)).max())
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

    const PAGE_TOOK_BACK: &str = "2026-11-02T05:21:10.101Z  INFO fastiplayer: Browser session end notice shown notice=page_took_video_back cause=page_took_video_back end=none";

    #[test]
    fn end_notice_fields_are_parsed_exactly() {
        let notice = parse_end_notice(PAGE_TOOK_BACK).expect("an end notice line");
        assert_eq!(notice.notice, "page_took_video_back");
        assert_eq!(notice.cause, "page_took_video_back");
        assert_eq!(notice.end, "none");
        let stream = parse_end_notice(
            "INFO Browser session end notice shown notice=stream_error cause=page_stream_error end=none",
        )
        .expect("a stream error line");
        assert_eq!(
            (stream.notice.as_str(), stream.cause.as_str()),
            ("stream_error", "page_stream_error")
        );
    }

    #[test]
    fn end_notice_ignores_other_lines_and_incomplete_ones() {
        assert_eq!(
            parse_end_notice("INFO Browser session end notice cleared"),
            None
        );
        assert_eq!(parse_end_notice(PERIODIC_FOR_NOTICE_TESTS), None);
        // Маркер есть, но поля cause нет: это не сообщение, а мусор.
        assert_eq!(
            parse_end_notice("Browser session end notice shown notice=stream_error end=none"),
            None
        );
        // `end=` не должен находиться внутри другого слова (`weekend=`).
        assert_eq!(
            parse_end_notice("Browser session end notice shown notice=a cause=b weekend=x"),
            None
        );
    }

    const PERIODIC_FOR_NOTICE_TESTS: &str =
        "INFO Browser playback summary kind=periodic presented_frames=1 position_regressions=0";

    #[test]
    fn waiting_for_the_end_notice_skips_older_lines_and_returns_the_fields() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("player.log");
        append(&path, "INFO Browser session opened\n");
        let mut log = PlayerLog::open(&path);
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            append(&writer_path, &format!("{PAGE_TOOK_BACK}\n"));
        });
        let notice = log
            .wait_for_end_notice(Duration::from_secs(5))
            .expect("the notice written later");
        writer.join().expect("writer thread");
        assert_eq!(notice.notice, "page_took_video_back");
    }

    #[test]
    fn waiting_for_the_end_notice_times_out_without_one_and_flags_a_broken_line() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("player.log");
        append(&path, "INFO Browser session opened\n");
        let mut log = PlayerLog::open(&path);
        assert!(matches!(
            log.wait_for_end_notice(Duration::from_millis(400)),
            Err(ObserveError::Timeout { .. })
        ));
        append(&path, "Browser session end notice shown notice=x\n");
        assert!(matches!(
            log.wait_for_end_notice(Duration::from_secs(2)),
            Err(ObserveError::MalformedLine { .. })
        ));
    }

    const PERIODIC: &str = "2026-11-02T05:20:50.021506Z  INFO Browser playback summary kind=periodic presented_frames=247 taken_frames=250 audio_written_ms=10111 audio_played_ms=9900 audio_output=simulated position_regressions=2 loads=1 position_ms=9900 state=playing";

    #[test]
    fn parses_presented_frames_and_regressions_exactly() {
        let evidence = parse_summary(PERIODIC).expect("a summary line");
        assert_eq!(evidence.presented_frames, 247);
        assert_eq!(evidence.position_regressions, 2);
        assert_eq!(evidence.summary_line, PERIODIC);
    }

    #[test]
    fn loads_are_read_from_summaries_only_and_exactly() {
        assert_eq!(parse_loads(PERIODIC), Some(1));
        assert_eq!(
            parse_loads(&PERIODIC.replace("loads=1", "loads=4")),
            Some(4)
        );
        // Не сводка: поле `loads` в чужой строке ничего не значит.
        assert_eq!(parse_loads("INFO something loads=9"), None);
        // Сводка без поля.
        assert_eq!(
            parse_loads("INFO Browser playback summary kind=final presented_frames=1"),
            None
        );
        // Чужое имя с тем же хвостом.
        assert_eq!(
            parse_loads("INFO Browser playback summary kind=final preloads=7"),
            None
        );
    }

    #[test]
    fn the_session_max_of_loads_survives_a_later_smaller_value() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("player.log");
        append(&path, "INFO noise loads=99\n");
        append(
            &path,
            &format!("{}\n", PERIODIC.replace("loads=1", "loads=2")),
        );
        append(
            &path,
            &format!("{}\n", PERIODIC.replace("loads=1", "loads=5")),
        );
        append(
            &path,
            &format!("{}\n", PERIODIC.replace("loads=1", "loads=3")),
        );
        let mut log = PlayerLog::open(&path);
        assert_eq!(log.snapshot_max_loads().expect("readable"), Some(5));
        let empty = dir.path().join("empty.log");
        append(&empty, "INFO nothing here\n");
        assert_eq!(
            PlayerLog::open(&empty)
                .snapshot_max_loads()
                .expect("readable"),
            None
        );
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

    fn summary_with(frames: u64) -> String {
        format!("{}\n", PERIODIC.replace("247", &frames.to_string()))
    }

    #[test]
    fn frame_growth_needs_two_summaries_and_ignores_history() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("player.log");
        // Старая сводка прошлой страницы: огромная, но к новому событию отношения не имеет.
        append(&path, &summary_with(9000));
        let mut log = PlayerLog::open(&path);
        log.skip_history().expect("readable");
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            for frames in [100, 120, 260] {
                std::thread::sleep(Duration::from_millis(250));
                append(&writer_path, &summary_with(frames));
            }
        });
        let growth = log
            .wait_for_frame_growth(100, Duration::from_secs(5))
            .expect("grows by 100 between 100 and 260");
        writer.join().expect("writer thread");
        assert_eq!(growth.before.presented_frames, 100);
        assert_eq!(growth.after.presented_frames, 260);
        assert_eq!(growth.frames_gained(), 160);
    }

    #[test]
    fn frame_growth_times_out_when_the_player_froze() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("player.log");
        append(&path, &summary_with(100));
        append(&path, &summary_with(103));
        let mut log = PlayerLog::open(&path);
        let error = log
            .wait_for_frame_growth(50, Duration::from_millis(400))
            .expect_err("only 3 more frames were presented");
        let ObserveError::Timeout { last_seen, .. } = error else {
            panic!("expected a timeout, got {error:?}");
        };
        assert!(
            last_seen
                .expect("baseline")
                .contains("presented_frames=100")
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
