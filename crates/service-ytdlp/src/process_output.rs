//! Ограниченное чтение process output и structural preflight одного JSON document.
//!
//! Модуль владеет resource-budget invariant candidate/recovery process path-а:
//! оба pipe-а всегда вычитываются конкурентно, `limit + 1` публикует typed
//! overflow, а JSON DOM строится только после bounded structural прохода.

use std::io::{self, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use rustiplayer_config::YtDlpConfig;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::error::YtDlpServiceError;
use crate::process_tree::{OwnedPipe, OwnedPipeReader, spawn_owned_pipe_reader};

/// Отсутствие опубликованного overflow в shared atomic signal.
const OUTPUT_BUDGET_WITHIN_LIMIT: u8 = 0;

/// Код stdout overflow в shared atomic signal.
const STDOUT_BUDGET_EXCEEDED: u8 = 1;

/// Код stderr overflow в shared atomic signal.
const STDERR_BUDGET_EXCEEDED: u8 = 2;

/// Профиль ограничений одного single-item candidate/recovery process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct YtDlpProcessOutputBudgets {
    /// Максимальное число bytes в stdout.
    stdout_bytes: u64,

    /// Максимальное число bytes в stderr.
    stderr_bytes: u64,

    /// Максимальное число JSON values до построения DOM.
    json_nodes: usize,
}

impl YtDlpProcessOutputBudgets {
    /// Создаёт budget profile только из полностью validated YtDlp config.
    pub(crate) fn from_config(config: &YtDlpConfig) -> Result<Self, YtDlpServiceError> {
        config.validate().map_err(YtDlpServiceError::process)?;
        Self::from_values(
            config.single_item_stdout_limit_bytes,
            config.single_item_stderr_limit_bytes,
            config.single_item_json_node_limit,
        )
    }

    /// Создаёт explicit profile для focused boundary tests.
    #[cfg(test)]
    pub(crate) fn new(
        stdout_bytes: u64,
        stderr_bytes: u64,
        json_nodes: u64,
    ) -> Result<Self, YtDlpServiceError> {
        Self::from_values(stdout_bytes, stderr_bytes, json_nodes)
    }

    /// Сохраняет checked conversions рядом с типом-владельцем budget invariant-а.
    fn from_values(
        stdout_bytes: u64,
        stderr_bytes: u64,
        json_nodes: u64,
    ) -> Result<Self, YtDlpServiceError> {
        if stdout_bytes == 0 {
            return Err(YtDlpServiceError::process(anyhow::anyhow!(
                "yt_dlp.single_item_stdout_limit_bytes должен быть положительным"
            )));
        }
        if stderr_bytes == 0 {
            return Err(YtDlpServiceError::process(anyhow::anyhow!(
                "yt_dlp.single_item_stderr_limit_bytes должен быть положительным"
            )));
        }
        if json_nodes == 0 {
            return Err(YtDlpServiceError::process(anyhow::anyhow!(
                "yt_dlp.single_item_json_node_limit должен быть положительным"
            )));
        }
        let json_nodes = usize::try_from(json_nodes).map_err(|_| {
            YtDlpServiceError::process(anyhow::anyhow!(
                "yt_dlp.single_item_json_node_limit не помещается в usize"
            ))
        })?;

        Ok(Self {
            stdout_bytes,
            stderr_bytes,
            json_nodes,
        })
    }

    /// Возвращает stdout byte budget для typed diagnostic-а.
    pub(crate) const fn stdout_bytes(self) -> u64 {
        self.stdout_bytes
    }

    /// Возвращает stderr byte budget для typed diagnostic-а.
    pub(crate) const fn stderr_bytes(self) -> u64 {
        self.stderr_bytes
    }

    /// Возвращает JSON node budget для structural preflight-а.
    pub(crate) const fn json_nodes(self) -> usize {
        self.json_nodes
    }
}

/// Поток, первым пересёкший свой независимый byte budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessOutputStream {
    /// Стандартный поток данных candidate process-а.
    Stdout,

    /// Стандартный диагностический поток candidate process-а.
    Stderr,
}

