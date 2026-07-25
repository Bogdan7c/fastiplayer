//! Progressive FTP(S) session для passive `TYPE I`, optional `SIZE`, `REST`+`RETR` и streaming fallback.
//!
//! Модуль владеет connect/login/probe/open и byte-source contract-ами. Policy parsing
//! (`FtpRequestTarget`) остаётся в `ftp_policy`; здесь выполняются только concrete FTP команды.

#[path = "ftp_session_data_channel.rs"]
mod data_channel;

use std::fmt;
use std::io::{self, Read};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use suppaftp::types::FileType;
use suppaftp::{FtpError, FtpStream, Mode, RustlsConnector, RustlsFtpStream};
use thiserror::Error;

use crate::{
    ByteSource, CancellationToken, FtpRequestTarget, FtpScheme, NotSeekableReason, Seekability,
    SourceError, SourceFingerprint, SourceResult, SourceRuntimeConfig, SourceValidators,
    StreamingByteSource,
};

/// Secret-safe категория transport failure без server payload и без URL/credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtpTransportFailureKind {
    /// Сеть недоступна либо connect/read завершился до установления сессии.
    NetworkUnavailable,
    /// Операция превысила configured timeout.
    Timeout,
    /// Сервер отклонил user/password.
    AuthenticationRejected,
    /// URL не содержит userinfo, а anonymous login недоступен.
    AuthenticationMissing,
    /// Ответ/команда нарушает ожидаемый FTP contract.
    ProtocolViolation,
    /// `ftps://` требует explicit TLS, но secure channel не установился.
    TlsRequired,
    /// Caller отменил операцию через `CancellationToken`.
    Cancelled,
    /// Blocking I/O был прерван (например `EINTR`).
    Interrupted,
}

/// Техническое подтверждение byte-accurate `REST` после успешного `TYPE I`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtpRestCapability {
    /// Сервер не принял probe `REST` после `TYPE I`.
    Unsupported,
    /// Probe `REST` прошёл; seekable open допустим.
    Supported,
}

/// Ошибка открытия FTP source без раскрытия locator/credentials.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct FtpSourceOpenError(#[from] pub SourceError);

/// Результат convenience-open: seekable только при подтверждённом REST.
pub enum FtpOpenOutcome {
    /// Byte-accurate `REST`+`RETR` source.
    Seekable(FtpSeekableSource),
    /// Forward-only `RETR` без REST.
    Streaming(FtpStreamingSource),
}

impl fmt::Debug for FtpOpenOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Seekable(_) => formatter.write_str("FtpOpenOutcome::Seekable(<redacted>)"),
            Self::Streaming(_) => formatter.write_str("FtpOpenOutcome::Streaming(<redacted>)"),
        }
    }
}

/// Подготовленное открытие после connect/login/`TYPE I`/probe без старта `RETR`.
#[derive(Debug)]
pub struct FtpPreparedOpen {
    /// Runtime timeouts и budgets, использованные при probe.
    runtime_config: SourceRuntimeConfig,
    /// Exact admitted target для последующих transfer-ов.
    target: FtpRequestTarget,
    /// Optional `SIZE` hint; сам по себе не делает source seekable.
    content_length_hint: Option<u64>,
    /// REST capability после probe.
    rest_capability: FtpRestCapability,
    /// Opaque fingerprint из stable identity hash.
    fingerprint: SourceFingerprint,
    /// Optional test-only rustls config для hermetic explicit FTPS.
    test_tls_config: Option<Arc<rustls::ClientConfig>>,
}

impl FtpPreparedOpen {
    /// Возвращает optional `SIZE` hint, если сервер его сообщил.
    #[must_use]
    pub const fn content_length_hint(&self) -> Option<u64> {
        self.content_length_hint
    }

    /// Возвращает результат REST probe после `TYPE I`.
    #[must_use]
    pub const fn rest_capability(&self) -> FtpRestCapability {
        self.rest_capability
    }

