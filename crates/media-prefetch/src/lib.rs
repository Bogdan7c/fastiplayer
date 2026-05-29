//! Предзагрузка byte-source данных перед demuxer-ом.
//!
//! Крейт держит только нейтральные настройки и RAM-состояние prefetch-окна.
//! Здесь намеренно нет потоков, блокировок, UI, demuxer-ов, codec-ов и renderer-а.

#![forbid(unsafe_code)]

pub mod buffer;
mod config;

pub use config::{PrefetchConfig, PrefetchConfigError};
