use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::*;

#[derive(Debug, Clone)]
struct TestRequest {
    headers: BTreeMap<String, String>,
}

struct TestHttpServer {
    url: String,
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
    requests: Arc<Mutex<Vec<TestRequest>>>,
}

impl TestHttpServer {
    fn spawn(handler: impl Fn(usize, TestRequest, TcpStream) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server bound");
        listener
            .set_nonblocking(true)
            .expect("test server nonblocking");
        let address = listener.local_addr().expect("test server address");
        let url = format!("http://{address}/media.bin");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_for_thread = Arc::clone(&requests);
        let handler = Arc::new(handler);

        let handle = thread::spawn(move || {
            let mut request_index = 0_usize;
            while !stop_for_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if stop_for_thread.load(Ordering::SeqCst) {
                            break;
                        }

                        let Ok(request) = read_test_request(&stream) else {
                            continue;
                        };
                        requests_for_thread
                            .lock()
                            .expect("requests lock")
                            .push(request.clone());
                        handler(request_index, request, stream);
                        request_index = request_index.saturating_add(1);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            url,
            address,
            stop,
            handle: Some(handle),
            requests,
        }
    }

    fn config(&self, headers: Vec<HttpHeader>) -> HttpRangeSourceConfig {
        HttpRangeSourceConfig::new(
            SecretHttpUrl::from_secret_for_open(self.url.clone()),
            headers,
            SourceRuntimeConfig::for_tests(
                1024 * 1024,
                Duration::from_millis(100),
                Duration::from_millis(100),
            ),
        )
    }

    fn requests(&self) -> Vec<TestRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl Drop for TestHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(handle) = self.handle.take() {
            handle.join().expect("test server joined");
        }
    }
}

fn read_test_request(stream: &TcpStream) -> std::io::Result<TestRequest> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let trimmed_line = line.trim_end_matches(['\r', '\n']);
        if trimmed_line.is_empty() {
            break;
        }

        if let Some((name, value)) = trimmed_line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    Ok(TestRequest { headers })
}

fn write_response(mut stream: TcpStream, status: &str, headers: &[(&str, String)], body: &[u8]) {
    write!(stream, "HTTP/1.1 {status}\r\n").expect("status written");
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n").expect("header written");
    }
    write!(stream, "Connection: close\r\n\r\n").expect("headers finished");
    stream.write_all(body).expect("body written");
    stream.flush().expect("response flushed");
}

fn respond_with_range(stream: TcpStream, request: &TestRequest, media: &[u8]) {
    let (start, end) = parse_test_range(
        request
            .headers
            .get("range")
            .expect("range header is present"),
    );
    let body = &media[start..=end];
    write_response(
        stream,
        "206 Partial Content",
        &[
            ("Content-Length", body.len().to_string()),
            (
                "Content-Range",
                format!("bytes {start}-{end}/{}", media.len()),
            ),
            ("ETag", "\"test-etag\"".to_string()),
            ("Last-Modified", "Tue, 12 May 2026 10:00:00 GMT".to_string()),
        ],
        body,
    );
}

fn parse_test_range(range_header: &str) -> (usize, usize) {
    let range = range_header
        .strip_prefix("bytes=")
        .expect("bytes range prefix");
    let (start, end) = range.split_once('-').expect("range separator");
    (
        start.parse::<usize>().expect("range start"),
        end.parse::<usize>().expect("range end"),
    )
}

#[test]
fn http_source_reads_206_ranges_and_preserves_headers() {
    let media = Arc::new(b"0123456789".to_vec());
    let media_for_server = Arc::clone(&media);
    let server = TestHttpServer::spawn(move |_index, request, stream| {
        assert_eq!(
            request.headers.get("x-direct-token").map(String::as_str),
            Some("token-1")
        );
        respond_with_range(stream, &request, &media_for_server);
    });

    let mut source =
        HttpRangeSource::open(server.config(vec![HttpHeader::new("X-Direct-Token", "token-1")]))
            .expect("http source opened");
    let token = CancellationToken::never_cancelled();
    let mut output = [0_u8; 4];

    assert!(source.seekability().is_seekable());
    assert_eq!(source.content_length(), Some(media.len() as u64));
    assert_eq!(source.validators().etag.as_deref(), Some("\"test-etag\""));

    let bytes_read = source.read(&mut output, &token).expect("range read works");
    assert_eq!(bytes_read, 4);
    assert_eq!(&output, b"0123");

    source.seek(5).expect("seek works");
    let mut second_output = [0_u8; 3];
    let bytes_read = source
        .read(&mut second_output, &token)
        .expect("second range read works");
    assert_eq!(bytes_read, 3);
    assert_eq!(&second_output, b"567");

    let ranges = server
        .requests()
        .into_iter()
        .filter_map(|request| request.headers.get("range").cloned())
        .collect::<Vec<_>>();
    assert_eq!(ranges, vec!["bytes=0-0", "bytes=0-3", "bytes=5-7"]);
    let diagnostics = source.range_diagnostics();
    assert_eq!(diagnostics.range_requests, 2);
    assert_eq!(diagnostics.bytes_requested, 7);
    assert_eq!(diagnostics.bytes_read, 7);
    assert_eq!(diagnostics.timeouts, 0);
}