    /// Возвращает fingerprint для cache/ownership.
    #[must_use]
    pub fn fingerprint(&self) -> &SourceFingerprint {
        &self.fingerprint
    }

    /// Открывает seekable source только при `FtpRestCapability::Supported`.
    pub fn into_seekable(
        self,
        cancellation: &CancellationToken,
    ) -> Result<FtpSeekableSource, FtpSourceOpenError> {
        if self.rest_capability != FtpRestCapability::Supported {
            return Err(FtpSourceOpenError(SourceError::NotSeekable {
                reason: NotSeekableReason::FtpRestUnsupported,
            }));
        }
        FtpSeekableSource::open_prepared(self, cancellation).map_err(FtpSourceOpenError)
    }

    /// Открывает forward-only streaming source независимо от REST capability.
    pub fn into_streaming(
        self,
        cancellation: &CancellationToken,
    ) -> Result<FtpStreamingSource, FtpSourceOpenError> {
        FtpStreamingSource::open_prepared(self, cancellation).map_err(FtpSourceOpenError)
    }
}

/// Session factory для progressive FTP(S) open/probe.
#[derive(Clone)]
pub struct FtpSourceSession {
    /// Normalized network timeouts из пользовательского config.
    runtime_config: SourceRuntimeConfig,
    /// Optional override rustls config (tests) либо lazy production webpki roots.
    test_tls_config: Option<Arc<rustls::ClientConfig>>,
}

impl fmt::Debug for FtpSourceSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FtpSourceSession")
            .field("runtime_config", &self.runtime_config)
            .field("has_tls_config_override", &self.test_tls_config.is_some())
            .finish()
    }
}

impl FtpSourceSession {
    /// Создаёт session с runtime timeouts из `SourceRuntimeConfig`.
    #[must_use]
    pub fn new(runtime_config: &SourceRuntimeConfig) -> Self {
        Self {
            runtime_config: runtime_config.clone(),
            test_tls_config: None,
        }
    }

    /// Выполняет connect/login/`TYPE I`, optional `SIZE` и REST probe.
    pub fn prepare(
        &self,
        target: FtpRequestTarget,
        cancellation: &CancellationToken,
    ) -> Result<FtpPreparedOpen, FtpSourceOpenError> {
        check_cancelled(cancellation, "prepare")?;
        let mut control = connect_control(
            &target,
            &self.runtime_config,
            self.test_tls_config.as_ref(),
            cancellation,
        )?;
        login_control(&mut control, &target, cancellation)?;
        set_binary_transfer_type(&mut control, cancellation)?;
        let content_length_hint = query_optional_size(&mut control, &target, cancellation)?;
        let rest_capability = probe_rest_capability(&mut control, target.scheme(), cancellation)?;
        let _ = close_control_quietly(control);

        Ok(FtpPreparedOpen {
            runtime_config: self.runtime_config.clone(),
            fingerprint: build_ftp_fingerprint(&target),
            target,
            content_length_hint,
            rest_capability,
            test_tls_config: self.test_tls_config.clone(),
        })
    }

    /// Convenience-open: seekable при Supported REST, иначе streaming.
    pub fn open(
        &self,
        target: FtpRequestTarget,
        cancellation: &CancellationToken,
    ) -> Result<FtpOpenOutcome, FtpSourceOpenError> {
        let prepared = self.prepare(target, cancellation)?;
        if prepared.rest_capability() == FtpRestCapability::Supported {
            Ok(FtpOpenOutcome::Seekable(
                prepared.into_seekable(cancellation)?,
            ))
        } else {
            Ok(FtpOpenOutcome::Streaming(
                prepared.into_streaming(cancellation)?,
            ))
        }
    }

    /// Test-only ctor с custom rustls roots для hermetic explicit FTPS fixtures.
    #[cfg(test)]
    fn with_test_tls_config(
        runtime_config: SourceRuntimeConfig,
        client_config: Arc<rustls::ClientConfig>,
    ) -> Self {
        Self {
            runtime_config,
            test_tls_config: Some(client_config),
        }
    }
}

