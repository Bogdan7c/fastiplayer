//! Audio pipeline crate: demuxer → codec-neutral decoder → ring buffer → cpal output.
//!
//! Архитектура:
//! ```text
//! Demuxer::next_packet() → media_core::Packet { kind: Audio, data: raw Opus }
//!     ↓
//! AudioDecoder::decode() → Vec<f32> (PCM interleaved)
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
pub use decoder::{
    AudioDecoder, AudioDecoderError, AudioDecoderHandle, OpusDecoder, create_audio_decoder,
};
pub use output::AudioOutput;
