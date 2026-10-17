//! Интеграционные тесты публичного API `ac3-syncframe`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test helpers fail the test by panicking"
)]

mod access_units;
mod header_tables;
mod real_frames;
mod search;
mod support;