/// Seekable FTP byte source через `REST`+`RETR` с reconnect на `seek`.
pub struct FtpSeekableSource {
    /// Shared open parameters для reconnect seek-ов.
    open_context: FtpOpenContext,
    /// Текущий byte cursor относительно начала remote file.
    position: u64,
    /// Активный data stream текущего `RETR`, если transfer открыт.
    active_data: Option<Box<dyn Read + Send>>,
    /// Активный control stream текущего `RETR`, если transfer открыт.
    active_control: Option<FtpControlStream>,
    /// После успешного EOF/finalize повторные reads обязаны стабильно возвращать EOF.
    reached_eof: bool,
}

impl fmt::Debug for FtpSeekableSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FtpSeekableSource")
            .field("target", &self.open_context.target)
            .field("position", &self.position)
            .field("has_active_transfer", &self.active_data.is_some())
            .finish()
    }
}

impl FtpSeekableSource {
    fn open_prepared(
        prepared: FtpPreparedOpen,
        cancellation: &CancellationToken,
    ) -> SourceResult<Self> {
        let open_context = FtpOpenContext::from_prepared(prepared);
        let mut source = Self {
            open_context,
            position: 0,
            active_data: None,
            active_control: None,
            reached_eof: false,
        };
        source.start_transfer_at(0, cancellation)?;
        Ok(source)
    }

    fn start_transfer_at(
        &mut self,
        offset: u64,
        cancellation: &CancellationToken,
    ) -> SourceResult<()> {
        self.discard_active_transfer();
        check_cancelled(cancellation, "retr")?;
        let mut control = self.open_context.connect_control(cancellation)?;
        login_control(&mut control, &self.open_context.target, cancellation)?;
        set_binary_transfer_type(&mut control, cancellation)?;
        if offset > 0 {
            rest_at_offset(&mut control, offset, cancellation)?;
        }
        let data = begin_retr(
            &mut control,
            self.open_context.target.remote_path_for_command(),
            self.open_context.runtime_config.read_timeout(),
            cancellation,
        )?;
        self.position = offset;
        self.active_control = Some(control);
        self.active_data = Some(data);
        self.reached_eof = false;
        Ok(())
    }

    /// Финализирует только transfer, который data channel уже довёл до EOF.
    fn finalize_completed_transfer(&mut self) -> SourceResult<()> {
        if let (Some(data), Some(mut control)) =
            (self.active_data.take(), self.active_control.take())
        {
            finalize_retr(&mut control, data)?;
            let _ = close_control_quietly(control);
        }
        self.reached_eof = true;
        Ok(())
    }

    /// Незавершённый RETR не обязан отвечать `226`; seek просто рвёт старую сессию.
    fn discard_active_transfer(&mut self) {
        drop(self.active_data.take());
        drop(self.active_control.take());
    }
}

impl ByteSource for FtpSeekableSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.reached_eof {
            return Ok(0);
        }
        check_cancelled(cancellation, "read")?;
        let data = read_with_cancellation(
            self.active_data.as_mut().ok_or_else(|| {
                transport_error("read", FtpTransportFailureKind::ProtocolViolation)
            })?,
            output,
            cancellation,
            "read",
            self.open_context.runtime_config.read_timeout(),
        )?;
        if data == 0 {
            self.finalize_completed_transfer()?;
            return Ok(0);
        }
        self.position = self.position.saturating_add(data as u64);
        Ok(data)
    }

    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        self.start_transfer_at(offset, &CancellationToken::never_cancelled())
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn seekability(&self) -> Seekability {
        Seekability::Seekable
    }

    fn validators(&self) -> SourceValidators {
        SourceValidators::default()
    }

    fn content_length(&self) -> Option<u64> {
        self.open_context.content_length_hint
    }

    fn fingerprint(&self) -> SourceFingerprint {
        self.open_context.fingerprint.clone()
    }
}

impl Drop for FtpSeekableSource {
    fn drop(&mut self) {
        self.discard_active_transfer();
    }
}

