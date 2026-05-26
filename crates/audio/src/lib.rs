//! Audio pipeline crate: demuxer → codec-neutral decoder → ring buffer → cpal output.
//!
//! Архитектура:
//! ```text
//! Demuxer::next_packet() → media_core::Packet { kind: Audio, data: encoded audio }
//!     ↓
//! AudioDecoder::decode(EncodedAudioPacket) → Vec<f32> (PCM interleaved)
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
mod output_adapter;

pub use audio_core::{
    AudioDecoder, AudioDecoderConfig, AudioDecoderError, AudioDecoderFactory, AudioDecoderHandle,
    AudioOutputFactory, AudioOutputSpec, AudioPacketTimeBase, AudioPacketTiming,
    EncodedAudioPacket, PlayerAudioClock, PlayerAudioOutput,
};
pub use clock::AudioClock;
pub use decoder::{ProductionAudioDecoderFactory, SymphoniaAudioDecoder, create_audio_decoder};
pub use output::AudioOutput;
pub use output_adapter::CpalAudioOutputFactory;
