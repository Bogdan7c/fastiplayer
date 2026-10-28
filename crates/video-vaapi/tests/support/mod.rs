//! Общий код интеграционных hardware-тестов (подключается через `mod support;`).

// Каждый интеграционный тест — отдельный crate и берёт свою часть помощников.
#[allow(
    dead_code,
    reason = "each integration test crate uses its own subset of helpers"
)]
pub mod h264_media;
