//! Concurrency regression для seek-а внутрь уже выполняемого prefetch request-а.

use std::sync::mpsc;

use super::*;

/// Источник удерживает первый read до foreground seek-а и затем возвращает ordinary error.
struct ControlledStaleFailureSource {
    /// Bytes для успешного refetch после stale failure.
    bytes: Vec<u8>,

    /// Текущая позиция seekable fake source-а.
    position: u64,

    /// Счётчик отделяет управляемый первый failure от последующих успешных read-ов.
    read_call_count: usize,

    /// Подтверждает тесту, что первый read уже стал active fetch-ом.
    first_read_started_sender: mpsc::SyncSender<()>,

    /// Не даёт первой ordinary error вернуться до публикации foreground seek-а.
    first_read_release_receiver: mpsc::Receiver<()>,
}

impl ControlledStaleFailureSource {
    /// Создаёт source и два однократных synchronization endpoint-а для теста.
    fn new(bytes: Vec<u8>) -> (Self, mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        // Zero-capacity канал доказывает, что worker действительно вошёл в первый read.
        let (first_read_started_sender, first_read_started_receiver) = mpsc::sync_channel(0);
        // Второй zero-capacity канал задаёт точный порядок seek -> stale ordinary error.
        let (first_read_release_sender, first_read_release_receiver) = mpsc::sync_channel(0);
        (
            Self {
                bytes,
                position: 0,
                read_call_count: 0,
                first_read_started_sender,
                first_read_release_receiver,
            },
            first_read_started_receiver,
            first_read_release_sender,
        )
    }
}

impl ByteSource for ControlledStaleFailureSource {
    /// Первый read возвращает управляемую stale error, остальные читают настоящие bytes.
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        // Cancellation до старта read-а сохраняет обычный ByteSource contract.
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        // Номер вызова меняется до handoff-а, чтобы первый read оставался однократным.
        self.read_call_count = self.read_call_count.saturating_add(1);
        if self.read_call_count == 1 {
            // Worker сообщает, что active fetch уже нельзя спутать с будущим refetch-ом.
            self.first_read_started_sender
                .send(())
                .expect("test должен ждать старта первого read-а");
            // Ожидание намеренно игнорирует cancellation и моделирует ordinary I/O failure.
            self.first_read_release_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("test должен освободить stale failure после seek-а");
            return Err(SourceError::UnexpectedEof {
                offset: self.position,
                expected_bytes: output.len(),
                actual_bytes: 0,
            });
        }

        // Refetch читает bytes с позиции, установленной worker-owned seek path.
        let start = usize::try_from(self.position).expect("test offset должен помещаться в usize");
        let returned_len = self.bytes.len().saturating_sub(start).min(output.len());
        output[..returned_len].copy_from_slice(&self.bytes[start..start + returned_len]);
        self.position = self.position.saturating_add(returned_len as u64);
        Ok(returned_len)
    }

    /// Seek переводит fake source к новому worker-owned fetch offset.
    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        self.position = offset;
        Ok(())
    }

    /// Возвращает текущую absolute позицию fake source-а.
    fn position(&self) -> u64 {
        self.position
    }

    /// Сценарий обязан проходить именно seekable reset path.
    fn seekability(&self) -> Seekability {
        Seekability::Seekable
    }

    /// Test source не объявляет HTTP validators.
    fn validators(&self) -> SourceValidators {
        SourceValidators::default()
    }

    /// Полная длина нужна prefetch boundary для EOF/accounting.
    fn content_length(&self) -> Option<u64> {
        Some(self.bytes.len() as u64)
    }

    /// Стабильный fingerprint делает fake полноценным ByteSource boundary double.
    fn fingerprint(&self) -> SourceFingerprint {
        SourceFingerprint::new("controlled-stale-prefetch-source")
    }
}

/// Пустой foreground read является no-op и не двигает source/accounting перед обычным чтением.
#[test]
fn empty_foreground_read_preserves_position_and_followup_bytes() {
    let bytes = sample_bytes(24);
    let (inner, handle) = FakeByteSource::seekable(bytes.clone());
    let mut source = start_test_source(Box::new(inner), test_config(4, 8, 16));
    wait_for_read_count(&handle, 1);

    assert_eq!(source.read(&mut [], &token()).expect("empty read"), 0);
    assert_eq!(source.position(), 0);
    assert_eq!(source.diagnostics().refetches, 0);

    let mut output = [0; 4];
    let bytes_read = source
        .read(&mut output, &token())
        .expect("обычное чтение после no-op должно пройти");
    assert_eq!(bytes_read, output.len());
    assert_eq!(output, bytes[..output.len()]);
}

/// Forward seek переиспользует active range без cancellation и duplicate refetch-а.
#[test]
fn forward_seek_inside_active_fetch_coalesces_without_cancel_or_refetch() {
    let bytes = sample_bytes(96);
    let read_delay = Duration::from_millis(80);
    let (inner, handle) = FakeByteSource::seekable(bytes.clone());
    let inner = inner.with_read_delay(read_delay);
    let mut source = start_test_source(Box::new(inner), test_config(8, 16, 48));

    let active_fetch = wait_for_read_offset(&handle, 8, Duration::from_millis(500));
    assert_eq!(active_fetch.requested_len, 16);
    source
        .seek(20)
        .expect("seek внутрь active fetch должен изменить только logical cursor");

    let mut output = [0_u8; 4];
    let bytes_read = source
        .read(&mut output, &token())
        .expect("foreground read должен дождаться active fetch");

    assert_eq!(bytes_read, output.len());
    assert_eq!(output, bytes[20..24]);
    assert_eq!(source.diagnostics().refetches, 0);
    assert_eq!(source.diagnostics().cancelled_fetches, 0);
    assert!(!handle.seek_offsets().contains(&20));
    assert!(
        !handle
            .read_records()
            .iter()
            .any(|record| record.offset == 20)
    );
}

