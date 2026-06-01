mod codec_adapter;

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

#[cfg(test)]
mod dependency_guardrail_tests {
    /// Проверяет, что backend crate не вернул прямые renderer/GPU import dependencies.
    #[test]
    fn video_vaapi_manifest_keeps_renderer_import_crates_out() {
        let manifest = include_str!("../Cargo.toml");

        for forbidden_dependency in ["wgpu", "wgpu-types", "ash"] {
            let has_forbidden_direct_dependency = manifest.lines().any(|line| {
                let trimmed_line = line.trim_start();
                trimmed_line.starts_with(&format!("{forbidden_dependency} "))
                    || trimmed_line.starts_with(&format!("{forbidden_dependency}="))
            });

            assert!(
                !has_forbidden_direct_dependency,
                "video-vaapi must not directly depend on `{forbidden_dependency}`"
            );
        }
    }
}
