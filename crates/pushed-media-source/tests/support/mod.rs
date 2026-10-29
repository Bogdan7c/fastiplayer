//! Общая обвязка интеграционных тестов источника.

#![allow(
    dead_code,
    reason = "each test binary uses its own subset of the helpers"
)]

pub mod media;
pub mod playback;
pub mod recorded;
pub mod simulated_audio;
