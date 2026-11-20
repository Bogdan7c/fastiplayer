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
    /// Сколько раз страница прогона подтвердила каждую команду (`ack <команда>`). Счётчик
    /// живёт отдельно от журнала: `take_logs` журнал забирает, а подтверждения не забываются.
    acks: HashMap<RunId, HashMap<String, usize>>,
}

/// Команда, которую подтверждает строка журнала страницы: `ack <команда>` в начале строки
/// или после метки страницы (`mse-fmp4: ack expect-paused`). Команда — весь остаток строки.
fn acknowledged_command(line: &str) -> Option<&str> {
    const ACK: &str = "ack ";
    let text = match line.split_once(": ") {
        Some((_, rest)) if rest.starts_with(ACK) => rest,
        _ => line,
    };
    text.strip_prefix(ACK).map(str::trim_end)
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
        if let Some(command) = acknowledged_command(&entry.line) {
            *state
                .acks
                .entry(entry.run.clone())
                .or_default()
                .entry(command.to_owned())
                .or_default() += 1;
            self.changed.notify_all();
        }
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

    /// Ждёт подтверждение ПОСЛЕДНЕЙ отправки команды `command`: страница подтвердила её
    /// столько раз, сколько робот её отправил. Одна и та же команда приходит за прогон не раз
    /// (`expect-paused` перед каждой перемоткой), и старое `ack` прошлой отправки новое не
    /// заменяет (раньше это маскировало 10-секундное ожидание сводки плеера, сессия 16b3c2).
    /// Команда сравнивается целиком: `ack expect-seek:13.24` не подтверждает `expect-seek:1`.
    pub(super) fn wait_command_ack(
        &self,
        run: &RunId,
        command: &str,
        timeout: Duration,
    ) -> Result<(), SiteError> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            let sent = state
                .commands
                .get(run)
                .map_or(0, |all| all.iter().filter(|sent| *sent == command).count());
            let acknowledged = state
                .acks
                .get(run)
                .and_then(|acks| acks.get(command))
                .copied()
                .unwrap_or(0);
            // Ни одной отправки: ждём хотя бы одно подтверждение (команду мог дать не робот).
            if acknowledged >= sent.max(1) {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(SiteError::LogLineTimeout {
                    run: run.clone(),
                    needle: format!("ack {command} (#{})", sent.max(1)),
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

#[cfg(test)]
mod ack_tests {
    use super::*;

    const SHORT: Duration = Duration::from_millis(50);

    fn run() -> RunId {
        RunId::new("ack-run-1").expect("run id")
    }

    fn page_line(store: &ReportStore, line: &str) {
        store.put_log(BrowserLogLine {
            source: LogSource::Page,
            run: run(),
            line: line.to_owned(),
        });
    }

    #[test]
    fn the_ack_of_a_command_is_found_after_it_was_sent() {
        let store = ReportStore::default();
        store.push_command(&run(), "expect-paused");
        assert!(
            store
                .wait_command_ack(&run(), "expect-paused", SHORT)
                .is_err()
        );
        page_line(&store, "mse-fmp4: ack expect-paused");
        assert!(
            store
                .wait_command_ack(&run(), "expect-paused", SHORT)
                .is_ok()
        );
    }

    #[test]
    fn an_old_ack_does_not_confirm_the_same_command_sent_again() {
        let store = ReportStore::default();
        store.push_command(&run(), "expect-paused");
        page_line(&store, "mse-fmp4: ack expect-paused");
        store.push_command(&run(), "expect-paused");
        assert!(
            store
                .wait_command_ack(&run(), "expect-paused", SHORT)
                .is_err(),
            "the first ack must not answer the second send"
        );
        page_line(&store, "mse-fmp4: ack expect-paused");
        assert!(
            store
                .wait_command_ack(&run(), "expect-paused", SHORT)
                .is_ok()
        );
    }

    #[test]
    fn acks_are_counted_even_after_the_log_was_taken() {
        let store = ReportStore::default();
        store.push_command(&run(), "handed-off");
        page_line(&store, "mse-fmp4: ack handed-off");
        assert_eq!(store.take_logs(&run()).len(), 1);
        assert!(store.wait_command_ack(&run(), "handed-off", SHORT).is_ok());
    }

    #[test]
    fn a_command_is_compared_whole_not_as_a_prefix() {
        let store = ReportStore::default();
        store.push_command(&run(), "expect-seek:1");
        page_line(&store, "mse-fmp4: ack expect-seek:13.24");
        assert!(
            store
                .wait_command_ack(&run(), "expect-seek:1", SHORT)
                .is_err()
        );
    }

    #[test]
    fn ack_lines_with_and_without_a_page_label_are_read() {
        assert_eq!(acknowledged_command("ack finish"), Some("finish"));
        assert_eq!(
            acknowledged_command("auto-film: ack hook:mark-window"),
            Some("hook:mark-window")
        );
        assert_eq!(acknowledged_command("mse-fmp4: check ok ack-like"), None);
        assert_eq!(acknowledged_command("mse-fmp4: handoff-ready"), None);
    }
}