/// Быстрый seek за окно и возврат в него сохраняют уже загруженные bytes.
///
/// Этот sequence моделирует metadata probe контейнера: запрос EOF нужен только
/// для вычисления длины, после чего parser возвращается к началу до того, как
/// prefetch worker успел применить новый network window.
#[test]
fn superseded_out_of_window_seek_returns_to_buffer_without_duplicate_refetch() {
    let bytes = sample_bytes(192);
    let read_delay = Duration::from_millis(120);
    let (inner, handle) = FakeByteSource::seekable(bytes.clone());
    let inner = inner.with_read_delay(read_delay);
    let mut source = start_test_source(Box::new(inner), test_config(8, 16, 48));

    let active_fetch = wait_for_read_offset(&handle, 8, Duration::from_millis(500));
    assert_eq!(active_fetch.requested_len, 16);

    source
        .seek(160)
        .expect("seek за текущее окно должен стать pending");
    source
        .seek(0)
        .expect("возврат в сохранённое окно должен supersede-нуть pending seek");

    let mut output = [0_u8; 4];
    let bytes_read = source
        .read(&mut output, &token())
        .expect("foreground должен сразу переиспользовать исходное окно");

    assert_eq!(bytes_read, output.len());
    assert_eq!(output, bytes[..output.len()]);
    wait_for_read_count(&handle, 3);
    assert_eq!(source.diagnostics().refetches, 0);
    assert_eq!(source.diagnostics().cancelled_fetches, 1);
    assert_eq!(
        handle
            .read_records()
            .iter()
            .filter(|record| record.offset == 0)
            .count(),
        1,
        "начальный Range не должен скачиваться повторно"
    );
    assert!(
        !handle.seek_offsets().contains(&160),
        "superseded seek не должен достигать inner source"
    );
}

/// Обычная ошибка superseded fetch-а не должна отравить новый foreground read.
#[test]
fn stale_non_cancelled_failure_after_seek_is_discarded_before_refetch() {
    // Данные позволяют второму fetch-у вернуть проверяемые bytes после seek-а.
    let bytes = sample_bytes(192);
    // Offset вне первого восьмибайтового fetch-а обязан поставить pending seek.
    let seek_offset = 96;
    // Управляемый source удерживает первую ordinary error до явного test release.
    let (inner, first_read_started, first_read_release) =
        ControlledStaleFailureSource::new(bytes.clone());
    let mut source = start_test_source(Box::new(inner), test_config(8, 64, 128));

    // Handoff доказывает, что последующий seek точно supersede-ит active fetch.
    first_read_started
        .recv_timeout(Duration::from_secs(1))
        .expect("первый prefetch read должен стартовать до deadline");
    source
        .seek(seek_offset)
        .expect("seek за active fetch должен поставить новое окно");
    // Ошибка возвращается worker-у только после публикации pending seek/cancellation.
    first_read_release
        .send(())
        .expect("worker должен ждать release stale failure");

    // Успешный read доказывает, что stale error отброшена, а refetch дошёл до consumer-а.
    let mut output = [0; 8];
    let bytes_read = source
        .read(&mut output, &token())
        .expect("stale ordinary error не должна стать foreground fatal error");

    // Consumer получает bytes именно из нового seek window.
    assert_eq!(bytes_read, output.len());
    assert_eq!(
        output,
        bytes[seek_offset as usize..seek_offset as usize + output.len()]
    );
    // Worker обязан учесть один отброшенный fetch и один реальный refetch.
    assert_eq!(source.diagnostics().cancelled_fetches, 1);
    assert_eq!(source.diagnostics().refetches, 1);
}

/// Active fetch не расширяет публичную seekability источника.
#[test]
fn non_seekable_source_rejects_forward_seek_inside_active_fetch() {
    let bytes = sample_bytes(96);
    let read_delay = Duration::from_millis(80);
    let seekability = Seekability::NotSeekable {
        reason: NotSeekableReason::Unknown,
    };
    let (inner, handle) = FakeByteSource::new(bytes, seekability);
    let inner = inner.with_read_delay(read_delay);
    let mut source = start_test_source(Box::new(inner), test_config(8, 16, 48));

    let active_fetch = wait_for_read_offset(&handle, 8, Duration::from_millis(500));
    assert_eq!(active_fetch.requested_len, 16);
    let error = source
        .seek(20)
        .expect_err("active fetch не должен обходить NotSeekable contract");

    assert!(matches!(error, SourceError::NotSeekable { .. }));
    assert_eq!(source.position(), 0);
    assert_eq!(source.diagnostics().refetches, 0);
    assert_eq!(source.diagnostics().cancelled_fetches, 0);
}
