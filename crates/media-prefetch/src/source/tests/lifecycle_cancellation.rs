//! Сессия 16: закрытие media прерывает чтение, ждущее восстановления сети.

use super::*;

/// Inner read «висит», как запрос к пропавшей сети. Foreground ждёт данные с вечным
/// токеном (так читает Symphonia). Отмена токена жизни ресурса должна разбудить
/// foreground типизированной `SourceClosed`, а Drop — отменить висящий fetch и
/// завершить worker без зависания.
#[test]
fn lifecycle_cancellation_wakes_blocked_foreground_with_source_closed() {
    let (inner, handle) = FakeByteSource::seekable(sample_bytes(64));
    let inner = inner.with_wait_until_cancelled();
    let lifecycle = CancellationToken::new();
    let mut source = start_test_source(Box::new(inner), test_config(8, 8, 32))
        .with_lifecycle_cancellation(lifecycle.clone());
    wait_for_read_count(&handle, 1);
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        lifecycle.cancel();
    });
    let started = Instant::now();

    let error = source
        .read(&mut [0; 4], &CancellationToken::never_cancelled())
        .expect_err("закрытый ресурс не отдаёт данные");

    assert!(matches!(error, SourceError::SourceClosed), "{error}");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(source.position(), 0, "закрытие не двигает позицию");
    canceller.join().expect("canceller thread");

    let drop_started = Instant::now();
    drop(source);
    assert!(
        drop_started.elapsed() < Duration::from_secs(2),
        "Drop отменяет висящий fetch и не ждёт сеть"
    );
}

/// Без отмены токен жизни ничего не меняет: данные читаются как обычно.
#[test]
fn active_lifecycle_token_does_not_change_reads() {
    let bytes = sample_bytes(40);
    let (inner, _handle) = FakeByteSource::seekable(bytes.clone());
    let mut source = start_test_source(Box::new(inner), test_config(8, 8, 32))
        .with_lifecycle_cancellation(CancellationToken::new());

    assert_eq!(read_all(&mut source, 5), bytes);
}
