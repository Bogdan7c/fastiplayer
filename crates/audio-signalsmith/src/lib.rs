//! Concrete tempo adapter над Signalsmith Stretch за `audio-core` boundary.
//!
//! Выбран backend-shootout-ом 2026-07-09 на реальном brick-wall треке
//! (`audio-timestretch/examples/backend_shootout.rs`): 0 клик-швов на всех
//! скоростях 0.5x-3x против 4-201 у `timestretch 0.5`, умеренные пики
//! (1.1-1.4, закрываются выходным лимитером `AudioOutput`) и 140-275x
//! realtime CPU. Качество — spectral алгоритм класса élastique.
//!
//! Contract boundary не меняется: `player-core` видит только нейтральные
//! `AudioTempoProcessor`/`AudioTempoProcessorFactory` из `audio-core`; прямой
//! dependency на `signalsmith-stretch` живёт только здесь.

mod adapter;

pub use adapter::{SignalsmithTempoProcessor, SignalsmithTempoProcessorFactory};
