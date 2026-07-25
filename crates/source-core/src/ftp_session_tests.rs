//! Focused FTP/FTPS wire fixtures для session lifecycle и security policy.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use super::*;
use crate::FtpRequestTarget;

/// Маленький deterministic payload для обычных transfer tests.
const TEST_FILE: &[u8] = b"0123456789abcdef";
/// Ответ успешного завершения data transfer-а.
const TRANSFER_COMPLETE_REPLY: &str = "226 transfer complete";
/// Ответ сервера, который считает досрочно закрытый transfer прерванным.
const TRANSFER_ABORTED_REPLY: &str = "426 transfer aborted";

/// Управляемое поведение hermetic FTP fixture-а.
#[derive(Debug, Clone)]
struct MiniFtpConfig {
    /// Bytes каждого RETR.
    file_bytes: Arc<[u8]>,
    /// Принимает ли control server REST.
    supports_rest: bool,
    /// Публикует ли SIZE.
    advertises_size: bool,
    /// Требует ли exact USER/PASS.
    require_auth: bool,
    /// Ожидаемый decoded username.
    expected_user: String,
    /// Ожидаемый decoded password.
    expected_pass: String,
    /// Optional certificate/key включает explicit FTPS.
    tls_cert: Option<(Vec<u8>, Vec<u8>)>,
    /// Host, который сервер намеренно публикует в PASV.
    advertised_pasv_host: [u8; 4],
    /// Terminal reply после записи data payload.
    terminal_reply: &'static str,
    /// Optional stall до первого data byte.
    data_stall: Option<Duration>,
}

impl Default for MiniFtpConfig {
    fn default() -> Self {
        Self {
            file_bytes: Arc::from(TEST_FILE),
            supports_rest: true,
            advertises_size: false,
            require_auth: false,
            expected_user: String::new(),
            expected_pass: String::new(),
            tls_cert: None,
            advertised_pasv_host: [127, 0, 0, 1],
            terminal_reply: TRANSFER_COMPLETE_REPLY,
            data_stall: None,
        }
    }
}

/// Process-local mini server с отдельным accept-loop.
struct MiniFtpServer {
    /// Control endpoint fixture-а.
    addr: SocketAddr,
    /// Detached accept-loop handle; test process завершает его вместе с fixture-ом.
    join: Option<thread::JoinHandle<()>>,
}

impl MiniFtpServer {
    /// Запускает server на loopback ephemeral port.
    fn start(config: MiniFtpConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(false).expect("blocking");
        let addr = listener.local_addr().expect("local addr");
        let join = thread::spawn(move || serve_forever(listener, config));
        Self {
            addr,
            join: Some(join),
        }
    }

    /// Строит cleartext locator.
    fn ftp_url(&self, path: &str) -> String {
        format!("ftp://127.0.0.1:{}{path}", self.addr.port())
    }

    /// Строит explicit-FTPS locator.
    fn ftps_url(&self, path: &str) -> String {
        format!("ftps://127.0.0.1:{}{path}", self.addr.port())
    }
}

impl Drop for MiniFtpServer {
    fn drop(&mut self) {
        // Accept-loop не блокирует test teardown: handle намеренно detach-ится.
        let _ = self.join.take();
    }
}

/// Control connection до и после AUTH TLS.
enum CommandIo {
    /// Cleartext FTP control socket.
    Plain(TcpStream),
    /// Box исключает большой enum variant в strict Clippy.
    Tls(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

impl CommandIo {
    /// Отправляет одну FTP response line.
    fn send_line(&mut self, line: &str) -> io::Result<()> {
        match self {
            Self::Plain(stream) => {
                write!(stream, "{line}\r\n")?;
                stream.flush()
            }
            Self::Tls(stream) => {
                write!(stream, "{line}\r\n")?;
                stream.flush()
            }
        }
    }

    /// Читает одну FTP command line без CRLF.
    fn read_line(&mut self) -> io::Result<String> {
        let mut byte = [0_u8; 1];
        let mut line_bytes = Vec::new();
        loop {
            match self {
                Self::Plain(stream) => stream.read_exact(&mut byte)?,
                Self::Tls(stream) => stream.read_exact(&mut byte)?,
            }
            if byte[0] == b'\n' {
                break;
            }
            if byte[0] != b'\r' {
                line_bytes.push(byte[0]);
            }
        }
        String::from_utf8(line_bytes)
            .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))
    }
}

