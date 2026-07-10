use std::sync::Arc;
use std::thread::{self, JoinHandle};

use source_core::{
    ByteSource, CancellationToken, Seekability, SourceError, SourceFingerprint, SourceResult,
    SourceValidators,
};

use crate::buffer::PrefetchBufferState;
use crate::config::PrefetchConfig;
use crate::shared::{PrefetchDiagnostics, PrefetchShared};
use crate::worker::PrefetchWorker;

/// `ByteSource`, который отдаёт foreground-чтение из RAM, пока worker читает inner source наперёд.
pub struct PrefetchingByteSource {
    /// Shared RAM-window и lifecycle state для foreground/worker обмена.
    shared: Arc<PrefetchShared>,

    /// Token, которым `Drop` будит worker wait-loop; активный fetch отменяется отдельным token-ом.
    worker_cancellation: CancellationToken,

    /// JoinHandle хранится в Option, чтобы `Drop` мог забрать его ровно один раз.
    worker_handle: Option<JoinHandle<()>>,

    /// Seekability snapshot, снятый до передачи inner source-а worker thread-у.
    seekability: Seekability,

    /// Validators snapshot, снятый до передачи inner source-а worker thread-у.
    validators: SourceValidators,

    /// Content length snapshot, снятый до передачи inner source-а worker thread-у.
    content_length: Option<u64>,

    /// Fingerprint snapshot, снятый до передачи inner source-а worker thread-у.
    fingerprint: SourceFingerprint,

    /// Foreground logical position; обновляется синхронно с `read` и `seek`.
    logical_position: u64,
}

impl PrefetchingByteSource {
    /// Забирает inner source во владение worker thread-а и готовит RAM-window с текущей позиции.
    #[must_use]
    pub fn new(inner: Box<dyn ByteSource>, config: PrefetchConfig) -> Self {
        let start_position = inner.position();
        let seekability = inner.seekability();
        let validators = inner.validators();
        let content_length = inner.content_length();
        let fingerprint = inner.fingerprint();
        let worker_cancellation = CancellationToken::new();
        let buffer =
            PrefetchBufferState::new(start_position, config.window_bytes(), config.chunk_bytes());
        let shared = Arc::new(PrefetchShared::new(buffer));
        let worker = PrefetchWorker::new(
            inner,
            Arc::clone(&shared),
            worker_cancellation.clone(),
            config,
            seekability.clone(),
        );
        let worker_handle = thread::Builder::new()
            .name("media-prefetch".to_owned())
            .spawn(move || worker.run())
            .expect("не удалось запустить media-prefetch worker thread");

        Self {
            shared,
            worker_cancellation,
            worker_handle: Some(worker_handle),
            seekability,
            validators,
            content_length,
            fingerprint,
            logical_position: start_position,
        }
    }

    /// Возвращает атомарный snapshot counters без доступа к inner source-у.
    #[must_use]
    pub fn diagnostics(&self) -> PrefetchDiagnostics {
        self.shared.lock_state().diagnostics
    }
}

