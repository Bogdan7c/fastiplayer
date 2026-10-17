//! Сквозные тесты crate-а на реальных кадрах ffmpeg (`tests/fixtures/`).
//!
//! Тесты живут внутри crate-а, потому что проверяют внутренние результаты
//! пакета D1 (частотные коэффициенты, учёт бит), которых нет в публичном API.

mod bit_budget;
mod bsi_fields;
mod eac3;
mod eac3_differential;
mod ffmpeg_differential;
mod pcm_behaviour;
mod pcm_reference;
mod reference_comparison;
mod robustness;
mod spectrum;
mod support;
mod synthetic;