/// Последовательно обслуживает независимые reconnect sessions.
fn serve_forever(listener: TcpListener, config: MiniFtpConfig) {
    while let Ok((stream, _)) = listener.accept() {
        let _ = handle_client(stream, config.clone());
    }
}

/// Реализует минимальный command set production client-а.
fn handle_client(stream: TcpStream, config: MiniFtpConfig) -> io::Result<()> {
    let mut command = CommandIo::Plain(stream);
    command.send_line("220 mini-ftp ready")?;
    let mut rest_offset: usize = 0;
    let mut passive_listener: Option<(TcpListener, usize)> = None;

    loop {
        let line = command.read_line()?;
        if line.is_empty() {
            continue;
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("AUTH TLS") {
            if config.tls_cert.is_none() {
                command.send_line("502 AUTH unavailable")?;
                continue;
            }
            command.send_line("234 Proceed with negotiation")?;
            let CommandIo::Plain(plain_stream) = command else {
                return Ok(());
            };
            let Some((certificate, key)) = config.tls_cert.clone() else {
                return Ok(());
            };
            let server_config = build_test_server_config(&certificate, &key);
            let connection =
                rustls::ServerConnection::new(Arc::new(server_config)).expect("server conn");
            command = CommandIo::Tls(Box::new(rustls::StreamOwned::new(connection, plain_stream)));
            continue;
        }
        if upper.starts_with("USER ") {
            let username = line[5..].trim();
            if config.require_auth && username != config.expected_user {
                command.send_line("530 auth failed")?;
            } else if config.require_auth {
                command.send_line("331 password required")?;
            } else {
                command.send_line("230 logged in")?;
            }
            continue;
        }
        if upper.starts_with("PASS ") {
            let password = line[5..].trim();
            if config.require_auth && password != config.expected_pass {
                command.send_line("530 auth failed")?;
            } else {
                command.send_line("230 logged in")?;
            }
            continue;
        }
        if upper.starts_with("PBSZ") || upper.starts_with("PROT") {
            command.send_line("200 ok")?;
            continue;
        }
        if upper.starts_with("TYPE I") || upper.starts_with("TYPE BIN") {
            command.send_line("200 Type set to I")?;
            continue;
        }
        if upper.starts_with("SIZE ") {
            if config.advertises_size {
                command.send_line(&format!("213 {}", config.file_bytes.len()))?;
            } else {
                command.send_line("550 size unavailable")?;
            }
            continue;
        }
        if upper.starts_with("REST ") {
            if !config.supports_rest {
                command.send_line("502 REST unavailable")?;
                continue;
            }
            rest_offset = line[5..].trim().parse().unwrap_or(0);
            command.send_line("350 Restart position accepted")?;
            continue;
        }
        if upper.starts_with("PASV") {
            let data_listener = TcpListener::bind("127.0.0.1:0")?;
            let port = data_listener.local_addr()?.port();
            let [host_1, host_2, host_3, host_4] = config.advertised_pasv_host;
            let (port_1, port_2) = (port / 256, port % 256);
            command.send_line(&format!(
                "227 Entering Passive Mode \
                 ({host_1},{host_2},{host_3},{host_4},{port_1},{port_2})"
            ))?;
            passive_listener = Some((data_listener, rest_offset));
            rest_offset = 0;
            continue;
        }
        if upper.starts_with("RETR ") {
            let Some((data_listener, offset)) = passive_listener.take() else {
                command.send_line("550 use pasv first")?;
                continue;
            };
            command.send_line("150 opening data")?;
            let (data_stream, _) = data_listener.accept()?;
            if let Some(stall) = config.data_stall {
                thread::sleep(stall);
            }
            write_data_payload(data_stream, &config, offset)?;
            command.send_line(config.terminal_reply)?;
            continue;
        }
        if upper.starts_with("QUIT") {
            command.send_line("221 bye")?;
            return Ok(());
        }
        command.send_line("502 command not implemented")?;
    }
}

/// Пишет clear либо TLS data payload.
fn write_data_payload(
    data_stream: TcpStream,
    config: &MiniFtpConfig,
    offset: usize,
) -> io::Result<()> {
    let payload = &config.file_bytes[offset..];
    if let Some((certificate, key)) = config.tls_cert.clone() {
        let server_config = build_test_server_config(&certificate, &key);
        let connection =
            rustls::ServerConnection::new(Arc::new(server_config)).expect("data server conn");
        let mut tls = rustls::StreamOwned::new(connection, data_stream);
        tls.write_all(payload)?;
        tls.flush()
    } else {
        let mut data_stream = data_stream;
        data_stream.write_all(payload)?;
        data_stream.shutdown(Shutdown::Both)
    }
}

/// Короткие deterministic network bounds focused tests.
fn test_runtime_config() -> SourceRuntimeConfig {
    SourceRuntimeConfig::for_tests(1024 * 1024, Duration::from_secs(2), Duration::from_secs(2))
}

/// Rustls server config из ephemeral self-signed material.
fn build_test_server_config(cert_der: &[u8], key_der: &[u8]) -> rustls::ServerConfig {
    let certificate = CertificateDer::from(cert_der.to_vec());
    let key = PrivateKeyDer::try_from(key_der.to_vec()).expect("key");
    rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .expect("server config")
}

/// Rustls client config, доверяющий только fixture certificate.
fn build_test_client_config(cert_der: &[u8]) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(cert_der.to_vec()))
        .expect("add cert");
    Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// Создаёт certificate/key для explicit FTPS fixture-а.
