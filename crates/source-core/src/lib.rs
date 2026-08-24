//! Нейтральный byte-source слой rustiplayer.
//!
//! Crate владеет только чтением bytes из локальных файлов, HTTP Range источников,
//! progressive FTP(S) источников, metadata источника и RAM range cache. Здесь
//! намеренно нет знаний о media services, extractor-ах, контейнерах, demuxer-ах,
//! decoder-ах или UI.

#![forbid(unsafe_code)]

mod abortable_http_task;
mod cache;
mod cancellation;
mod config;
mod error;
mod ftp_locator;
mod ftp_policy;
mod ftp_session;
mod http;
mod http_bounded;
#[cfg(test)]
mod http_bounded_async_tests;
mod http_client;
mod http_cookie;
mod http_cookie_seed;
mod http_locator;
mod http_policy;
mod http_retry_after;
mod http_session;
mod local;
mod metadata;

pub use abortable_http_task::{
    AbortableHttpTask, AbortableHttpTaskExecutor, AbortableHttpTaskExecutorError,
};
pub use cache::{
    CacheDiagnostics, CachedByteSource, RamByteRangeCache, RangeDiagnostics, SourceDiagnostics,
};
pub use cancellation::CancellationToken;
pub use config::SourceRuntimeConfig;
pub use error::{HttpRepresentationChange, HttpRequestPolicyFailure, SourceError, SourceResult};
pub use ftp_locator::SecretFtpUrl;
pub use ftp_policy::{FtpEndpoint, FtpRequestTarget, FtpRequestTargetError, FtpScheme};
pub use ftp_session::{
    FtpOpenOutcome, FtpPreparedOpen, FtpRestCapability, FtpSeekableSource, FtpSourceOpenError,
    FtpSourceSession, FtpStreamingSource, FtpTransportFailureKind,
};
pub use http::{HttpHeader, HttpRangeSource, HttpRangeSourceConfig};
pub use http_bounded::{
    HttpBoundedByteRange, HttpBoundedFetchHop, HttpBoundedFetchKind, HttpBoundedFetchRequest,
    HttpBoundedResponse, HttpRangeResponseMetadata,
};
pub use http_cookie::{ScopedHttpCookieJar, ScopedHttpCookieJarError};
pub use http_cookie_seed::{HttpCookieSeed, HttpCookieSeedBuilder, HttpCookieSeedError};
pub use http_locator::SecretHttpUrl;
pub use http_policy::{
    HttpHeaderValidationError, HttpOrigin, HttpPathScope, HttpPathScopeError, HttpRequestScope,
    HttpRequestTarget, HttpRequestTargetError, HttpScheme, HttpScopeSecurity, ValidatedHttpHeaders,
};
pub use http_retry_after::HttpRetryAfter;
pub use http_session::{
    HttpRangeRedirectBodyForwarding, HttpRangeRedirectHandler, HttpRangeRedirectHopCount,
    HttpRangeRedirectRejection, HttpRangeRedirectRequestMaterial, HttpRedirectHop,
    HttpRedirectRequestBehavior, HttpRequestBody, HttpSingleHopRequest, HttpSourceHop,
    HttpSourceSession, HttpStreamingSource,
};
pub use local::{LocalFileMetadataSnapshot, LocalFileSource};
pub use metadata::{
    ByteSource, NotSeekableReason, Seekability, SourceFingerprint, SourceValidators,
    StreamingByteSource,
};