#[test]
fn http_source_returns_partial_tail_read_at_content_length() {
    let media = Arc::new(b"0123456789".to_vec());
    let media_for_server = Arc::clone(&media);
    let server = TestHttpServer::spawn(move |_index, request, stream| {
        respond_with_range(stream, &request, &media_for_server);
    });

    let mut source = HttpRangeSource::open(server.config(Vec::new())).expect("source opens");
    let token = CancellationToken::never_cancelled();
    let mut output = *b"XXXXXXXX";

    source.seek(7).expect("seek to tail works");
    let bytes_read = source
        .read(&mut output, &token)
        .expect("tail range read works");

    assert_eq!(bytes_read, 3);
    assert_eq!(&output, b"789XXXXX");
    assert_eq!(source.position(), 10);

    let ranges = server
        .requests()
        .into_iter()
        .filter_map(|request| request.headers.get("range").cloned())
        .collect::<Vec<_>>();
    assert_eq!(ranges, vec!["bytes=0-0", "bytes=7-9"]);

    let diagnostics = source.range_diagnostics();
    assert_eq!(diagnostics.range_requests, 1);
    assert_eq!(diagnostics.bytes_requested, 3);
    assert_eq!(diagnostics.bytes_read, 3);
}

#[test]
fn http_source_reports_not_seekable_when_range_returns_200() {
    let media = b"plain-body".to_vec();
    let media_for_server = media.clone();
    let server = TestHttpServer::spawn(move |_index, _request, stream| {
        write_response(
            stream,
            "200 OK",
            &[("Content-Length", media_for_server.len().to_string())],
            &media_for_server,
        );
    });

    let mut source =
        HttpRangeSource::open(server.config(Vec::new())).expect("non-range http source opens");

    assert_eq!(
        source.seekability(),
        Seekability::NotSeekable {
            reason: NotSeekableReason::HttpRangeStatus { status: 200 }
        }
    );
    assert_eq!(source.content_length(), Some(media.len() as u64));

    let mut output = [0_u8; 4];
    let error = source
        .read(&mut output, &CancellationToken::never_cancelled())
        .expect_err("range read is rejected for non-seekable source");
    assert!(matches!(error, SourceError::NotSeekable { .. }));
}

#[test]
fn http_source_reports_timeout() {
    let server = TestHttpServer::spawn(move |_index, _request, _stream| {
        thread::sleep(Duration::from_millis(250));
    });
    let config = HttpRangeSourceConfig::new(
        SecretHttpUrl::from_secret_for_open(server.url.clone()),
        Vec::new(),
        SourceRuntimeConfig::for_tests(1024, Duration::from_millis(50), Duration::from_millis(50)),
    );

    let error = HttpRangeSource::open(config).expect_err("probe times out");
    assert!(matches!(error, SourceError::HttpTimeout { .. }));
}

#[test]
fn cancelled_range_read_returns_cancelled_without_advancing_position() {
    let media = Arc::new(b"abcdefghij".to_vec());
    let media_for_server = Arc::clone(&media);
    let cancellation = CancellationToken::new();
    let cancellation_for_server = cancellation.clone();
    let server = TestHttpServer::spawn(move |index, request, stream| {
        if index == 1 {
            cancellation_for_server.cancel();
        }

        respond_with_range(stream, &request, &media_for_server);
    });

    let mut source = HttpRangeSource::open(server.config(Vec::new())).expect("source opens");
    let mut output = [0_u8; 5];
    let error = source
        .read(&mut output, &cancellation)
        .expect_err("range read observes cancellation");

    assert!(matches!(error, SourceError::Cancelled));
    assert_eq!(source.position(), 0);
    assert_eq!(server.requests().len(), 2);

    let diagnostics = source.range_diagnostics();
    assert_eq!(diagnostics.range_requests, 1);
    assert_eq!(diagnostics.bytes_requested, 5);
    assert_eq!(diagnostics.bytes_read, 0);
}

#[test]
fn interrupted_range_response_retries_once() {
    let media = Arc::new(b"abcdefghij".to_vec());
    let media_for_server = Arc::clone(&media);
    let server = TestHttpServer::spawn(move |index, request, mut stream| {
        if index == 1 {
            let (start, end) = parse_test_range(request.headers.get("range").unwrap());
            let body = &media_for_server[start..=end];
            write!(stream, "HTTP/1.1 206 Partial Content\r\n").expect("status written");
            write!(stream, "Content-Length: {}\r\n", body.len()).expect("length written");
            write!(
                stream,
                "Content-Range: bytes {start}-{end}/{}\r\n",
                media_for_server.len()
            )
            .expect("range written");
            write!(stream, "Connection: close\r\n\r\n").expect("headers written");
            stream.write_all(&body[..2]).expect("partial body written");
            let _ = stream.shutdown(Shutdown::Both);
            return;
        }

        respond_with_range(stream, &request, &media_for_server);
    });

    let mut source = HttpRangeSource::open(server.config(Vec::new())).expect("source opens");
    let mut output = [0_u8; 5];
    let bytes_read = source
        .read(&mut output, &CancellationToken::never_cancelled())
        .expect("interrupted read retried");

    assert_eq!(bytes_read, 5);
    assert_eq!(&output, b"abcde");
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn range_failure_retries_once() {
    let media = Arc::new(b"abcdefghij".to_vec());
    let media_for_server = Arc::clone(&media);
    let server = TestHttpServer::spawn(move |index, request, stream| {
        if index == 1 {
            write_response(
                stream,
                "500 Internal Server Error",
                &[("Content-Length", "0".to_string())],
                b"",
            );
            return;
        }

        respond_with_range(stream, &request, &media_for_server);
    });

    let mut source = HttpRangeSource::open(server.config(Vec::new())).expect("source opens");
    let mut output = [0_u8; 4];
    let bytes_read = source
        .read(&mut output, &CancellationToken::never_cancelled())
        .expect("server error retried once");

    assert_eq!(bytes_read, 4);
    assert_eq!(&output, b"abcd");
    assert_eq!(server.requests().len(), 3);
}