fn self_signed_material() -> (Vec<u8>, Vec<u8>) {
    let CertifiedKey { cert, key_pair } =
        generate_simple_self_signed(["127.0.0.1".to_string(), "localhost".to_string()])
            .expect("cert");
    (cert.der().to_vec(), key_pair.serialize_der())
}

#[test]
fn passive_type_i_and_rest_retr_reads_exact_offsets() {
    let server = MiniFtpServer::start(MiniFtpConfig {
        advertises_size: true,
        ..MiniFtpConfig::default()
    });
    let target = FtpRequestTarget::parse_exact(server.ftp_url("/media.bin")).expect("target");
    let session = FtpSourceSession::new(&test_runtime_config());
    let prepared = session
        .prepare(target, &CancellationToken::never_cancelled())
        .expect("prepare");
    assert_eq!(prepared.rest_capability(), FtpRestCapability::Supported);
    assert_eq!(prepared.content_length_hint(), Some(TEST_FILE.len() as u64));
    let mut source = prepared
        .into_seekable(&CancellationToken::never_cancelled())
        .expect("seekable");
    let mut bytes = [0_u8; 4];
    assert_eq!(
        source
            .read(&mut bytes, &CancellationToken::never_cancelled())
            .expect("read"),
        4
    );
    assert_eq!(&bytes, b"0123");
    source.seek(10).expect("seek");
    assert_eq!(
        source
            .read(&mut bytes, &CancellationToken::never_cancelled())
            .expect("read after seek"),
        4
    );
    assert_eq!(&bytes, b"abcd");
}

#[test]
fn pasv_advertised_host_is_replaced_with_control_peer() {
    let server = MiniFtpServer::start(MiniFtpConfig {
        // TEST-NET-2 не должен стать реальным data endpoint-ом.
        advertised_pasv_host: [198, 51, 100, 7],
        ..MiniFtpConfig::default()
    });
    let target = FtpRequestTarget::parse_exact(server.ftp_url("/media.bin")).expect("target");
    let session = FtpSourceSession::new(&test_runtime_config());
    let mut source = session
        .open(target, &CancellationToken::never_cancelled())
        .expect("control peer must own data host");
    let mut bytes = [0_u8; 4];
    let read = match &mut source {
        FtpOpenOutcome::Seekable(source) => {
            source.read(&mut bytes, &CancellationToken::never_cancelled())
        }
        FtpOpenOutcome::Streaming(source) => {
            source.read(&mut bytes, &CancellationToken::never_cancelled())
        }
    }
    .expect("data read");
    assert_eq!(read, 4);
    assert_eq!(&bytes, b"0123");
}

