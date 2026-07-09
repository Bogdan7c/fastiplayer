//! НЕ runtime backend: с 2026-07-09 плеер использует `audio-signalsmith`
//! (backend shootout показал 0 клик-швов у Signalsmith против 4-201 у
//! `timestretch`; см. `examples/backend_shootout.rs`). Crate сохранён как
//! evaluation/probe host: примеры метрик качества tempo-бэкендов
//! (`pcm_stats`, `fft_geometry_probe`, `quality_click_probe`,
//! `runtime_sequence_probe`, `tempo_throughput`, `backend_shootout`).
//!
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
//! - runtime default профиль — `Balanced` (`SESSION_THREAD_DEFAULT`): tempo
//!   processing идёт на session thread с ring-buffer запасом, а `LowLatency`
//!   отключает HPSS/adaptive phase locking/residual branch и слышимо портит
//!   качество;
//! - mismatch с README/Context7: fixed-buffer методы
//!   `process_interleaved_into`, `flush_interleaved_into` и
//!   `max_next_process_interleaved_output_samples` описаны в README, но не
//!   присутствуют в опубликованном source `0.4.0`.

mod adapter;

pub use adapter::{
    TimestretchOutputCapacityBudget, TimestretchQualityMode, TimestretchRatioSnapshot,
    TimestretchTempoError, TimestretchTempoProcessor, TimestretchTempoProcessorFactory,
    TimestretchTempoSettings,
};