impl ByteSource for PrefetchingByteSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        if output.is_empty() {
            return Ok(0);
        }

        let mut state = self.shared.lock_state();

        loop {
            if cancellation.is_cancelled() {
                return Err(SourceError::Cancelled);
            }

            if state.shutdown {
                return Err(SourceError::Cancelled);
            }

            if let Some(error) = state.fatal_error.take() {
                self.shared.notify_all();
                return Err(error);
            }

            if state.buffer.available_from_cursor() > 0 {
                let bytes_copied = state.buffer.copy_to(output);
                self.logical_position = self.logical_position.saturating_add(bytes_copied as u64);
                self.shared.notify_all();
                return Ok(bytes_copied);
            }

            if state.buffer.is_eof_at_cursor() {
                return Ok(0);
            }

            state.diagnostics.foreground_waits =
                state.diagnostics.foreground_waits.saturating_add(1);
            state = self.shared.wait_state_with_cancellation_poll(state);
        }
    }

    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        let mut state = self.shared.lock_state();
        let buffered_end = state.buffer.buffered_end();
        let offset_is_buffered = state.buffer.contains(offset) || offset == buffered_end;

        if offset_is_buffered {
            state.buffer.set_cursor_within(offset);
            self.logical_position = offset;
            tracing::debug!(
                offset,
                buffered_end,
                "media prefetch foreground seek остался внутри RAM window"
            );
            self.shared.notify_all();
            return Ok(());
        }

        if let Seekability::NotSeekable { reason } = &self.seekability {
            if offset != self.logical_position {
                return Err(SourceError::NotSeekable {
                    reason: reason.clone(),
                });
            }

            return Ok(());
        }

        state.buffer.reset_to(offset);
        state.seek_request = Some(offset);
        state.fatal_error = None;
        // Отмена под тем же mutex-ом, что и `seek_request`, делает порядок видимым worker-у:
        // когда `inner.read` вернёт Cancelled, новый offset уже лежит в shared state.
        if let Some(fetch_cancellation) = &state.fetch_cancellation {
            fetch_cancellation.cancel();
        }
        state.diagnostics.refetches = state.diagnostics.refetches.saturating_add(1);
        self.logical_position = offset;
        tracing::debug!(
            offset,
            previous_buffered_end = buffered_end,
            refetches = state.diagnostics.refetches,
            "media prefetch foreground seek запросил новое RAM window"
        );
        self.shared.notify_all();
        Ok(())
    }

    fn position(&self) -> u64 {
        self.logical_position
    }

    fn seekability(&self) -> Seekability {
        self.seekability.clone()
    }

    fn validators(&self) -> SourceValidators {
        self.validators.clone()
    }

    fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    fn fingerprint(&self) -> SourceFingerprint {
        self.fingerprint.clone()
    }
}