/// Forward-only FTP byte source через один `RETR` без `REST`.
pub struct FtpStreamingSource {
    /// Shared open parameters (для diagnostics/fingerprint).
    open_context: FtpOpenContext,
    /// Control stream удерживается до завершения transfer-а.
    control: Option<FtpControlStream>,
    /// Единственный data stream текущего `RETR`.
    data: Option<Box<dyn Read + Send>>,
    /// Успешный EOF уже финализирован и должен оставаться стабильным.
    reached_eof: bool,
}

impl fmt::Debug for FtpStreamingSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FtpStreamingSource")
            .field("target", &self.open_context.target)
            .finish_non_exhaustive()
    }
}

impl FtpStreamingSource {
    fn open_prepared(
        prepared: FtpPreparedOpen,
        cancellation: &CancellationToken,
    ) -> SourceResult<Self> {
        let open_context = FtpOpenContext::from_prepared(prepared);
        let mut control = open_context.connect_control(cancellation)?;
        login_control(&mut control, &open_context.target, cancellation)?;
        set_binary_transfer_type(&mut control, cancellation)?;
        let data = begin_retr(
            &mut control,
            open_context.target.remote_path_for_command(),
            open_context.runtime_config.read_timeout(),
            cancellation,
        )?;
        Ok(Self {
            open_context,
            control: Some(control),
            data: Some(data),
            reached_eof: false,
        })
    }

    /// Закрывает успешно прочитанный transfer и потребляет terminal FTP reply.
    fn finalize_completed_transfer(&mut self) -> SourceResult<()> {
        if let (Some(data), Some(mut control)) = (self.data.take(), self.control.take()) {
            finalize_retr(&mut control, data)?;
            let _ = close_control_quietly(control);
        }
        self.reached_eof = true;
        Ok(())
    }

    /// Drop/cancel не требуют ложного успешного terminal reply незавершённого RETR.
    fn discard_active_transfer(&mut self) {
        drop(self.data.take());
        drop(self.control.take());
    }
}

impl StreamingByteSource for FtpStreamingSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.reached_eof {
            return Ok(0);
        }
        let bytes_read = read_with_cancellation(
            self.data.as_mut().ok_or_else(|| {
                transport_error("read", FtpTransportFailureKind::ProtocolViolation)
            })?,
            output,
            cancellation,
            "read",
            self.open_context.runtime_config.read_timeout(),
        )?;
        if bytes_read == 0 {
            self.finalize_completed_transfer()?;
        }
        Ok(bytes_read)
    }
}

impl Drop for FtpStreamingSource {
    fn drop(&mut self) {
        self.discard_active_transfer();
    }
}

/// Параметры открытия, общие для seekable/streaming source-ов.
#[derive(Clone)]
struct FtpOpenContext {
    runtime_config: SourceRuntimeConfig,
    target: FtpRequestTarget,
    content_length_hint: Option<u64>,
    fingerprint: SourceFingerprint,
    test_tls_config: Option<Arc<rustls::ClientConfig>>,
}

impl fmt::Debug for FtpOpenContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FtpOpenContext")
            .field("target", &self.target)
            .field("content_length_hint", &self.content_length_hint)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

impl FtpOpenContext {
    fn from_prepared(prepared: FtpPreparedOpen) -> Self {
        Self {
            runtime_config: prepared.runtime_config,
            target: prepared.target,
            content_length_hint: prepared.content_length_hint,
            fingerprint: prepared.fingerprint,
            test_tls_config: prepared.test_tls_config,
        }
    }

    fn connect_control(&self, cancellation: &CancellationToken) -> SourceResult<FtpControlStream> {
        connect_control(
            &self.target,
            &self.runtime_config,
            self.test_tls_config.as_ref(),
            cancellation,
        )
    }
}

/// Unified control stream для plain FTP и explicit FTPS.
enum FtpControlStream {
    Plain(FtpStream),
    Secure(RustlsFtpStream),
}