#[test]
fn seek_discards_aborted_transfer_before_reconnect() {
    let server = MiniFtpServer::start(MiniFtpConfig {
        terminal_reply: TRANSFER_ABORTED_REPLY,
        ..MiniFtpConfig::default()
    });
    let target = FtpRequestTarget::parse_exact(server.ftp_url("/media.bin")).expect("target");
    let session = FtpSourceSession::new(&test_runtime_config());
    let prepared = session
        .prepare(target, &CancellationToken::never_cancelled())
        .expect("prepare");
    let mut source = prepared
        .into_seekable(&CancellationToken::never_cancelled())
        .expect("seekable");
    let mut bytes = [0_u8; 4];
    source
        .read(&mut bytes, &CancellationToken::never_cancelled())
        .expect("initial read");
    source
        .seek(10)
        .expect("426 старой сессии не принадлежит новому seek");
    source
        .read(&mut bytes, &CancellationToken::never_cancelled())
        .expect("read after reconnect");
    assert_eq!(&bytes, b"abcd");
}

#[test]
fn size_without_rest_stays_streaming() {
    let server = MiniFtpServer::start(MiniFtpConfig {
        supports_rest: false,
        advertises_size: true,
        ..MiniFtpConfig::default()
    });
    let target = FtpRequestTarget::parse_exact(server.ftp_url("/media.bin")).expect("target");
    let session = FtpSourceSession::new(&test_runtime_config());
    let prepared = session
        .prepare(target, &CancellationToken::never_cancelled())
        .expect("prepare");
    assert_eq!(prepared.rest_capability(), FtpRestCapability::Unsupported);
    assert_eq!(prepared.content_length_hint(), Some(TEST_FILE.len() as u64));
    assert!(
        prepared
            .into_seekable(&CancellationToken::never_cancelled())
            .is_err()
    );
}

#[test]
fn no_rest_server_opens_streaming_outcome() {
    let server = MiniFtpServer::start(MiniFtpConfig {
        supports_rest: false,
        ..MiniFtpConfig::default()
    });
    let target = FtpRequestTarget::parse_exact(server.ftp_url("/media.bin")).expect("target");
    let session = FtpSourceSession::new(&test_runtime_config());
    let FtpOpenOutcome::Streaming(mut source) = session
        .open(target, &CancellationToken::never_cancelled())
        .expect("open")
    else {
        panic!("expected streaming fallback");
    };
    let mut bytes = vec![0_u8; TEST_FILE.len()];
    assert_eq!(
        source
            .read(&mut bytes, &CancellationToken::never_cancelled())
            .expect("read"),
        TEST_FILE.len()
    );
    assert_eq!(bytes, TEST_FILE);
}

#[test]
fn cancellation_interrupts_stalled_data_read_before_global_timeout() {
    let server = MiniFtpServer::start(MiniFtpConfig {
        supports_rest: false,
        data_stall: Some(Duration::from_secs(2)),
        ..MiniFtpConfig::default()
    });
    let target = FtpRequestTarget::parse_exact(server.ftp_url("/media.bin")).expect("target");
    let session = FtpSourceSession::new(&test_runtime_config());
    let prepared = session
        .prepare(target, &CancellationToken::never_cancelled())
        .expect("prepare");
    let mut source = prepared
        .into_streaming(&CancellationToken::never_cancelled())
        .expect("streaming");
    let cancellation = CancellationToken::new();
    let cancellation_for_thread = cancellation.clone();
    let cancel_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        cancellation_for_thread.cancel();
    });
    let started_at = Instant::now();
    let error = source
        .read(&mut [0_u8; 4], &cancellation)
        .expect_err("stalled read must observe cancellation");
    cancel_thread.join().expect("cancel thread");
    assert!(started_at.elapsed() < Duration::from_secs(1));
    assert!(matches!(
        error,
        SourceError::FtpTransport {
            kind: FtpTransportFailureKind::Cancelled,
            ..
        }
    ));
}

