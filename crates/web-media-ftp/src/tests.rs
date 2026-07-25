//! Focused hermetic tests для progressive FTP TransportProvider.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rustiplayer_config::NetworkConfig;
use source_core::{CancellationToken, FtpRequestTarget, SourceRuntimeConfig};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
    SourceIdentity,
};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, ProviderOpenError,
    SourceGeneration, TransportOpenRequest, TransportProvider, TransportProviderId,
    TransportRegistry, TransportScheme, UnsupportedTransportReason,
};

use super::{WEB_MEDIA_FTP_PROVIDER_ID, WebMediaFtpProvider};

const TEST_BYTES: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

struct MiniFtpServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<()>>,
}

struct MiniFtpConfig {
    file_bytes: Arc<[u8]>,
    supports_rest: bool,
    advertises_size: bool,
}

impl MiniFtpServer {
    fn start(config: MiniFtpConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let address = listener.local_addr().expect("addr");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let join_handle = thread::spawn(move || {
            while !worker_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = handle_client(stream, &config);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            stop,
            join_handle: Some(join_handle),
        }
    }

    fn ftp_url(&self, path: &str) -> String {
        format!(
            "ftp://127.0.0.1:{}/{}",
            self.address.port(),
            path.trim_start_matches('/')
        )
    }
}

impl Drop for MiniFtpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

fn send_line(stream: &mut TcpStream, line: &str) -> std::io::Result<()> {
    write!(stream, "{line}\r\n")?;
    stream.flush()
}

fn read_line(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut byte = [0_u8; 1];
    let mut line = String::new();
    loop {
        stream.read_exact(&mut byte)?;
        let ch = byte[0] as char;
        if ch == '\n' {
            break;
        }
        if ch != '\r' {
            line.push(ch);
        }
    }
    Ok(line)
}

fn handle_client(mut stream: TcpStream, config: &MiniFtpConfig) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    send_line(&mut stream, "220 ready")?;
    let mut rest_offset = 0_usize;
    let mut passive: Option<(TcpListener, usize)> = None;
    loop {
        let line = read_line(&mut stream)?;
        if line.is_empty() {
            continue;
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("USER ") || upper.starts_with("PASS ") {
            send_line(&mut stream, "230 logged in")?;
        } else if upper.starts_with("TYPE I") || upper.starts_with("TYPE BIN") {
            send_line(&mut stream, "200 Type set to I")?;
        } else if upper.starts_with("SIZE ") {
            if config.advertises_size {
                send_line(&mut stream, &format!("213 {}", config.file_bytes.len()))?;
            } else {
                send_line(&mut stream, "550 size unavailable")?;
            }
        } else if upper.starts_with("REST ") {
            if !config.supports_rest {
                send_line(&mut stream, "502 REST unavailable")?;
            } else {
                rest_offset = line[5..].trim().parse().unwrap_or(0);
                send_line(&mut stream, "350 Restart position accepted")?;
            }
        } else if upper.starts_with("PASV") {
            let data = TcpListener::bind("127.0.0.1:0")?;
            let port = data.local_addr()?.port();
            send_line(
                &mut stream,
                &format!(
                    "227 Entering Passive Mode (127,0,0,1,{},{})",
                    port / 256,
                    port % 256
                ),
            )?;
            passive = Some((data, rest_offset));
            rest_offset = 0;
        } else if upper.starts_with("RETR ") {
            let Some((data, offset)) = passive.take() else {
                send_line(&mut stream, "550 pasv first")?;
                continue;
            };
            send_line(&mut stream, "150 opening data")?;
            let (mut data_stream, _) = data.accept()?;
            data_stream.write_all(&config.file_bytes[offset..])?;
            data_stream.shutdown(Shutdown::Both)?;
            send_line(&mut stream, "226 transfer complete")?;
        } else if upper.starts_with("QUIT") {
            send_line(&mut stream, "221 bye")?;
            return Ok(());
        } else {
            send_line(&mut stream, "502 command not implemented")?;
        }
    }
}

fn runtime_config() -> SourceRuntimeConfig {
    SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("config")
}

fn open_request(url: &str) -> TransportOpenRequest {
    let provider = TransportProviderId::new(WEB_MEDIA_FTP_PROVIDER_ID).expect("id");
    let source = SourceIdentity::new(37);
    let identity = MediaComponentIdentity::new(
        CandidateIdentity::new(
            source,
            ExtractionGeneration::new(1),
            CandidateFormatIdentity::new("ftp-fmt").expect("format"),
        ),
        SemanticIdentity::new(source, "ftp-muxed").expect("semantic"),
        MediaComponentRole::Muxed,
    )
    .expect("identity");
    let target = FtpRequestTarget::parse_exact(url).expect("ftp target");
    TransportOpenRequest::for_ftp(
        provider,
        identity,
        target,
        MediaPresentation::Vod,
        SourceGeneration::new(1),
        CancellationToken::never_cancelled(),
    )
    .expect("request")
}

fn registry_with_provider() -> TransportRegistry {
    let mut registry = TransportRegistry::new();
    registry
        .register(Box::new(
            WebMediaFtpProvider::new(runtime_config()).expect("provider"),
        ))
        .expect("register");
    registry
}

#[test]
fn descriptor_admits_exact_ftp_and_ftps_schemes() {
    let provider = WebMediaFtpProvider::new(runtime_config()).expect("provider");
    let schemes = provider.descriptor().schemes();
    assert!(schemes.contains(&TransportScheme::Ftp(source_core::FtpScheme::Ftp)));
    assert!(schemes.contains(&TransportScheme::Ftp(source_core::FtpScheme::Ftps)));
}

#[test]
fn rest_capable_server_opens_seekable_input() {
    let server = MiniFtpServer::start(MiniFtpConfig {
        file_bytes: Arc::from(TEST_BYTES),
        supports_rest: true,
        advertises_size: true,
    });
    let opened = registry_with_provider()
        .open(open_request(&server.ftp_url("/media.bin")))
        .expect("open");
    let mut source = opened.into_input().into_seekable().expect("seekable");
    let mut buffer = [0_u8; 4];
    assert_eq!(
        source
            .read(&mut buffer, &CancellationToken::never_cancelled())
            .expect("read"),
        4
    );
    assert_eq!(&buffer, b"abcd");
    source.seek(10).expect("seek");
    assert_eq!(
        source
            .read(&mut buffer, &CancellationToken::never_cancelled())
            .expect("read after seek"),
        4
    );
    assert_eq!(&buffer, b"klmn");
}

#[test]
fn no_rest_server_opens_streaming_input() {
    let server = MiniFtpServer::start(MiniFtpConfig {
        file_bytes: Arc::from(TEST_BYTES),
        supports_rest: false,
        advertises_size: true,
    });
    let opened = registry_with_provider()
        .open(open_request(&server.ftp_url("/media.bin")))
        .expect("open");
    let mut source = opened.into_input().into_streaming().expect("streaming");
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 16];
    loop {
        let read = source
            .read(&mut chunk, &CancellationToken::never_cancelled())
            .expect("read");
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    assert_eq!(buffer, TEST_BYTES);
}

#[test]
fn size_present_without_rest_stays_streaming() {
    let server = MiniFtpServer::start(MiniFtpConfig {
        file_bytes: Arc::from(TEST_BYTES),
        supports_rest: false,
        advertises_size: true,
    });
    let opened = registry_with_provider()
        .open(open_request(&server.ftp_url("/media.bin")))
        .expect("open");
    assert!(
        opened.into_input().into_seekable().is_err(),
        "SIZE alone must not publish seekable input"
    );
}

#[test]
fn http_target_is_typed_unsupported_scheme() {
    let provider = WebMediaFtpProvider::new(runtime_config()).expect("provider");
    let http_provider = TransportProviderId::new(WEB_MEDIA_FTP_PROVIDER_ID).expect("id");
    let source = SourceIdentity::new(38);
    let identity = MediaComponentIdentity::new(
        CandidateIdentity::new(
            source,
            ExtractionGeneration::new(1),
            CandidateFormatIdentity::new("http-fmt").expect("format"),
        ),
        SemanticIdentity::new(source, "http-reject").expect("semantic"),
        MediaComponentRole::Muxed,
    )
    .expect("identity");
    let http_target =
        source_core::HttpRequestTarget::parse_exact("https://media.invalid/a.webm").expect("http");
    let request = TransportOpenRequest::new(
        http_provider,
        identity,
        http_target,
        MediaPresentation::Vod,
        SourceGeneration::new(1),
        web_media_transport_api::SecretRequestContext::empty(),
        web_media_transport_api::RedirectPolicy::new(
            web_media_transport_api::RedirectHopLimit::none(),
            web_media_transport_api::RedirectOriginPolicy::SameOriginOnly,
            web_media_transport_api::SecureRedirectPolicy::DenyDowngrade,
        ),
        CancellationToken::never_cancelled(),
    )
    .expect("request");
    match provider.open(&request) {
        Err(ProviderOpenError::Unsupported(UnsupportedTransportReason::Scheme)) => {}
        Ok(_) => panic!("http target must be unsupported by FTP provider"),
        Err(other) => panic!("unexpected open error: {other:?}"),
    }
}

#[test]
fn registry_absent_provider_is_typed() {
    let registry = TransportRegistry::new();
    let error = registry
        .open(open_request("ftp://127.0.0.1:9/missing.bin"))
        .expect_err("absent");
    assert!(matches!(
        error,
        web_media_transport_api::TransportOpenError::ProviderUnavailable { .. }
    ));
}

#[test]
fn open_errors_and_debug_redact_credentials() {
    let server = MiniFtpServer::start(MiniFtpConfig {
        file_bytes: Arc::from(TEST_BYTES),
        supports_rest: true,
        advertises_size: false,
    });
    let secret_url = format!(
        "ftp://alice:s3cret-pass@127.0.0.1:{}/private/video.webm",
        server.address.port()
    );
    let provider = WebMediaFtpProvider::new(runtime_config()).expect("provider");
    let request = open_request(&secret_url);
    let formatted = format!("{request:?}");
    for secret in ["alice", "s3cret-pass", "private", "video.webm"] {
        assert!(!formatted.contains(secret), "{formatted}");
    }
    let _ = provider.open(&request).expect("open with auth");
}
