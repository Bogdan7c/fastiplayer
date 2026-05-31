pub mod decoder;
pub mod decoder_thread;
pub mod dma_heap;
pub mod frame_pool;
pub mod internal_vaapi_frame;
pub mod player_core_adapter;
pub mod probe;
pub mod resource_pool;
pub mod zero_copy_surface_pool;

#[cfg(test)]
pub mod gbm_allocator;

#[cfg(test)]
pub mod linear_gbm_frame;

pub use decoder::VaapiVideoDecoder;
pub use decoder_thread::{
    DecodePacket, DecodeThreadBackpressureReason, DecodeThreadError, DecodeThreadSendError,
    VideoDecodeThread, VideoDecodeThreadConfig, VideoDecoderControlChannelPressureStats,
    VideoFrameResourceDescriptorLookup, VideoFrameResourceLockDiagnostics,
    VideoFrameResourceLookup, VideoFrameResourceProvider,
};
pub use player_core_adapter::VaapiVideoBackendFactory;
pub use probe::{VaapiCapabilityProvider, probe_vaapi_capabilities};
