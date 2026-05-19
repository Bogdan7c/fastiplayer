use render_core::{P010StorageLayout, RenderCapabilities};

/// Строит public renderer capabilities из реально включённых wgpu device features.
pub(crate) fn wgpu_capabilities_from_features(
    max_texture_size: Option<u32>,
    device_features: wgpu::Features,
) -> RenderCapabilities {
    let mut supported_p010_storage_layouts = Vec::new();

    if device_features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM) {
        supported_p010_storage_layouts.push(P010StorageLayout::BaselineSeparateLayer);
    }

    if device_features.contains(wgpu::Features::TEXTURE_FORMAT_P010) {
        supported_p010_storage_layouts.push(P010StorageLayout::CompatibilityComposed);
    }

    if supported_p010_storage_layouts.is_empty() {
        RenderCapabilities::wgpu_nv12(max_texture_size)
    } else {
        RenderCapabilities::wgpu_p010_bt2446c_with_storage_layouts(
            max_texture_size,
            supported_p010_storage_layouts,
        )
    }
}

#[cfg(test)]
mod tests {
    use render_core::{HdrToSdrSettings, VideoFrameFormat};

    use super::*;

    #[test]
    fn renderer_capabilities_advertise_hdr_when_p010_import_feature_is_enabled() {
        let separate_layer_capabilities =
            wgpu_capabilities_from_features(Some(4096), wgpu::Features::TEXTURE_FORMAT_16BIT_NORM);
        let composed_capabilities =
            wgpu_capabilities_from_features(Some(4096), wgpu::Features::TEXTURE_FORMAT_P010);

        assert!(separate_layer_capabilities.supports_p010_rendering());
        assert!(separate_layer_capabilities.supports_hdr_to_sdr_with(&HdrToSdrSettings::default()));
        assert!(
            separate_layer_capabilities
                .supports_p010_storage_layout(P010StorageLayout::BaselineSeparateLayer)
        );
        assert!(
            !separate_layer_capabilities
                .supports_p010_storage_layout(P010StorageLayout::CompatibilityComposed)
        );
        assert!(composed_capabilities.supports_p010_rendering());
        assert!(composed_capabilities.supports_hdr_to_sdr_with(&HdrToSdrSettings::default()));
        assert!(
            composed_capabilities
                .supports_p010_storage_layout(P010StorageLayout::CompatibilityComposed)
        );
        assert!(
            !composed_capabilities
                .supports_p010_storage_layout(P010StorageLayout::BaselineSeparateLayer)
        );
    }

    #[test]
    fn renderer_capabilities_stay_nv12_when_p010_import_features_are_missing() {
        let capabilities = wgpu_capabilities_from_features(Some(4096), wgpu::Features::empty());

        assert!(capabilities.supports_frame_format(VideoFrameFormat::Nv12));
        assert!(!capabilities.supports_p010_rendering());
        assert!(!capabilities.supports_hdr_to_sdr_with(&HdrToSdrSettings::default()));
    }
}
