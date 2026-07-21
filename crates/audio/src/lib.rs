//! Audio pipeline crate: demuxer → codec-neutral decoder → ring buffer → cpal output.
//!
//! Архитектура:
//! ```text
//! Demuxer::next_event() → DemuxReadEvent::Packet { kind: Audio, data: encoded audio }
//!     ↓
//! AudioDecoder::decode(EncodedAudioPacket) → Vec<f32> (PCM interleaved)
//!     ↓
//! AudioOutput::write_samples() → RingBuffer Producer
//!     ↓
//! CPAL callback → RingBuffer Consumer → Динамики
//! ```
//!
//! AudioClock отслеживает время воспроизведения для A/V sync.

mod channel_mixer;
pub mod clock;
pub mod decoder;
pub mod devices;
pub mod output;
mod output_adapter;

pub use audio_core::{
    AudioChannelLayout, AudioChannelLayoutError, AudioChannelPosition, AudioDecodeCapability,
    AudioDecodeCapabilityProvider, AudioDecodeCapabilityQueryError, AudioDecodeCapabilitySnapshot,
    AudioDecodeCodecFamily, AudioDecodeCodecFamilyQuery, AudioDecoder, AudioDecoderConfig,
    AudioDecoderError, AudioDecoderFactory, AudioDecoderHandle, AudioOutputClockTiming,
    AudioOutputFactory, AudioOutputInputFrameCount, AudioOutputSpec, AudioOutputStreamFrameCount,
    AudioOutputWriteError, AudioOutputWriteIntent, AudioOutputWriteReport, AudioPacketTimeBase,
    AudioPacketTiming, EncodedAudioPacket, PlayerAudioClock, PlayerAudioOutput,
};
pub use clock::AudioClock;
pub use decoder::{ProductionAudioDecoderFactory, SymphoniaAudioDecoder, create_audio_decoder};
pub use devices::{
    AudioOutputDeviceController, AudioOutputDeviceError, AudioOutputDeviceInfo,
    AudioOutputDeviceSelectionChange, DEFAULT_AUDIO_OUTPUT_DEVICE_ID, list_output_devices,
};
pub use output::AudioOutput;
pub use output_adapter::CpalAudioOutputFactory;
