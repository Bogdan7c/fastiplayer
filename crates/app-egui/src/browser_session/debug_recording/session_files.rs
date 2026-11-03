//! Файлы записи одного сеанса: `stream.json`, `NNN.json`/`NNN.bin`, `outgoing.jsonl`.
//!
//! Формат — тот же, что у фикстур `crates/browser-bridge-protocol/fixtures/streams/<name>/`
//! (их читают `pushed-media-source/tests/support/recorded.rs` и тесты процесса), плюс
//! необязательное поле `at_ms` у каждого шага — время прихода от начала записи:
//! ```text
//! stream.json: {"steps":[{"text":"001.json","at_ms":0},{"binary":"002.bin","at_ms":13}, …]}
//! outgoing.jsonl: {"at_ms":5,"message":{…}}   (по строке на сообщение плеера странице)
//! ```
//! Файлы шагов пишутся сразу, `stream.json` — не чаще раза в [`MANIFEST_REFRESH_PERIOD`]
//! (перезапись на каждый кусок была бы квадратичной) и при закрытии соединения; если
//! процесс убьют, запись теряет не больше этого окна шагов.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use thiserror::Error;

/// Как часто переписывать `stream.json` во время записи.
const MANIFEST_REFRESH_PERIOD: Duration = Duration::from_secs(1);

/// Имя манифеста потока (то же, что у фикстур).
const MANIFEST_FILE: &str = "stream.json";

/// Имя журнала исходящих сообщений плеера.
const OUTGOING_FILE: &str = "outgoing.jsonl";

/// Ошибка записи: любая — повод выключить запись целиком (плеер играет дальше).
#[derive(Debug, Error)]
pub(super) enum RecordingError {
    /// Не удалось создать каталог записи.
    #[error("cannot create the recording directory: {0}")]
    CreateDirectory(std::io::Error),
    /// Не удалось записать файл шага, манифест или журнал исходящих.
    #[error("cannot write recording file `{file}`: {source}")]
    WriteFile {
        /// Имя файла внутри каталога записи (без пути).
        file: String,
        /// Исходная ошибка ввода-вывода.
        source: std::io::Error,
    },
    /// Свободный номер каталога не нашёлся (каталог забит чужими записями).
    #[error("no free session directory name under the recording root")]
    NoFreeDirectory,
}

/// Вид шага записи.
#[derive(Clone, Copy)]
enum StepKind {
    /// JSON-сообщение страницы (`NNN.json`).
    Text,
    /// Двоичная рамка (`NNN.bin`).
    Binary,
}

/// Шаг, уже лежащий на диске: что нужно, чтобы описать его в манифесте.
struct StepEntry {
    kind: StepKind,
    file_name: String,
    at_ms: u64,
}

/// Запись одного сеанса.
pub(super) struct SessionFiles {
    directory: PathBuf,
    /// Нулевая точка `at_ms`: создание записи (первое сообщение подключённого сеанса).
    started: Instant,
    steps: Vec<StepEntry>,
    outgoing: File,
    /// Последнее выданное `at_ms`: счёт не убывает, даже если часы дали бы шаг назад.
    last_at_ms: u64,
    manifest_written_at: Option<Instant>,
}

impl SessionFiles {
    /// Создаёт каталог записи `<root>/<label>-<n>` с первым свободным `n` (считая с 1).
    pub(super) fn create(root: &Path, label: &str) -> Result<Self, RecordingError> {
        std::fs::create_dir_all(root).map_err(RecordingError::CreateDirectory)?;
        for number in 1..=MAX_DIRECTORY_NUMBER {
            let directory = root.join(format!("{label}-{number}"));
            match std::fs::create_dir(&directory) {
                Ok(()) => return Self::open_in(directory),
                // Занято записью прошлого процесса: берём следующий номер.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(RecordingError::CreateDirectory(error)),
            }
        }
        Err(RecordingError::NoFreeDirectory)
    }

    fn open_in(directory: PathBuf) -> Result<Self, RecordingError> {
        let outgoing = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(directory.join(OUTGOING_FILE))
            .map_err(|source| RecordingError::WriteFile {
                file: OUTGOING_FILE.to_owned(),
                source,
            })?;
        Ok(Self {
            directory,
            started: Instant::now(),
            steps: Vec::new(),
            outgoing,
            last_at_ms: 0,
            manifest_written_at: None,
        })
    }