macro_rules! dispatch_control {
    ($control:expr, $method:ident ( $($arg:expr),* $(,)? )) => {
        match $control {
            FtpControlStream::Plain(stream) => stream.$method($($arg),*),
            FtpControlStream::Secure(stream) => stream.$method($($arg),*),
        }
    };
    ($control:expr, mut $method:ident ( $($arg:expr),* $(,)? )) => {
        match $control {
            FtpControlStream::Plain(stream) => stream.$method($($arg),*),
            FtpControlStream::Secure(stream) => stream.$method($($arg),*),
        }
    };
}

fn apply_read_timeout(control: &FtpControlStream, timeout: Duration) -> SourceResult<()> {
    let socket = dispatch_control!(control, get_ref());
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|source| map_io_error("connect", source))?;
    socket
        .set_write_timeout(Some(timeout))
        .map_err(|source| map_io_error("connect", source))
}

fn set_binary_transfer_type(
    control: &mut FtpControlStream,
    cancellation: &CancellationToken,
) -> SourceResult<()> {
    check_cancelled(cancellation, "type")?;
    dispatch_control!(control, transfer_type(FileType::Binary))
        .map_err(|error| map_ftp_error("type", error, FtpScheme::Ftp))
}

fn login_control(
    control: &mut FtpControlStream,
    target: &FtpRequestTarget,
    cancellation: &CancellationToken,
) -> SourceResult<()> {
    check_cancelled(cancellation, "login")?;
    let (username, password) = target.login_credentials();
    dispatch_control!(control, login(username, password)).map_err(|error| {
        if !target.has_userinfo() && matches!(error, FtpError::UnexpectedResponse(_)) {
            transport_error("login", FtpTransportFailureKind::AuthenticationMissing)
        } else {
            map_ftp_error("login", error, target.scheme())
        }
    })
}

fn query_optional_size(
    control: &mut FtpControlStream,
    target: &FtpRequestTarget,
    cancellation: &CancellationToken,
) -> SourceResult<Option<u64>> {
    check_cancelled(cancellation, "size")?;
    match dispatch_control!(control, size(target.remote_path_for_command())) {
        Ok(size) => Ok(Some(size as u64)),
        Err(FtpError::UnexpectedResponse(_)) => Ok(None),
        Err(error) => Err(map_ftp_error("size", error, target.scheme())),
    }
}

fn probe_rest_capability(
    control: &mut FtpControlStream,
    scheme: FtpScheme,
    cancellation: &CancellationToken,
) -> SourceResult<FtpRestCapability> {
    check_cancelled(cancellation, "rest-probe")?;
    const PROBE_OFFSET: usize = 1;
    match dispatch_control!(control, resume_transfer(PROBE_OFFSET)) {
        Ok(()) => {
            let _ = dispatch_control!(control, resume_transfer(0));
            Ok(FtpRestCapability::Supported)
        }
        Err(FtpError::UnexpectedResponse(_)) => Ok(FtpRestCapability::Unsupported),
        Err(error) => Err(map_ftp_error("rest-probe", error, scheme)),
    }
}

fn rest_at_offset(
    control: &mut FtpControlStream,
    offset: u64,
    cancellation: &CancellationToken,
) -> SourceResult<()> {
    check_cancelled(cancellation, "rest")?;
    let offset = usize::try_from(offset)
        .map_err(|_| transport_error("rest", FtpTransportFailureKind::ProtocolViolation))?;
    dispatch_control!(control, resume_transfer(offset))
        .map_err(|error| map_ftp_error("rest", error, FtpScheme::Ftp))
}