impl Drop for PrefetchingByteSource {
    fn drop(&mut self) {
        {
            let mut state = self.shared.lock_state();
            state.shutdown = true;
            // Worker больше не передаёт shutdown-token в `inner.read`, поэтому Drop обязан
            // отдельно отменить token текущего fetch-а перед join-ом.
            if let Some(fetch_cancellation) = &state.fetch_cancellation {
                fetch_cancellation.cancel();
            }
        }

        self.worker_cancellation.cancel();
        self.shared.notify_all();

        if let Some(worker_handle) = self.worker_handle.take()
            && let Err(panic_payload) = worker_handle.join()
        {
            let panic_message = panic_payload
                .downcast_ref::<&'static str>()
                .copied()
                .or_else(|| panic_payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("panic payload не является строкой");
            tracing::error!(
                panic_message,
                "media-prefetch worker thread завершился panic-ом"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use source_core::{NotSeekableReason, SourceFingerprint};

    use super::*;

    /// Частота проверки test cancellation во время искусственно медленного fake read-а.
    const FAKE_READ_CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(5);

    /// Один вызов inner.read, сохранённый fake source-ом для проверки chunk policy.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ReadRecord {
        /// Absolute offset inner source-а на момент вызова.
        offset: u64,

        /// Размер output buffer, который запросил prefetch worker.
        requested_len: usize,

        /// Сколько bytes fake source реально вернул worker-у.
        returned_len: usize,
    }

    /// Shared state fake source-а, который остаётся доступен тесту через handle.
    #[derive(Debug)]
    struct FakeInnerState {
        /// Все bytes, которые fake source отдаёт последовательно.
        bytes: Vec<u8>,

        /// Текущий cursor fake source-а.
        position: u64,

        /// Seekability, которую fake source сообщает wrapper-у.
        seekability: Seekability,

        /// Искусственная задержка перед read для проверки Drop/cancellation.
        read_delay: Duration,

        /// Если true, read ждёт worker cancellation и имитирует зависший network read.
        wait_until_cancelled: bool,

        /// Номер read-вызова, на котором fake source вернёт ошибку.
        fail_on_read_call: Option<usize>,

        /// История read-вызовов.
        read_records: Vec<ReadRecord>,

        /// История seek-вызовов.
        seek_offsets: Vec<u64>,
    }

    /// Клон, через который тест наблюдает за fake source-ом после передачи в worker.
    #[derive(Clone)]
    struct FakeByteSourceHandle {
        /// Shared state fake source-а.
        state: Arc<Mutex<FakeInnerState>>,
    }

    impl FakeByteSourceHandle {
        /// Возвращает snapshot read history.
        fn read_records(&self) -> Vec<ReadRecord> {
            self.state
                .lock()
                .expect("fake state mutex не должен быть poisoned")
                .read_records
                .clone()
        }

        /// Возвращает snapshot seek history.
        fn seek_offsets(&self) -> Vec<u64> {
            self.state
                .lock()
                .expect("fake state mutex не должен быть poisoned")
                .seek_offsets
                .clone()
        }
    }

    /// Fake ByteSource поверх Vec<u8>, который worker получает во владение.
    struct FakeByteSource {
        /// Shared state нужен только тестам; production source таким образом не шарится.
        state: Arc<Mutex<FakeInnerState>>,
    }

    impl FakeByteSource {
        /// Создаёт seekable fake source без задержек и ошибок.
        fn seekable(bytes: Vec<u8>) -> (Self, FakeByteSourceHandle) {
            Self::new(bytes, Seekability::Seekable)
        }

        /// Создаёт fake source с заданной seekability.
        fn new(bytes: Vec<u8>, seekability: Seekability) -> (Self, FakeByteSourceHandle) {
            let state = Arc::new(Mutex::new(FakeInnerState {
                bytes,
                position: 0,
                seekability,
                read_delay: Duration::ZERO,
                wait_until_cancelled: false,
                fail_on_read_call: None,
                read_records: Vec::new(),
                seek_offsets: Vec::new(),
            }));

            (
                Self {
                    state: Arc::clone(&state),
                },
                FakeByteSourceHandle { state },
            )
        }

        /// Настраивает искусственную задержку read-а.
        fn with_read_delay(self, read_delay: Duration) -> Self {
            self.state
                .lock()
                .expect("fake state mutex не должен быть poisoned")
                .read_delay = read_delay;
            self
        }

        /// Настраивает read, который выходит только после cancellation.
        fn with_wait_until_cancelled(self) -> Self {
            self.state
                .lock()
                .expect("fake state mutex не должен быть poisoned")
                .wait_until_cancelled = true;
            self
        }

        /// Настраивает номер read-вызова, который должен вернуть ошибку.
        fn with_fail_on_read_call(self, read_call: usize) -> Self {
            self.state
                .lock()
                .expect("fake state mutex не должен быть poisoned")
                .fail_on_read_call = Some(read_call);
            self
        }
    }

    /// Записывает старт read-а до блокирующей задержки, чтобы тест мог увидеть in-flight fetch.
    fn record_started_read(
        state: &mut FakeInnerState,
        requested_len: usize,
    ) -> (usize, usize, u64) {
        let call_index = state.read_records.len() + 1;
        let record_index = state.read_records.len();
        let offset = state.position;
        state.read_records.push(ReadRecord {
            offset,
            requested_len,
            returned_len: 0,
        });

        (call_index, record_index, offset)
    }

    /// Ждёт искусственную задержку маленькими шагами, чтобы fetch token мог прервать read.
    fn wait_for_cancellable_read_delay(
        read_delay: Duration,
        cancellation: &CancellationToken,
    ) -> SourceResult<()> {
        let deadline = Instant::now() + read_delay;

        while Instant::now() < deadline {
            if cancellation.is_cancelled() {
                return Err(SourceError::Cancelled);
            }

            let remaining_delay = deadline.saturating_duration_since(Instant::now());
            thread::sleep(remaining_delay.min(FAKE_READ_CANCELLATION_POLL_INTERVAL));
        }

        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        Ok(())
    }

    /// Имитирует зависшее network read, которое завершается только после fetch cancellation.
    fn wait_until_fake_read_cancelled(cancellation: &CancellationToken) -> SourceResult<usize> {
        while !cancellation.is_cancelled() {
            thread::sleep(FAKE_READ_CANCELLATION_POLL_INTERVAL);
        }

        Err(SourceError::Cancelled)
    }

    /// Завершает ранее записанный fake read: копирует bytes, двигает cursor или возвращает ошибку.
    fn complete_recorded_read(
        state: &mut FakeInnerState,
        output: &mut [u8],
        call_index: usize,
        record_index: usize,
        offset: u64,
    ) -> SourceResult<usize> {
        if state.fail_on_read_call == Some(call_index) {
            return Err(SourceError::UnexpectedEof {
                offset,
                expected_bytes: output.len(),
                actual_bytes: 0,
            });
        }

        let start = usize::try_from(offset).expect("fake offset должен помещаться в usize");
        let available = state.bytes.len().saturating_sub(start);
        let returned_len = available.min(output.len());

        if returned_len > 0 {
            output[..returned_len].copy_from_slice(&state.bytes[start..start + returned_len]);
        }

        state.position = state.position.saturating_add(returned_len as u64);
        state.read_records[record_index].returned_len = returned_len;

        Ok(returned_len)
    }

    impl ByteSource for FakeByteSource {
        fn read(
            &mut self,
            output: &mut [u8],
            cancellation: &CancellationToken,
        ) -> SourceResult<usize> {
            if cancellation.is_cancelled() {
                return Err(SourceError::Cancelled);
            }

            let mut state = self
                .state
                .lock()
                .expect("fake state mutex не должен быть poisoned");
            let read_delay = state.read_delay;
            let wait_until_cancelled = state.wait_until_cancelled;
            let (call_index, record_index, offset) = record_started_read(&mut state, output.len());

            if read_delay > Duration::ZERO || wait_until_cancelled {
                drop(state);

                wait_for_cancellable_read_delay(read_delay, cancellation)?;

                if wait_until_cancelled {
                    return wait_until_fake_read_cancelled(cancellation);
                }

                let mut state = self
                    .state
                    .lock()
                    .expect("fake state mutex не должен быть poisoned");
                return complete_recorded_read(
                    &mut state,
                    output,
                    call_index,
                    record_index,
                    offset,
                );
            }

            complete_recorded_read(&mut state, output, call_index, record_index, offset)
        }

        fn seek(&mut self, offset: u64) -> SourceResult<()> {
            let mut state = self
                .state
                .lock()
                .expect("fake state mutex не должен быть poisoned");

            if let Seekability::NotSeekable { reason } = &state.seekability
                && offset != state.position
            {
                return Err(SourceError::NotSeekable {
                    reason: reason.clone(),
                });
            }

            state.position = offset;
            state.seek_offsets.push(offset);
            Ok(())
        }

        fn position(&self) -> u64 {
            self.state
                .lock()
                .expect("fake state mutex не должен быть poisoned")
                .position
        }

        fn seekability(&self) -> Seekability {
            self.state
                .lock()
                .expect("fake state mutex не должен быть poisoned")
                .seekability
                .clone()
        }

        fn validators(&self) -> SourceValidators {
            SourceValidators::default()
        }

        fn content_length(&self) -> Option<u64> {
            let state = self
                .state
                .lock()
                .expect("fake state mutex не должен быть poisoned");
            Some(state.bytes.len() as u64)
        }

        fn fingerprint(&self) -> SourceFingerprint {
            SourceFingerprint::new("fake-prefetch-source")
        }
    }

    /// Короткий config делает тесты быстрыми и позволяет явно видеть chunk boundaries.
    fn test_config(
        initial_chunk_bytes: u64,
        chunk_bytes: u64,
        window_bytes: u64,
    ) -> PrefetchConfig {
        PrefetchConfig::new(initial_chunk_bytes, chunk_bytes, window_bytes)
            .expect("test config должен сохранять инварианты prefetch buffer")
    }

    /// Данные с предсказуемым содержимым, где byte равен offset modulo 251.
    fn sample_bytes(len: usize) -> Vec<u8> {
        (0..len).map(|index| (index % 251) as u8).collect()
    }

    /// Token без cancellation для обычных foreground read-ов.
    fn token() -> CancellationToken {
        CancellationToken::never_cancelled()
    }

    /// Ждёт, пока worker сделает минимум ожидаемое число inner.read вызовов.
    fn wait_for_read_count(handle: &FakeByteSourceHandle, expected: usize) {
        let deadline = Instant::now() + Duration::from_millis(500);

        while Instant::now() < deadline {
            if handle.read_records().len() >= expected {
                return;
            }

            thread::sleep(Duration::from_millis(5));
        }

        panic!(
            "worker не сделал {expected} read-вызовов, текущая история: {:?}",
            handle.read_records()
        );
    }

    /// Ждёт, пока fake source начнёт read с нужного offset-а.
    fn wait_for_read_offset(
        handle: &FakeByteSourceHandle,
        expected_offset: u64,
        timeout: Duration,
    ) -> ReadRecord {
        let deadline = Instant::now() + timeout;

        while Instant::now() < deadline {
            if let Some(record) = handle
                .read_records()
                .into_iter()
                .find(|record| record.offset == expected_offset)
            {
                return record;
            }

            thread::sleep(Duration::from_millis(5));
        }

        panic!(
            "worker не начал read с offset {expected_offset}, текущая история: {:?}",
            handle.read_records()
        );
    }

    /// Дочитывает source до EOF маленькими foreground буферами.
    fn read_all(source: &mut PrefetchingByteSource, output_len: usize) -> Vec<u8> {
        let mut result = Vec::new();
        let mut output = vec![0; output_len];
        let cancellation = token();

        loop {
            let bytes_read = source
                .read(&mut output, &cancellation)
                .expect("prefetch source должен читаться без ошибок");

            if bytes_read == 0 {
                return result;
            }

            result.extend_from_slice(&output[..bytes_read]);
        }
    }

    #[test]
    fn sequential_read_returns_all_bytes_and_inner_reads_large_chunks() {
        let bytes = sample_bytes(50);
        let (inner, handle) = FakeByteSource::seekable(bytes.clone());
        let mut source = PrefetchingByteSource::new(Box::new(inner), test_config(16, 16, 32));

        let actual = read_all(&mut source, 5);

        assert_eq!(actual, bytes);
        assert!(
            handle
                .read_records()
                .iter()
                .all(|record| record.requested_len == 16)
        );
        assert!(handle.read_records().len() < 50 / 5);
    }

    #[test]
    fn worker_doubles_chunk_len_until_configured_ceiling() {
        let (inner, handle) = FakeByteSource::seekable(sample_bytes(256));
        let _source = PrefetchingByteSource::new(Box::new(inner), test_config(4, 32, 128));

        wait_for_read_count(&handle, 5);

        let requested_lens = handle
            .read_records()
            .into_iter()
            .take(5)
            .map(|record| record.requested_len)
            .collect::<Vec<_>>();

        assert_eq!(requested_lens, vec![4, 8, 16, 32, 32]);
    }

    #[test]
    fn small_window_stops_worker_until_foreground_consumes_bytes() {
        let (inner, handle) = FakeByteSource::seekable(sample_bytes(128));
        let mut source = PrefetchingByteSource::new(Box::new(inner), test_config(8, 8, 16));

        wait_for_read_count(&handle, 2);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(handle.read_records().len(), 2);

        let mut output = [0; 8];
        let bytes_read = source
            .read(&mut output, &token())
            .expect("read должен пройти");
        assert_eq!(bytes_read, 8);
        wait_for_read_count(&handle, 3);
    }

    #[test]
    fn seek_inside_window_reuses_buffer_without_refetching_from_seek_offset() {
        let bytes = sample_bytes(64);
        let (inner, handle) = FakeByteSource::seekable(bytes.clone());
        let mut source = PrefetchingByteSource::new(Box::new(inner), test_config(8, 8, 16));

        wait_for_read_count(&handle, 2);
        source
            .seek(4)
            .expect("seek внутри RAM-window должен пройти");

        let mut output = [0; 4];
        let bytes_read = source
            .read(&mut output, &token())
            .expect("read должен пройти");

        assert_eq!(bytes_read, 4);
        assert_eq!(output, bytes[4..8]);
        assert_eq!(source.diagnostics().refetches, 0);
        assert!(!handle.seek_offsets().contains(&4));
        assert!(
            !handle
                .read_records()
                .iter()
                .any(|record| record.offset == 4)
        );
    }

    #[test]
    fn seek_outside_window_resets_buffer_and_refetches_from_requested_offset() {
        let bytes = sample_bytes(96);
        let (inner, handle) = FakeByteSource::seekable(bytes.clone());
        let mut source = PrefetchingByteSource::new(Box::new(inner), test_config(4, 8, 16));

        wait_for_read_count(&handle, 2);
        source
            .seek(40)
            .expect("seekable source должен принять seek за окно");
        wait_for_read_count(&handle, 3);

        let mut output = [0; 4];
        let bytes_read = source
            .read(&mut output, &token())
            .expect("read должен пройти");

        assert_eq!(bytes_read, 4);
        assert_eq!(output, bytes[40..44]);
        assert_eq!(source.diagnostics().refetches, 1);
        let refetch_record = handle
            .read_records()
            .into_iter()
            .find(|record| record.offset == 40)
            .expect("worker должен refetch-нуть с requested seek offset");
        assert_eq!(refetch_record.requested_len, 4);
    }

    #[test]
    fn seek_interrupts_slow_in_flight_read() {
        let bytes = sample_bytes(192);
        let read_delay = Duration::from_millis(300);
        let seek_offset = 96;
        let (inner, handle) = FakeByteSource::seekable(bytes.clone());
        let inner = inner.with_read_delay(read_delay);
        let mut source = PrefetchingByteSource::new(Box::new(inner), test_config(8, 64, 128));

        wait_for_read_count(&handle, 1);
        let seek_started = Instant::now();
        source
            .seek(seek_offset)
            .expect("seek за окно должен отменить текущий fetch");

        let refetch_record = wait_for_read_offset(
            &handle,
            seek_offset,
            read_delay.saturating_sub(Duration::from_millis(50)),
        );
        assert!(
            seek_started.elapsed() < read_delay,
            "worker должен начать новый fetch до завершения старого read_delay"
        );
        assert_eq!(refetch_record.requested_len, 8);

        let mut output = [0; 8];
        let bytes_read = source
            .read(&mut output, &token())
            .expect("seek-cancel не должен стать foreground ошибкой");

        assert_eq!(bytes_read, 8);
        assert_eq!(
            output,
            bytes[seek_offset as usize..seek_offset as usize + 8]
        );
        assert_eq!(source.diagnostics().cancelled_fetches, 1);
    }

    #[test]
    fn seek_cancel_is_not_reported_as_foreground_error() {
        let bytes = sample_bytes(128);
        let seek_offset = 64;
        let (inner, handle) = FakeByteSource::seekable(bytes.clone());
        let inner = inner.with_read_delay(Duration::from_millis(80));
        let mut source = PrefetchingByteSource::new(Box::new(inner), test_config(8, 32, 64));

        wait_for_read_count(&handle, 1);
        source
            .seek(seek_offset)
            .expect("seek за окно должен пройти");

        let mut output = [0; 8];
        let bytes_read = source
            .read(&mut output, &token())
            .expect("Cancelled от старого fetch-а не должен протечь в foreground read");

        assert_eq!(bytes_read, 8);
        assert_eq!(
            output,
            bytes[seek_offset as usize..seek_offset as usize + 8]
        );
        assert_eq!(source.diagnostics().cancelled_fetches, 1);
    }

    #[test]
    fn non_seekable_source_rejects_backward_seek_outside_window_and_reads_forward() {
        let bytes = sample_bytes(48);
        let seekability = Seekability::NotSeekable {
            reason: NotSeekableReason::Unknown,
        };
        let (inner, handle) = FakeByteSource::new(bytes.clone(), seekability);
        let mut source = PrefetchingByteSource::new(Box::new(inner), test_config(4, 4, 8));
        let mut output = [0; 12];

        wait_for_read_count(&handle, 2);
        let bytes_read = source
            .read(&mut output, &token())
            .expect("forward read должен пройти");
        assert_eq!(bytes_read, 8);
        assert_eq!(output[..bytes_read], bytes[..8]);

        let error = source
            .seek(0)
            .expect_err("backward seek за пределами окна должен быть NotSeekable");
        assert!(matches!(error, SourceError::NotSeekable { .. }));
    }

    #[test]
    fn eof_returns_short_tail_and_then_zero() {
        let bytes = sample_bytes(10);
        let (inner, handle) = FakeByteSource::seekable(bytes.clone());
        let mut source = PrefetchingByteSource::new(Box::new(inner), test_config(4, 4, 8));
        let actual = read_all(&mut source, 3);

        assert_eq!(actual, bytes);
        assert!(
            handle
                .read_records()
                .iter()
                .any(|record| record.returned_len == 2)
        );
        assert!(
            handle
                .read_records()
                .iter()
                .any(|record| record.returned_len == 0)
        );
    }

    #[test]
    fn inner_error_is_returned_to_foreground_read() {
        let (inner, _handle) = FakeByteSource::seekable(sample_bytes(32));
        let inner = inner.with_fail_on_read_call(1);
        let mut source = PrefetchingByteSource::new(Box::new(inner), test_config(8, 8, 16));
        let mut output = [0; 4];

        let error = source
            .read(&mut output, &token())
            .expect_err("worker error должен дойти до foreground read");

        assert!(matches!(error, SourceError::UnexpectedEof { .. }));
    }

    #[test]
    fn drop_cancels_slow_inner_read_and_joins_worker() {
        let (inner, handle) = FakeByteSource::seekable(sample_bytes(32));
        let inner = inner
            .with_read_delay(Duration::from_millis(10))
            .with_wait_until_cancelled();
        let source = PrefetchingByteSource::new(Box::new(inner), test_config(8, 8, 16));

        wait_for_read_count(&handle, 1);
        let started = Instant::now();
        drop(source);

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "drop должен завершиться после cancellation текущего fetch token"
        );
    }

    #[test]
    fn metadata_methods_return_constructor_snapshots() {
        let (inner, handle) = FakeByteSource::seekable(sample_bytes(16));
        let source = PrefetchingByteSource::new(Box::new(inner), test_config(8, 8, 16));

        assert_eq!(source.seekability(), Seekability::Seekable);
        assert_eq!(source.content_length(), Some(16));
        assert_eq!(source.validators(), SourceValidators::default());
        assert_eq!(source.fingerprint().as_str(), "fake-prefetch-source");
        drop(handle);
    }

    #[test]
    fn foreground_cancellation_interrupts_wait_without_reporting_worker_error() {
        let (inner, _handle) = FakeByteSource::seekable(sample_bytes(32));
        let inner = inner.with_read_delay(Duration::from_millis(80));
        let mut source = PrefetchingByteSource::new(Box::new(inner), test_config(8, 8, 16));
        let cancellation = CancellationToken::new();
        let cancellation_for_thread = cancellation.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancellation_for_thread.cancel();
        });
        let mut output = [0; 4];

        let error = source
            .read(&mut output, &cancellation)
            .expect_err("foreground cancellation должен прервать ожидание данных");

        canceller
            .join()
            .expect("cancellation helper thread не должен panic-овать");
        assert!(matches!(error, SourceError::Cancelled));
    }
}
