//! Чистый WGPU video renderer без window/surface/egui ownership.
//!
//! Crate владеет только video boundary: renderer capabilities, color pipeline,
//! NV12/P010 shader paths и materialization-facing public types.

#![forbid(unsafe_code)]

mod capabilities;
mod color_pipeline;
mod video;

#[cfg(test)]
mod bt2446c_reference;

pub use video::{
    WgpuFramePlanes, WgpuFrameTextureViewLookup, WgpuFrameTextureViewMaterializer,
    WgpuFrameTextureViews, WgpuRenderableFrame, WgpuVideoRenderer, clear_to_black,
};

/// Возвращает набор WGPU features, обязательный для video texture boundary.
///
/// NV12 запрашивается всегда, потому что production renderer не имеет CPU upload
/// fallback. P010-related features добавляются только если adapter уже сообщил
/// поддержку, чтобы shell не включал неподдержанные optional paths.
#[must_use]
pub fn required_wgpu_video_texture_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    required_wgpu_video_texture_features_from_adapter_features(adapter.features())
}

/// Чистая часть feature policy для unit tests без реального GPU adapter-а.
fn required_wgpu_video_texture_features_from_adapter_features(
    adapter_features: wgpu::Features,
) -> wgpu::Features {
    let mut required_features = wgpu::Features::TEXTURE_FORMAT_NV12;

    if adapter_features.contains(wgpu::Features::TEXTURE_FORMAT_P010) {
        required_features |= wgpu::Features::TEXTURE_FORMAT_P010;
    }

    if adapter_features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM) {
        required_features |= wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;
    }

    required_features
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_feature_policy_always_requires_nv12() {
        let required_features =
            required_wgpu_video_texture_features_from_adapter_features(wgpu::Features::empty());

        assert!(required_features.contains(wgpu::Features::TEXTURE_FORMAT_NV12));
    }

    #[test]
    fn video_feature_policy_requests_supported_p010_paths() {
        let adapter_features =
            wgpu::Features::TEXTURE_FORMAT_P010 | wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;

        let required_features =
            required_wgpu_video_texture_features_from_adapter_features(adapter_features);

        assert!(required_features.contains(wgpu::Features::TEXTURE_FORMAT_NV12));
        assert!(required_features.contains(wgpu::Features::TEXTURE_FORMAT_P010));
        assert!(required_features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM));
    }

    #[test]
    fn video_feature_policy_does_not_request_unsupported_p010_paths() {
        let required_features =
            required_wgpu_video_texture_features_from_adapter_features(wgpu::Features::empty());

        assert!(!required_features.contains(wgpu::Features::TEXTURE_FORMAT_P010));
        assert!(!required_features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM));
    }
}
