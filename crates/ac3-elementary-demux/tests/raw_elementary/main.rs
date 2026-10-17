//! Функциональные тесты demuxer-а сырого AC-3 / E-AC-3 на фикстурах ffmpeg.
//!
//! Фикстуры не копируются: используются уже закоммиченные потоки
//! `crates/audio/tests/fixtures/ac3/` и `crates/ac3-syncframe/tests/fixtures/`
//! (README и `generate.sh` рядом с ними).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests: panic is the failure report"
)]

mod packets;
mod probe_selection;
mod resilience;
mod seek;
mod support;
