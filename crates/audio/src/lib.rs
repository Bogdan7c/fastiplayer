//! Audio pipeline crate: demuxer → decoder → ring buffer → cpal output.
//!
//! Архитектура:
//! ```text
//! Demuxer::next_packet() → media_core::Packet { kind: Audio, data: raw Opus }
//!     ↓
//! OpusDecoder::decode() → Vec<f32> (PCM interleaved)
//!     ↓
//! AudioOutput::write_samples() → RingBuffer Producer
//!     ↓
//! CPAL callback → RingBuffer Consumer → Динамики
//! ```
//!
//! AudioClock отслеживает время воспроизведения для A/V sync.

pub mod clock;
pub mod decoder;
pub mod output;

pub use clock::AudioClock;
pub use decoder::OpusDecoder;
pub use output::AudioOutput;