impl ProcessOutputStream {
    /// Строит public typed error и не раскрывает содержимое process output.
    pub(crate) const fn into_error(self, budgets: YtDlpProcessOutputBudgets) -> YtDlpServiceError {
        match self {
            Self::Stdout => YtDlpServiceError::StdoutLimitExceeded {
                limit_bytes: budgets.stdout_bytes(),
            },
            Self::Stderr => YtDlpServiceError::StderrLimitExceeded {
                limit_bytes: budgets.stderr_bytes(),
            },
        }
    }
}

/// First-writer-wins сигнал между обоими readers и process wait owner-ом.
#[derive(Debug, Clone)]
pub(crate) struct ProcessOutputBudgetSignal {
    state: Arc<AtomicU8>,
}

impl ProcessOutputBudgetSignal {
    /// Создаёт signal без опубликованного overflow.
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(OUTPUT_BUDGET_WITHIN_LIMIT)),
        }
    }

    /// Публикует первый overflow; последующие не стирают первичную причину.
    fn publish(&self, stream: ProcessOutputStream) {
        let stream_code = match stream {
            ProcessOutputStream::Stdout => STDOUT_BUDGET_EXCEEDED,
            ProcessOutputStream::Stderr => STDERR_BUDGET_EXCEEDED,
        };
        let _ = self.state.compare_exchange(
            OUTPUT_BUDGET_WITHIN_LIMIT,
            stream_code,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// Возвращает первую опубликованную stream identity.
    pub(crate) fn load(&self) -> Option<ProcessOutputStream> {
        match self.state.load(Ordering::Acquire) {
            OUTPUT_BUDGET_WITHIN_LIMIT => None,
            STDOUT_BUDGET_EXCEEDED => Some(ProcessOutputStream::Stdout),
            STDERR_BUDGET_EXCEEDED => Some(ProcessOutputStream::Stderr),
            _ => unreachable!("process output budget signal хранит только известные коды"),
        }
    }
}

/// Запускает bounded stdout reader, сохраняющий только допустимый payload.
pub(crate) fn spawn_stdout_reader<R>(
    pipe: R,
    budget_bytes: u64,
    budget_signal: ProcessOutputBudgetSignal,
) -> Result<OwnedPipeReader<Vec<u8>>, YtDlpServiceError>
where
    R: OwnedPipe,
{
    spawn_owned_pipe_reader("yt-dlp-stdout", pipe, move |reader| {
        read_bounded_stdout(reader, budget_bytes, &budget_signal)
    })
    .map_err(YtDlpServiceError::process)
}

/// Запускает bounded stderr reader, который считает bytes без хранения payload.
pub(crate) fn spawn_stderr_reader<R>(
    pipe: R,
    budget_bytes: u64,
    budget_signal: ProcessOutputBudgetSignal,
) -> Result<OwnedPipeReader<usize>, YtDlpServiceError>
where
    R: OwnedPipe,
{
    spawn_owned_pipe_reader("yt-dlp-stderr", pipe, move |reader| {
        count_bounded_stderr(reader, budget_bytes, &budget_signal)
    })
    .map_err(YtDlpServiceError::process)
}

/// Читает не более `limit + 1`, чтобы различать exact boundary и overflow.
fn read_bounded_stdout(
    reader: &mut dyn Read,
    budget_bytes: u64,
    budget_signal: &ProcessOutputBudgetSignal,
) -> io::Result<Vec<u8>> {
    let probe_bytes = budget_bytes.saturating_add(1);
    let mut bounded_reader = reader.take(probe_bytes);
    let mut captured_bytes = Vec::new();
    bounded_reader.read_to_end(&mut captured_bytes)?;
    if u64::try_from(captured_bytes.len()).unwrap_or(u64::MAX) > budget_bytes {
        budget_signal.publish(ProcessOutputStream::Stdout);
        captured_bytes.truncate(usize::try_from(budget_bytes).unwrap_or(usize::MAX));
    }
    Ok(captured_bytes)
}

/// Считает stderr до `limit + 1`, не удерживая диагностический payload в памяти.
fn count_bounded_stderr(
    reader: &mut dyn Read,
    budget_bytes: u64,
    budget_signal: &ProcessOutputBudgetSignal,
) -> io::Result<usize> {
    let probe_bytes = budget_bytes.saturating_add(1);
    let mut bounded_reader = reader.take(probe_bytes);
    let mut read_buffer = [0_u8; 8 * 1024];
    let mut observed_bytes = 0_u64;
    loop {
        let read_bytes = bounded_reader.read(&mut read_buffer)?;
        if read_bytes == 0 {
            break;
        }
        observed_bytes = observed_bytes.saturating_add(read_bytes as u64);
    }
    if observed_bytes > budget_bytes {
        budget_signal.publish(ProcessOutputStream::Stderr);
    }
    Ok(usize::try_from(observed_bytes.min(budget_bytes)).unwrap_or(usize::MAX))
}

/// Проверяет structural JSON budget без materialization промежуточного DOM.
pub(crate) fn validate_json_node_budget(
    stdout_bytes: &[u8],
    budgets: YtDlpProcessOutputBudgets,
) -> Result<(), YtDlpServiceError> {
    let mut node_counter = JsonNodeCounter::new(budgets.json_nodes());
    let mut deserializer = serde_json::Deserializer::from_slice(stdout_bytes);
    let validation_result = JsonNodeSeed {
        counter: &mut node_counter,
    }
    .deserialize(&mut deserializer)
    .and_then(|()| deserializer.end());

    if node_counter.exceeded {
        return Err(YtDlpServiceError::JsonNodeLimitExceeded {
            limit_nodes: u64::try_from(budgets.json_nodes()).unwrap_or(u64::MAX),
        });
    }
    validation_result.map_err(YtDlpServiceError::invalid_response)
}

/// Mutable structural counter одного JSON document.
struct JsonNodeCounter {
    remaining_nodes: usize,
    exceeded: bool,
}

impl JsonNodeCounter {
    /// Создаёт counter с точным maximum number of values.
    const fn new(maximum_nodes: usize) -> Self {
        Self {
            remaining_nodes: maximum_nodes,
            exceeded: false,
        }
    }

    /// Резервирует один JSON value либо публикует deterministic parser error.
    fn consume_node<E>(&mut self) -> Result<(), E>
    where
        E: de::Error,
    {
        if self.remaining_nodes == 0 {
            self.exceeded = true;
            return Err(E::custom("yt-dlp JSON node budget exceeded"));
        }
        self.remaining_nodes -= 1;
        Ok(())
    }
}

/// Recursive seed, считающий каждый JSON value до его посещения.
struct JsonNodeSeed<'counter> {
    counter: &'counter mut JsonNodeCounter,
}