fn begin_retr(
    control: &mut FtpControlStream,
    remote_path: &str,
    read_timeout: Duration,
    cancellation: &CancellationToken,
) -> SourceResult<Box<dyn Read + Send>> {
    check_cancelled(cancellation, "retr")?;
    match control {
        FtpControlStream::Plain(stream) => {
            let data = stream
                .retr_as_stream(remote_path)
                .map_err(|error| map_ftp_error("retr", error, FtpScheme::Ftp))?;
            check_cancelled(cancellation, "retr")?;
            data_channel::configure_read_poll(data.get_ref(), read_timeout)
                .map_err(|source| map_io_error("retr", source))?;
            Ok(Box::new(data))
        }
        FtpControlStream::Secure(stream) => {
            let data = stream
                .retr_as_stream(remote_path)
                .map_err(|error| map_ftp_error("retr", error, FtpScheme::Ftps))?;
            check_cancelled(cancellation, "retr")?;
            data_channel::configure_read_poll(data.get_ref(), read_timeout)
                .map_err(|source| map_io_error("retr", source))?;
            Ok(Box::new(data))
        }
    }
}

fn finalize_retr(control: &mut FtpControlStream, data: Box<dyn Read + Send>) -> SourceResult<()> {
    match control {
        FtpControlStream::Plain(stream) => stream
            .finalize_retr_stream(data)
            .map_err(|error| map_ftp_error("retr-finalize", error, FtpScheme::Ftp)),
        FtpControlStream::Secure(stream) => stream
            .finalize_retr_stream(data)
            .map_err(|error| map_ftp_error("retr-finalize", error, FtpScheme::Ftps)),
    }
}

fn close_control_quietly(mut control: FtpControlStream) -> SourceResult<()> {
    let _ = match &mut control {
        FtpControlStream::Plain(stream) => stream.quit(),
        FtpControlStream::Secure(stream) => stream.quit(),
    };
    Ok(())
}

fn connect_control(
    target: &FtpRequestTarget,
    runtime_config: &SourceRuntimeConfig,
    tls_override: Option<&Arc<rustls::ClientConfig>>,
    cancellation: &CancellationToken,
) -> SourceResult<FtpControlStream> {
    check_cancelled(cancellation, "connect")?;
    let socket_addr = resolve_socket_addr(target)?;
    let control = match target.scheme() {
        FtpScheme::Ftp => {
            let mut stream =
                FtpStream::connect_timeout(socket_addr, runtime_config.connect_timeout())
                    .map_err(|error| map_ftp_error("connect", error, FtpScheme::Ftp))?
                    .passive_stream_builder(data_channel::stream_builder(
                        runtime_config.connect_timeout(),
                        runtime_config.read_timeout(),
                    ));
            if socket_addr.is_ipv6() {
                // EPSV уже использует control peer и не публикует отдельный data host.
                stream.set_mode(Mode::ExtendedPassive);
            } else {
                // SuppaFTP default доверяет host из PASV. Подмена на control peer
                // закрывает FTP bounce/SSRF и одновременно поддерживает NAT-серверы.
                stream.set_passive_nat_workaround(true);
            }
            FtpControlStream::Plain(stream)
        }
        FtpScheme::Ftps => {
            let connector = RustlsConnector::from(
                tls_override
                    .cloned()
                    .unwrap_or_else(production_rustls_config),
            );
            let mut stream =
                RustlsFtpStream::connect_timeout(socket_addr, runtime_config.connect_timeout())
                    .map_err(|error| map_ftp_error("connect", error, FtpScheme::Ftps))?
                    .passive_stream_builder(data_channel::stream_builder(
                        runtime_config.connect_timeout(),
                        runtime_config.read_timeout(),
                    ));
            if socket_addr.is_ipv6() {
                stream.set_mode(Mode::ExtendedPassive);
            } else {
                stream.set_passive_nat_workaround(true);
            }
            let secure = stream
                .into_secure(connector, target.endpoint().host())
                .map_err(|error| map_ftps_secure_error("tls", error))?;
            FtpControlStream::Secure(secure)
        }
    };
    apply_read_timeout(&control, runtime_config.read_timeout())?;
    Ok(control)
}

