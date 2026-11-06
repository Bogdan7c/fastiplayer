//! Хранилище отчётов страниц и журнала из браузера. Владеет данными и синхронизацией ожидания.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::error::SiteError;
use super::run_id::RunId;

/// Предел строк журнала на один прогон: страница в цикле не должна съесть память стенда.
const MAX_LOG_LINES_PER_RUN: usize = 10_000;

/// Итог страницы. `Unsupported` — честное «в этом браузере такой формы не существует».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageStatus {
    Pass,
    Fail,
    Unsupported,
}

/// Одна проверка внутри отчёта.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportCheck {
    pub name: String,
    pub ok: bool,
    #[serde(default)]
    pub detail: String,
}

/// Отчёт страницы (`POST /bench/report`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageReport {
    pub run: RunId,
    pub scenario: String,
    pub status: PageStatus,
    #[serde(default)]
    pub checks: Vec<ReportCheck>,
    #[serde(default)]
    pub user_agent: String,
    #[serde(default)]
    pub elapsed_ms: u64,
    /// Именованные числа, которые страница измерила (кадры, секунды, счётчики). Вердикт
    /// сценария сравнивает их с порогами, а не разбирает текст `detail`.
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
}

/// Кто прислал строку журнала.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogSource {
    Extension,
    Page,
}

/// Строка журнала из браузера (`POST /bench/log`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserLogLine {
    pub source: LogSource,
    pub run: RunId,
    pub line: String,
}

#[derive(Default)]
struct StoreState {
    /// Последний присланный отчёт по прогону (повторная отправка страницей заменяет прежний).
    reports: HashMap<RunId, PageReport>,
    logs: HashMap<RunId, Vec<BrowserLogLine>>,
    /// Команды робота странице по прогону, в порядке отправки (страница опрашивает их сама).
    commands: HashMap<RunId, Vec<String>>,
}

/// Потокобезопасное хранилище. `Condvar` будит ожидающих (`wait_report`, `wait_log_line`)
/// сразу при приходе отчёта или строки журнала.
#[derive(Default)]
pub(super) struct ReportStore {
    state: Mutex<StoreState>,
    changed: Condvar,
}

impl ReportStore {
    pub(super) fn put_report(&self, report: PageReport) {
        let mut state = self.lock();
        state.reports.insert(report.run.clone(), report);
        self.changed.notify_all();
    }

    pub(super) fn put_log(&self, entry: BrowserLogLine) {
        let mut state = self.lock();
        let lines = state.logs.entry(entry.run.clone()).or_default();
        // Сверх предела строки молча не теряем: последняя строка предупреждает об обрезке.
        if lines.len() < MAX_LOG_LINES_PER_RUN {
            lines.push(entry);
            self.changed.notify_all();
        } else if lines.len() == MAX_LOG_LINES_PER_RUN {
            let notice = BrowserLogLine {
                line: format!("[bench] log truncated after {MAX_LOG_LINES_PER_RUN} lines"),
                ..entry
            };
            lines.push(notice);
        }
    }

    /// Ждёт отчёт прогона до `timeout`. Отчёт остаётся в хранилище: повторный вызов вернёт его же.
    pub(super) fn wait_report(
        &self,
        run: &RunId,
        timeout: Duration,
    ) -> Result<PageReport, SiteError> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if let Some(report) = state.reports.get(run) {
                return Ok(report.clone());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(SiteError::ReportTimeout {
                    run: run.clone(),
                    waited: timeout,
                });
            }
            state = match self.changed.wait_timeout(state, remaining) {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }

    /// Ждёт строку журнала прогона, содержащую `needle`. Журнал не расходуется: строку
    /// увидит и последующий `take_logs`. Возвращает найденную строку целиком.
    pub(super) fn wait_log_line(
        &self,
        run: &RunId,
        needle: &str,
        timeout: Duration,
    ) -> Result<String, SiteError> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            let found = state
                .logs
                .get(run)
                .and_then(|lines| lines.iter().find(|entry| entry.line.contains(needle)));
            if let Some(entry) = found {
                return Ok(entry.line.clone());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(SiteError::LogLineTimeout {
                    run: run.clone(),
                    needle: needle.to_owned(),
                    waited: timeout,
                });
            }
            state = match self.changed.wait_timeout(state, remaining) {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }

    /// Кладёт команду робота странице прогона.
    pub(super) fn push_command(&self, run: &RunId, command: &str) {
        self.lock()
            .commands
            .entry(run.clone())
            .or_default()
            .push(command.to_owned());
    }

    /// Команды прогона, начиная с номера `seen` (сколько страница уже получила).
    pub(super) fn commands_since(&self, run: &RunId, seen: usize) -> Vec<String> {
        self.lock()
            .commands
            .get(run)
            .map(|all| all.iter().skip(seen).cloned().collect())
            .unwrap_or_default()
    }

    /// Строки журнала с `needle` в порядке прихода; журнал не расходуется.
    pub(super) fn lines_containing(&self, run: &RunId, needle: &str) -> Vec<String> {
        self.lock()
            .logs
            .get(run)
            .map(|lines| {
                lines
                    .iter()
                    .filter(|entry| entry.line.contains(needle))
                    .map(|entry| entry.line.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Забирает (и удаляет) накопленный журнал прогона в порядке прихода.
    pub(super) fn take_logs(&self, run: &RunId) -> Vec<BrowserLogLine> {
        self.lock().logs.remove(run).unwrap_or_default()
    }

    /// Отравленный мьютекс здесь безвреден: данные — простые коллекции, они целы.
    fn lock(&self) -> MutexGuard<'_, StoreState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
