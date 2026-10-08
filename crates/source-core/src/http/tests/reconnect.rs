//! Сессия 16: переподключение HTTP Range чтения на настоящем loopback-сервере.

use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use super::*;
use crate::HttpRepresentationChange;

/// 404 после обрыва — мёртвая ссылка: повторов нет, ошибка приходит сразу.
#[test]
fn not_found_is_not_retried() {
    let media = Arc::new(b"abcdefghij".to_vec());
    let media_for_server = Arc::clone(&media);
    let server = TestHttpServer::spawn(move |index, request, stream| {
        if index >= 1 {
            write_response(
                stream,
                "404 Not Found",
                &[("Content-Length", "0".to_string())],
                b"",
            );
            return;
        }
        respond_with_range(stream, &request, &media_for_server);
    });

    let mut source = HttpRangeSource::open(server.config_with_reconnect(Duration::from_secs(30)))
        .expect("source opens");
    let started = Instant::now();
    let error = source
        .read(&mut [0_u8; 4], &CancellationToken::never_cancelled())
        .expect_err("404 не лечится повтором");

    assert!(
        matches!(error, SourceError::HttpStatus { status, .. } if status == StatusCode::NOT_FOUND),
        "{error}"
    );
    assert_eq!(
        server.requests().len(),
        2,
        "probe + один range, без повторов"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(source.range_diagnostics().reconnects, 0);
}

/// 403 (протухшая подпись yt-dlp) тоже не повторяется: им занимается VOD recovery.
#[test]
fn forbidden_is_not_retried() {
    let media = Arc::new(b"abcdefghij".to_vec());
    let media_for_server = Arc::clone(&media);
    let server = TestHttpServer::spawn(move |index, request, stream| {
        if index >= 1 {
            write_response(
                stream,
                "403 Forbidden",
                &[("Content-Length", "0".to_string())],
                b"",
            );
            return;
        }
        respond_with_range(stream, &request, &media_for_server);
    });

    let mut source = HttpRangeSource::open(server.config_with_reconnect(Duration::from_secs(30)))
        .expect("source opens");
    let error = source
        .read(&mut [0_u8; 4], &CancellationToken::never_cancelled())
        .expect_err("403 не лечится повтором");

    assert!(!error.is_transient_network_failure(), "{error}");
    assert_eq!(server.requests().len(), 2);
}

/// `503 Retry-After: 1` — сервер просит подождать секунду, и это дольше нашей
/// первой паузы 0,5 с: второй запрос уходит не раньше чем через секунду.
#[test]
fn service_unavailable_waits_for_retry_after_header() {
    let media = Arc::new(b"abcdefghij".to_vec());
    let media_for_server = Arc::clone(&media);
    let request_times = Arc::new(Mutex::new(Vec::new()));
    let request_times_for_server = Arc::clone(&request_times);
    let server = TestHttpServer::spawn(move |index, request, stream| {
        request_times_for_server
            .lock()
            .expect("request times lock")
            .push(Instant::now());
        if index == 1 {
            write_response(
                stream,
                "503 Service Unavailable",
                &[
                    ("Content-Length", "0".to_string()),
                    ("Retry-After", "1".to_string()),
                ],
                b"",
            );
            return;
        }
        respond_with_range(stream, &request, &media_for_server);
    });

    let mut source = HttpRangeSource::open(server.config_with_reconnect(Duration::from_secs(30)))
        .expect("source opens");
    let mut output = [0_u8; 4];
    let bytes_read = source
        .read(&mut output, &CancellationToken::never_cancelled())
        .expect("после Retry-After сервер отвечает");

    assert_eq!(&output[..bytes_read], b"abcd");
    let request_times = request_times.lock().expect("request times lock").clone();
    assert_eq!(request_times.len(), 3);
    let server_directed_wait = request_times[2].duration_since(request_times[1]);
    assert!(
        server_directed_wait >= Duration::from_secs(1),
        "ожидание {server_directed_wait:?} короче Retry-After"
    );
}

/// Связь не вернулась за бюджет: чтение сдаётся с последней сетевой ошибкой.
#[test]
fn outage_longer_than_budget_returns_last_transient_error() {
    let media = Arc::new(b"abcdefghij".to_vec());
    let media_for_server = Arc::clone(&media);
    let server = TestHttpServer::spawn(move |index, request, stream| {
        if index >= 1 {
            // Соединение принято и сразу закрыто без ответа: как пропавшая сеть.
            let _ = stream.shutdown(Shutdown::Both);
            return;
        }
        respond_with_range(stream, &request, &media_for_server);
    });

    let mut source =
        HttpRangeSource::open(server.config_with_reconnect(Duration::from_millis(1_200)))
            .expect("source opens");
    let started = Instant::now();
    let error = source
        .read(&mut [0_u8; 4], &CancellationToken::never_cancelled())
        .expect_err("сеть так и не вернулась");
    let elapsed = started.elapsed();

    assert!(error.is_transient_network_failure(), "{error}");
    assert!(
        elapsed >= Duration::from_millis(1_200) && elapsed < Duration::from_secs(5),
        "сдаёмся по бюджету, а не сразу и не бесконечно: {elapsed:?}"
    );
    // Попытки в моменты ~0; 0,5; 1,2 с — паузы 0,5 и 0,7 (обрезана бюджетом).
    assert_eq!(source.range_diagnostics().reconnects, 2);
    assert_eq!(source.position(), 0);
}

/// Файл на сервере заменили (другой ETag): никакой склейки, сразу типизированная ошибка.
#[test]
fn changed_etag_is_typed_representation_change_without_retry() {
    let media = Arc::new(b"abcdefghij".to_vec());
    let media_for_server = Arc::clone(&media);
    let server = TestHttpServer::spawn(move |index, request, stream| {
        if index >= 1 {
            let (start, end) = parse_test_range(request.headers.get("range").expect("range"));
            let body = &media_for_server[start..=end];
            write_response(
                stream,
                "206 Partial Content",
                &[
                    ("Content-Length", body.len().to_string()),
                    (
                        "Content-Range",
                        format!("bytes {start}-{end}/{}", media_for_server.len()),
                    ),
                    ("ETag", "\"another-file\"".to_string()),
                ],
                body,
            );
            return;
        }
        respond_with_range(stream, &request, &media_for_server);
    });

    let mut source = HttpRangeSource::open(server.config_with_reconnect(Duration::from_secs(30)))
        .expect("source opens");
    let mut output = [0_u8; 4];
    let error = source
        .read(&mut output, &CancellationToken::never_cancelled())
        .expect_err("подмена файла обнаружена");

    assert!(matches!(
        error,
        SourceError::HttpRepresentationChanged {
            reason: HttpRepresentationChange::Validators
        }
    ));
    assert_eq!(output, [0_u8; 4], "байты чужого файла не отданы caller-у");
    assert_eq!(server.requests().len(), 2, "изменение файла не повторяется");
}

/// Другой полный размер файла — тоже изменение, даже без validators.
#[test]
fn changed_total_length_is_typed_representation_change() {
    let media = Arc::new(b"abcdefghij".to_vec());
    let media_for_server = Arc::clone(&media);
    let server = TestHttpServer::spawn(move |index, request, stream| {
        if index >= 1 {
            let (start, end) = parse_test_range(request.headers.get("range").expect("range"));
            let body = &media_for_server[start..=end];
            write_response(
                stream,
                "206 Partial Content",
                &[
                    ("Content-Length", body.len().to_string()),
                    ("Content-Range", format!("bytes {start}-{end}/999")),
                ],
                body,
            );
            return;
        }
        respond_with_range(stream, &request, &media_for_server);
    });

    let mut source = HttpRangeSource::open(server.config_with_reconnect(Duration::from_secs(30)))
        .expect("source opens");
    let error = source
        .read(&mut [0_u8; 4], &CancellationToken::never_cancelled())
        .expect_err("другой размер файла обнаружен");

    assert!(matches!(
        error,
        SourceError::HttpRepresentationChanged {
            reason: HttpRepresentationChange::TotalLength
        }
    ));
}

/// CDN, который в одном из ответов не прислал validator, не считается подменой.
#[test]
fn missing_validator_in_later_response_is_not_a_change() {
    let media = Arc::new(b"abcdefghij".to_vec());
    let media_for_server = Arc::clone(&media);
    let server = TestHttpServer::spawn(move |index, request, stream| {
        if index >= 1 {
            let (start, end) = parse_test_range(request.headers.get("range").expect("range"));
            let body = &media_for_server[start..=end];
            write_response(
                stream,
                "206 Partial Content",
                &[
                    ("Content-Length", body.len().to_string()),
                    (
                        "Content-Range",
                        format!("bytes {start}-{end}/{}", media_for_server.len()),
                    ),
                ],
                body,
            );
            return;
        }
        respond_with_range(stream, &request, &media_for_server);
    });

    let mut source = HttpRangeSource::open(server.config_with_reconnect(Duration::from_secs(30)))
        .expect("source opens");
    let mut output = [0_u8; 4];
    source
        .read(&mut output, &CancellationToken::never_cancelled())
        .expect("ответ без validators читается");

    assert_eq!(&output, b"abcd");
}

/// Закрытие media во время паузы: ожидание прерывается сразу, новых запросов нет.
#[test]
fn cancellation_during_backoff_stops_reconnect_without_new_requests() {
    let media = Arc::new(b"abcdefghij".to_vec());
    let media_for_server = Arc::clone(&media);
    let range_attempts = Arc::new(AtomicUsize::new(0));
    let range_attempts_for_server = Arc::clone(&range_attempts);
    let server = TestHttpServer::spawn(move |index, request, stream| {
        if index >= 1 {
            range_attempts_for_server.fetch_add(1, Ordering::SeqCst);
            write_response(
                stream,
                "503 Service Unavailable",
                &[
                    ("Content-Length", "0".to_string()),
                    ("Retry-After", "30".to_string()),
                ],
                b"",
            );
            return;
        }
        respond_with_range(stream, &request, &media_for_server);
    });

    let mut source = HttpRangeSource::open(server.config_with_reconnect(Duration::from_secs(60)))
        .expect("source opens");
    let cancellation = CancellationToken::new();
    let canceller = cancellation.clone();
    let cancel_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        canceller.cancel();
    });
    let started = Instant::now();

    let error = source
        .read(&mut [0_u8; 4], &cancellation)
        .expect_err("отмена прерывает ожидание");

    assert!(matches!(error, SourceError::Cancelled));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "ожидание Retry-After 30 с оборвано отменой"
    );
    cancel_thread.join().expect("cancel thread");
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        range_attempts.load(Ordering::SeqCst),
        1,
        "после отмены повторный запрос не уходит"
    );
}