fn resolve_socket_addr(target: &FtpRequestTarget) -> SourceResult<SocketAddr> {
    let endpoint = target.endpoint();
    let host_port = format!("{}:{}", endpoint.host(), endpoint.effective_port());
    host_port
        .to_socket_addrs()
        .map_err(|source| map_io_error("connect", source))?
        .next()
        .ok_or_else(|| transport_error("connect", FtpTransportFailureKind::NetworkUnavailable))
}

fn build_ftp_fingerprint(target: &FtpRequestTarget) -> SourceFingerprint {
    SourceFingerprint::new(format!("ftp:{:016x}", target.stable_identity_hash()))
}

fn production_rustls_config() -> Arc<rustls::ClientConfig> {
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth(),
    )
}

fn check_cancelled(cancellation: &CancellationToken, operation: &'static str) -> SourceResult<()> {
    if cancellation.is_cancelled() {
        Err(SourceError::FtpTransport {
            operation,
            kind: FtpTransportFailureKind::Cancelled,
        })
    } else {
        Ok(())
    }
}

fn read_with_cancellation(
    reader: &mut (dyn Read + Send),
    output: &mut [u8],
    cancellation: &CancellationToken,
    operation: &'static str,
    read_timeout: Duration,
) -> SourceResult<usize> {
    let started_at = Instant::now();
    loop {
        check_cancelled(cancellation, operation)?;
        match reader.read(output) {
            Ok(bytes_read) => return Ok(bytes_read),
            Err(source)
                if matches!(
                    source.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                ) =>
            {
                if started_at.elapsed() >= read_timeout {
                    return Err(transport_error(operation, FtpTransportFailureKind::Timeout));
                }
            }
            Err(source) => return Err(map_io_error(operation, source)),
        }
    }
}

fn transport_error(operation: &'static str, kind: FtpTransportFailureKind) -> SourceError {
    SourceError::FtpTransport { operation, kind }
}

fn map_io_error(operation: &'static str, source: io::Error) -> SourceError {
    let kind = match source.kind() {
        io::ErrorKind::TimedOut => FtpTransportFailureKind::Timeout,
        io::ErrorKind::Interrupted => FtpTransportFailureKind::Interrupted,
        io::ErrorKind::NotConnected
        | io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::UnexpectedEof => FtpTransportFailureKind::NetworkUnavailable,
        _ => FtpTransportFailureKind::NetworkUnavailable,
    };
    SourceError::FtpTransport { operation, kind }
}

fn map_ftps_secure_error(operation: &'static str, error: FtpError) -> SourceError {
    match error {
        FtpError::SecureError(_) | FtpError::UnexpectedResponse(_) => SourceError::FtpTransport {
            operation,
            kind: FtpTransportFailureKind::TlsRequired,
        },
        other => map_ftp_error(operation, other, FtpScheme::Ftps),
    }
}

fn map_ftp_error(operation: &'static str, error: FtpError, scheme: FtpScheme) -> SourceError {
    let kind = match error {
        FtpError::ConnectionError(source) => match source.kind() {
            io::ErrorKind::TimedOut => FtpTransportFailureKind::Timeout,
            io::ErrorKind::Interrupted => FtpTransportFailureKind::Interrupted,
            _ => FtpTransportFailureKind::NetworkUnavailable,
        },
        FtpError::SecureError(_) if scheme == FtpScheme::Ftps => {
            FtpTransportFailureKind::TlsRequired
        }
        FtpError::SecureError(_) => FtpTransportFailureKind::ProtocolViolation,
        FtpError::UnexpectedResponse(response) => {
            if matches!(operation, "login")
                && matches!(response.status.code(), 331 | 530 | 501 | 421)
            {
                FtpTransportFailureKind::AuthenticationRejected
            } else {
                FtpTransportFailureKind::ProtocolViolation
            }
        }
        FtpError::BadResponse
        | FtpError::InvalidAddress(_)
        | FtpError::DataConnectionAlreadyOpen => FtpTransportFailureKind::ProtocolViolation,
    };
    SourceError::FtpTransport { operation, kind }
}

#[cfg(test)]
#[path = "ftp_session_tests.rs"]
mod tests;
