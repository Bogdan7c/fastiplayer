//! Изолированный concrete prototype для проверки `timestretch` до runtime-интеграции.
//!
//! S36 фиксирует minimum ship gate, а не подключает playback-rate к плееру:
//! `audio-core` остаётся нейтральным contract crate, `player-core` не получает
//! direct dependency на `timestretch`, а этот crate владеет только concrete
//! `StreamProcessor` state и проверочным adapter API.
//!
//! Проверенный API crates.io `timestretch 0.4.0`:
//! - usable streaming path: `StreamProcessor::process_into` и `flush_into`;
//! - dynamic control: `set_stretch_ratio`, `current_stretch_ratio`,
//!   `target_stretch_ratio`;
//! - bounded state introspection: `capacities`, `latency_secs`;
//! - realtime latency profile: `QualityMode::LowLatency` плюс FFT 1024 / hop
//!   256, потому что сам enum в `0.4.0` не уменьшает FFT/hop;
//! - mismatch с README/Context7: fixed-buffer методы
//!   `process_interleaved_into`, `flush_interleaved_into` и
//!   `max_next_process_interleaved_output_samples` описаны в README, но не
//!   присутствуют в опубликованном source `0.4.0`.

mod adapter;

pub use adapter::{
    TimestretchOutputCapacityBudget, TimestretchQualityMode, TimestretchRatioSnapshot,
    TimestretchTempoError, TimestretchTempoProcessor, TimestretchTempoSettings,
};
