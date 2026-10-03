//! Предзагрузка byte-source данных перед demuxer-ом.
//!
//! Крейт держит нейтральные настройки, RAM-состояние prefetch-окна и `ByteSource` wrapper.
//! Здесь намеренно нет UI, demuxer-ов, codec-ов, renderer-а и service-specific интеграции.

#![forbid(unsafe_code)]

pub mod buffer;
mod config;
mod seek;
mod shared;
mod source;
mod worker;

pub use config::{PrefetchConfig, PrefetchConfigError};
pub use shared::PrefetchDiagnostics;
pub use source::{PrefetchStartupError, PrefetchingByteSource};

// Функциональные тесты wrapper-а через публичный API; файл под `tests/`,
// поэтому в coverage universe production-кода не входит.
#[cfg(test)]
#[path = "tests/address_space_end.rs"]
mod address_space_end_tests;
