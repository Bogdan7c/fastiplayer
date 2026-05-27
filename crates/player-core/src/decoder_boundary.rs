pub use video_backend_api::{
    PresentFrameResourceProvider, PresentFrameResourceProviderHandle,
    PresentFrameResourceProviderLookup, StartedVideoBackend,
};
#[cfg(test)]
pub(crate) use video_core::DecodeBackpressureReason;
pub(crate) use video_core::{
    DecodePacket as PlayerDecodePacket, DecodeSendError, DecodeThreadError, DecoderResourceSnapshot,
};

/// Backwards-compatible public name для backend-neutral decoder-thread config.
pub type PlayerVideoDecoderThreadConfig = video_core::VideoDecoderThreadConfig;

/// Player-core specialization neutral decoder handle-а с renderer-neutral resource provider-ом.
pub(crate) type PlayerVideoDecoderThreadHandle = video_backend_api::VideoBackendDecoderThreadHandle;
