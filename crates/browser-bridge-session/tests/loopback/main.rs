//! Тесты сервера сеанса по настоящему loopback: клиент `tungstenite` ходит в
//! настоящий `BrowserSessionServer` так же, как будет ходить background экстеншена.
//!
//! Тест проверяет РЕЗУЛЬТАТ: коды закрытия, байты в приёмнике, события жизни.
//! Ожидания идут по условиям с щедрым потолком (`support::WAIT`), а не по
//! `sleep`: тест не зависит от скорости машины.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod backpressure;
mod close_order;
mod farewell;
mod lifecycle_events;
mod orphan_link;
mod page_subtitles;
mod protocol_errors;
mod protocol_versions;
mod stream_replay;
mod support;
mod wire_order;
mod wire_tap;
