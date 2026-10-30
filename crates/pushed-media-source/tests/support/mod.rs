//! Общая обвязка интеграционных тестов источника.

#![allow(
    dead_code,
    reason = "each test binary uses its own subset of the helpers"
)]

pub mod media;
pub mod playback;
pub mod progressive_file;
pub mod range_page;
pub mod recorded;
pub mod simulated_audio;