#[test]
fn encoded_credentials_are_decoded_and_diagnostics_stay_redacted() {
    let server = MiniFtpServer::start(MiniFtpConfig {
        require_auth: true,
        expected_user: "media user".to_owned(),
        expected_pass: "päss word".to_owned(),
        ..MiniFtpConfig::default()
    });
    let exact = format!(
        "ftp://media%20user:p%C3%A4ss%20word@127.0.0.1:{}/private%20video.bin",
        server.addr.port()
    );
    let target = FtpRequestTarget::parse_exact(exact).expect("target");
    let session = FtpSourceSession::new(&test_runtime_config());
    let prepared = session
        .prepare(target.clone(), &CancellationToken::never_cancelled())
        .expect("decoded auth");
    let diagnostic = format!("{prepared:?} {target:?}");
    for secret in ["media user", "päss word", "private video.bin"] {
        assert!(!diagnostic.contains(secret));
    }
}

#[test]
fn rejected_auth_does_not_leak_credentials() {
    let server = MiniFtpServer::start(MiniFtpConfig {
        require_auth: true,
        expected_user: "alice".to_owned(),
        expected_pass: "secret-pass".to_owned(),
        ..MiniFtpConfig::default()
    });
    let target = FtpRequestTarget::parse_exact(format!(
        "ftp://bob:wrong-pass@127.0.0.1:{}/private.bin",
        server.addr.port()
    ))
    .expect("target");
    let session = FtpSourceSession::new(&test_runtime_config());
    let error = session
        .prepare(target.clone(), &CancellationToken::never_cancelled())
        .expect_err("bad auth");
    let diagnostic = format!("{error:?} {target:?}");
    for secret in ["bob", "wrong-pass", "secret-pass", "private.bin"] {
        assert!(!diagnostic.contains(secret));
    }
}

#[test]
fn ftps_url_against_cleartext_server_fails_typed() {
    let server = MiniFtpServer::start(MiniFtpConfig::default());
    let target = FtpRequestTarget::parse_exact(server.ftps_url("/media.bin")).expect("target");
    let session = FtpSourceSession::new(&test_runtime_config());
    let error = session
        .prepare(target, &CancellationToken::never_cancelled())
        .expect_err("tls required");
    assert!(matches!(
        error.0,
        SourceError::FtpTransport {
            kind: FtpTransportFailureKind::TlsRequired
                | FtpTransportFailureKind::ProtocolViolation
                | FtpTransportFailureKind::NetworkUnavailable,
            ..
        }
    ));
}

#[test]
fn explicit_tls_fixture_roundtrip() {
    let (certificate, key) = self_signed_material();
    let server = MiniFtpServer::start(MiniFtpConfig {
        advertises_size: true,
        tls_cert: Some((certificate.clone(), key)),
        ..MiniFtpConfig::default()
    });
    let connector = build_test_client_config(&certificate);
    let session = FtpSourceSession::with_test_tls_config(test_runtime_config(), connector);
    let target = FtpRequestTarget::parse_exact(server.ftps_url("/secure.bin")).expect("target");
    let prepared = session
        .prepare(target, &CancellationToken::never_cancelled())
        .expect("prepare ftps");
    let mut source = prepared
        .into_seekable(&CancellationToken::never_cancelled())
        .expect("seekable");
    let mut bytes = vec![0_u8; TEST_FILE.len()];
    assert_eq!(
        source
            .read(&mut bytes, &CancellationToken::never_cancelled())
            .expect("read"),
        TEST_FILE.len()
    );
    assert_eq!(bytes, TEST_FILE);
}
