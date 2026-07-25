//! Concrete progressive FTP(S) provider для neutral web transport API.
//!
//! Crate владеет только per-open seekability policy поверх `source-core` FTP
//! session. Demux, player, yt-dlp DTO и HTTP secrets остаются у внешних owners.

#![forbid(unsafe_code)]

use std::error::Error;
use std::fmt;

use source_core::{
    FtpPreparedOpen, FtpRestCapability, FtpScheme, FtpSourceSession, FtpTransportFailureKind,
    SourceError, SourceRuntimeConfig,
};
use web_media_transport_api::{
    AuthenticationFailure, MediaPresentation, ProviderDescriptor, ProviderDescriptorError,
    ProviderOpenError, ProviderOpenOutput, ProviderRefreshError, RedirectHopCount, RefreshSupport,
    TransportFailure, TransportInput, TransportOpenRequest, TransportProvider, TransportProviderId,
    TransportProviderIdError, TransportRefreshRequest, TransportScheme, UnsupportedTransportReason,
};

/// Stable registry identity concrete progressive FTP provider-а.
pub const WEB_MEDIA_FTP_PROVIDER_ID: &str = "progressive-ftp";

/// Ошибка построения immutable provider descriptor-а до network side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebMediaFtpProviderBuildError {
    /// Compile-time provider ID перестал удовлетворять neutral grammar.
    InvalidProviderId(TransportProviderIdError),
    /// Static capability descriptor нарушает transport registry contract.
    InvalidDescriptor(ProviderDescriptorError),
}

impl fmt::Display for WebMediaFtpProviderBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProviderId(source) => {
                write!(formatter, "invalid FTP provider ID: {source}")
            }
            Self::InvalidDescriptor(source) => {
                write!(formatter, "invalid FTP provider descriptor: {source}")
            }
        }
    }
}

impl Error for WebMediaFtpProviderBuildError {}

/// Thin concrete provider над `source-core::FtpSourceSession`.
pub struct WebMediaFtpProvider {
    /// Immutable neutral registration descriptor.
    descriptor: ProviderDescriptor,
    /// Validated connection/read runtime policy.
    source_config: SourceRuntimeConfig,
}

impl WebMediaFtpProvider {
    /// Создаёт provider без FTP client-а и без network side effects.
    pub fn new(source_config: SourceRuntimeConfig) -> Result<Self, WebMediaFtpProviderBuildError> {
        let provider_id = TransportProviderId::new(WEB_MEDIA_FTP_PROVIDER_ID)
            .map_err(WebMediaFtpProviderBuildError::InvalidProviderId)?;
        let descriptor = ProviderDescriptor::new(
            provider_id,
            vec![
                TransportScheme::Ftp(FtpScheme::Ftp),
                TransportScheme::Ftp(FtpScheme::Ftps),
            ],
            RefreshSupport::Supported,
        )
        .map_err(WebMediaFtpProviderBuildError::InvalidDescriptor)?;
        Ok(Self {
            descriptor,
            source_config,
        })
    }

    /// Выполняет open/refresh через source-core FTP prepare + seekability policy.
    fn open_component(
        &self,
        request: &TransportOpenRequest,
    ) -> Result<ProviderOpenOutput, ProviderOpenError> {
        if request.cancellation().is_cancelled() {
            return Err(ProviderOpenError::Cancelled);
        }
        if !matches!(request.presentation(), MediaPresentation::Vod) {
            return Err(ProviderOpenError::Unsupported(
                UnsupportedTransportReason::Presentation,
            ));
        }
        let ftp_target = request
            .target()
            .as_ftp()
            .ok_or(ProviderOpenError::Unsupported(
                UnsupportedTransportReason::Scheme,
            ))?
            .clone();
        let session = FtpSourceSession::new(&self.source_config);
        let prepared = session
            .prepare(ftp_target, request.cancellation())
            .map_err(|error| map_ftp_open_error(error.0))?;
        let input = transport_input_from_prepared(prepared, request.cancellation())?;
        Ok(ProviderOpenOutput::new(
            request.target().clone(),
            RedirectHopCount::none(),
            request.presentation(),
            input,
        ))
    }
}

impl fmt::Debug for WebMediaFtpProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebMediaFtpProvider")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

impl TransportProvider for WebMediaFtpProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn open(
        &self,
        request: &TransportOpenRequest,
    ) -> Result<ProviderOpenOutput, ProviderOpenError> {
        self.open_component(request)
    }

    fn refresh(
        &self,
        request: &TransportRefreshRequest,
    ) -> Result<ProviderOpenOutput, ProviderRefreshError> {
        // Semantic/provider/generation fences уже проверены TransportRefreshRequest.
        self.open_component(request.replacement())
            .map_err(map_open_to_refresh_error)
    }
}

/// Per-open policy: seekable только после TYPE I + Supported REST; SIZE — hint only.
fn transport_input_from_prepared(
    prepared: FtpPreparedOpen,
    cancellation: &source_core::CancellationToken,
) -> Result<TransportInput, ProviderOpenError> {
    match prepared.rest_capability() {
        FtpRestCapability::Supported => {
            let source = prepared
                .into_seekable(cancellation)
                .map_err(|error| map_ftp_open_error(error.0))?;
            TransportInput::seekable(Box::new(source))
                .map_err(|_| ProviderOpenError::Transport(TransportFailure::InvalidResponse))
        }
        FtpRestCapability::Unsupported => {
            let source = prepared
                .into_streaming(cancellation)
                .map_err(|error| map_ftp_open_error(error.0))?;
            Ok(TransportInput::streaming(source))
        }
    }
}

/// Маппит source-core FTP errors в typed provider open outcomes без URL/credentials.
fn map_ftp_open_error(error: SourceError) -> ProviderOpenError {
    match error {
        SourceError::Cancelled => ProviderOpenError::Cancelled,
        SourceError::NotSeekable { .. } => {
            ProviderOpenError::Unsupported(UnsupportedTransportReason::Seekability)
        }
        SourceError::FtpTransport { kind, .. } => match kind {
            FtpTransportFailureKind::Cancelled => ProviderOpenError::Cancelled,
            FtpTransportFailureKind::AuthenticationMissing => {
                ProviderOpenError::Authentication(AuthenticationFailure::CredentialsMissing)
            }
            FtpTransportFailureKind::AuthenticationRejected => {
                ProviderOpenError::Authentication(AuthenticationFailure::CredentialsRejected)
            }
            FtpTransportFailureKind::Timeout => {
                ProviderOpenError::Transport(TransportFailure::Timeout)
            }
            FtpTransportFailureKind::Interrupted | FtpTransportFailureKind::NetworkUnavailable => {
                ProviderOpenError::Transport(TransportFailure::NetworkUnavailable)
            }
            FtpTransportFailureKind::ProtocolViolation | FtpTransportFailureKind::TlsRequired => {
                ProviderOpenError::Transport(TransportFailure::InvalidResponse)
            }
        },
        _ => ProviderOpenError::Transport(TransportFailure::NetworkUnavailable),
    }
}

/// Переводит open error в refresh error без смешивания категорий.
fn map_open_to_refresh_error(error: ProviderOpenError) -> ProviderRefreshError {
    match error {
        ProviderOpenError::Unsupported(reason) => ProviderRefreshError::Unsupported(reason),
        ProviderOpenError::Authentication(failure) => ProviderRefreshError::Authentication(failure),
        ProviderOpenError::Transport(failure) => ProviderRefreshError::Transport(failure),
        ProviderOpenError::Cancelled => ProviderRefreshError::Cancelled,
    }
}

#[cfg(test)]
mod tests;
