//! Нейтральный byte-source слой rustiplayer.
//!
//! Crate владеет только чтением bytes из локальных файлов и HTTP Range
//! источников, metadata источника и RAM range cache. Здесь намеренно нет знаний
//! о YouTube, контейнерах, demuxer-ах, decoder-ах или UI.

#![forbid(unsafe_code)]

mod cache;
mod cancellation;
mod config;
mod error;
mod http;
mod http_locator;
mod local;
mod metadata;

pub use cache::{
    CacheDiagnostics, CachedByteSource, RamByteRangeCache, RangeDiagnostics, SourceDiagnostics,
};
pub use cancellation::CancellationToken;
pub use config::SourceRuntimeConfig;
pub use error::{SourceError, SourceResult};
pub use http::{HttpHeader, HttpRangeSource, HttpRangeSourceConfig};
pub use http_locator::SecretHttpUrl;
pub use local::LocalFileSource;
pub use metadata::{
    ByteSource, NotSeekableReason, Seekability, SourceFingerprint, SourceValidators,
};