    /// Каталог записи (для журнала и тестов).
    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }

    /// Записывает входящее текстовое сообщение страницы как есть.
    pub(super) fn record_page_text(&mut self, bytes: &[u8]) -> Result<(), RecordingError> {
        self.record_step(StepKind::Text, bytes)
    }

    /// Записывает входящую двоичную рамку страницы как есть.
    pub(super) fn record_page_binary(&mut self, bytes: &[u8]) -> Result<(), RecordingError> {
        self.record_step(StepKind::Binary, bytes)
    }

    /// Дописывает строку в `outgoing.jsonl`. `json` — уже проверенный протоколом JSON
    /// сообщения плеера, поэтому вставляется в строку как есть.
    pub(super) fn record_player_message(&mut self, json: &str) -> Result<(), RecordingError> {
        let at_ms = self.next_at_ms();
        // Одна запись = одна строка: JSON сообщения протокола не содержит переводов строк.
        let line = format!("{{\"at_ms\":{at_ms},\"message\":{}}}\n", json.trim());
        self.outgoing
            .write_all(line.as_bytes())
            .map_err(|source| RecordingError::WriteFile {
                file: OUTGOING_FILE.to_owned(),
                source,
            })
    }

    /// Принудительно переписывает `stream.json` (закрытие соединения).
    pub(super) fn flush_manifest(&mut self) -> Result<(), RecordingError> {
        let manifest = render_manifest(&self.steps);
        // Через временный файл и переименование: читатель не увидит половину манифеста.
        let temporary = self.directory.join(format!("{MANIFEST_FILE}.tmp"));
        write_file(&temporary, MANIFEST_FILE, manifest.as_bytes())?;
        std::fs::rename(&temporary, self.directory.join(MANIFEST_FILE)).map_err(|source| {
            RecordingError::WriteFile {
                file: MANIFEST_FILE.to_owned(),
                source,
            }
        })?;
        self.manifest_written_at = Some(Instant::now());
        Ok(())
    }

    fn record_step(&mut self, kind: StepKind, bytes: &[u8]) -> Result<(), RecordingError> {
        let at_ms = self.next_at_ms();
        let extension = match kind {
            StepKind::Text => "json",
            StepKind::Binary => "bin",
        };
        // Нумерация общая для текста и двоичных, с 001, как у фикстур.
        let file_name = format!("{:03}.{extension}", self.steps.len() + 1);
        write_file(&self.directory.join(&file_name), &file_name, bytes)?;
        self.steps.push(StepEntry {
            kind,
            file_name,
            at_ms,
        });
        let refresh_due = self
            .manifest_written_at
            .is_none_or(|written| written.elapsed() >= MANIFEST_REFRESH_PERIOD);
        if refresh_due {
            self.flush_manifest()?;
        }
        Ok(())
    }

    /// Миллисекунды от начала записи, не убывающие.
    fn next_at_ms(&mut self) -> u64 {
        let elapsed = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_at_ms = self.last_at_ms.max(elapsed);
        self.last_at_ms
    }
}

/// Сколько каталогов `<label>-<n>` пробовать, прежде чем сдаться.
const MAX_DIRECTORY_NUMBER: u32 = 100_000;

fn write_file(path: &Path, display_name: &str, bytes: &[u8]) -> Result<(), RecordingError> {
    std::fs::write(path, bytes).map_err(|source| RecordingError::WriteFile {
        file: display_name.to_owned(),
        source,
    })
}

/// Текст `stream.json` по шагам. Имена файлов состоят из цифр и точки — экранировать нечего.
fn render_manifest(steps: &[StepEntry]) -> String {
    let rendered: Vec<String> = steps
        .iter()
        .map(|step| {
            let key = match step.kind {
                StepKind::Text => "text",
                StepKind::Binary => "binary",
            };
            format!(
                "{{\"{key}\":\"{}\",\"at_ms\":{}}}",
                step.file_name, step.at_ms
            )
        })
        .collect();
    format!("{{\"steps\":[{}]}}\n", rendered.join(","))
}