impl<'de> DeserializeSeed<'de> for JsonNodeSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        self.counter.consume_node::<D::Error>()?;
        deserializer.deserialize_any(JsonNodeVisitor {
            counter: self.counter,
        })
    }
}

/// Allocation-free visitor для JSON primitives и recursive containers.
struct JsonNodeVisitor<'counter> {
    counter: &'counter mut JsonNodeCounter,
}

impl<'de> Visitor<'de> for JsonNodeVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("один JSON value в пределах configured node budget")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_char<E>(self, _value: char) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        JsonNodeSeed {
            counter: self.counter,
        }
        .deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence
            .next_element_seed(JsonNodeSeed {
                counter: self.counter,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_key::<IgnoredAny>()?.is_some() {
            map.next_value_seed(JsonNodeSeed {
                counter: self.counter,
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_node_budget_accepts_exact_boundary() {
        let budgets = YtDlpProcessOutputBudgets::new(1024, 1024, 4).expect("valid budgets");

        validate_json_node_budget(br#"{"items":[1,2]}"#, budgets)
            .expect("root object, array и два числа равны четырём nodes");
    }

    #[test]
    fn json_node_budget_rejects_limit_plus_one() {
        let budgets = YtDlpProcessOutputBudgets::new(1024, 1024, 3).expect("valid budgets");

        let error = validate_json_node_budget(br#"{"items":[1,2]}"#, budgets)
            .expect_err("четвёртый node должен пересечь budget три");

        assert!(matches!(
            error,
            YtDlpServiceError::JsonNodeLimitExceeded { limit_nodes: 3 }
        ));
    }

    #[test]
    fn invalid_json_remains_invalid_response_below_node_budget() {
        let budgets = YtDlpProcessOutputBudgets::new(1024, 1024, 32).expect("valid budgets");

        let error = validate_json_node_budget(br#"{"items":[1,}"#, budgets)
            .expect_err("syntax error не должен превращаться в resource overflow");

        assert!(matches!(
            error,
            YtDlpServiceError::InvalidExtractorResponse { .. }
        ));
    }
}
