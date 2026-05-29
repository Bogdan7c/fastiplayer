//! Предзагрузка byte-source данных перед demuxer-ом.
//!
//! Крейт держит нейтральные настройки, RAM-состояние prefetch-окна и `ByteSource` wrapper.
//! Здесь намеренно нет UI, demuxer-ов, codec-ов, renderer-а и service-specific интеграции.

#![forbid(unsafe_code)]

pub mod buffer;
mod config;
mod shared;
mod source;
mod worker;

pub use config::{PrefetchConfig, PrefetchConfigError};
pub use shared::PrefetchDiagnostics;
pub use source::PrefetchingByteSource;
