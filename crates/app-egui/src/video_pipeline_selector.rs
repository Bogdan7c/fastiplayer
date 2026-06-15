//! Выбор concrete video pipeline для app-owned composition layer-а.
//!
//! Selector остаётся pure: он читает committed config intent и capability snapshot,
//! но не запускает backend, не создаёт WGPU resources и не пересчитывает renderer
//! intersection. Исполнение выбранного плана остаётся в `AppState`.

use capability_core::{SupportedVideoOutput, SystemCapabilities};
use player_core::PlayerVideoDecoderThreadConfig;
use rustiplayer_config::VideoBackendPreference;
use thiserror::Error;
use video_frame_contract::{HardwareFrameHandle, VideoFrameTransferPath};

/// Stable backend id из `codec-core`; app layer сравнивает строку без direct dependency.
const VAAPI_BACKEND_ID: &str = "vaapi";

/// Concrete video pipeline, который `app-egui` умеет реально запустить сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoPipelinePlan {
    /// Production path: VA-API decode, DMA-BUF zero-copy transfer, WGPU materializer.
    VaapiDmaBufWgpu {
        /// Runtime limits decoder thread-а, которые нужно передать concrete backend factory.
        decoder_thread_config: PlayerVideoDecoderThreadConfig,
    },
}

/// Ошибка pure selection до запуска backend-а.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum VideoPipelineSelectionError {
    /// Shell ещё не передал capability snapshot в app state.
    #[error("capability snapshot ещё недоступен для выбора video pipeline")]
    CapabilitiesUnavailable,

    /// `auto` не нашёл ни одного уже playable path-а.
    #[error(
        "video.preferred_backend=auto: нет playable VA-API DMA-BUF output после renderer intersection"
    )]
    AutoVaapiDmaBufUnavailable,

    /// Пользователь явно потребовал VA-API, но capability report не подтверждает path.
    #[error(
        "video.preferred_backend=vaapi: нет playable VA-API DMA-BUF output после renderer intersection"
    )]
    PreferredVaapiDmaBufUnavailable,
}

/// Выбирает concrete plan из committed video config и готового capability snapshot-а.
pub(crate) fn select_video_pipeline_plan(
    preferred_backend: VideoBackendPreference,
    system_capabilities: Option<&SystemCapabilities>,
    decoder_thread_config: PlayerVideoDecoderThreadConfig,
) -> Result<VideoPipelinePlan, VideoPipelineSelectionError> {
    let system_capabilities =
        system_capabilities.ok_or(VideoPipelineSelectionError::CapabilitiesUnavailable)?;

    let has_playable_vaapi_dma_buf = system_capabilities
        .supported_video_outputs()
        .any(is_playable_vaapi_dma_buf_output);

    match preferred_backend {
        VideoBackendPreference::Auto if has_playable_vaapi_dma_buf => {
            Ok(VideoPipelinePlan::VaapiDmaBufWgpu {
                decoder_thread_config,
            })
        }
        VideoBackendPreference::Auto => {
            Err(VideoPipelineSelectionError::AutoVaapiDmaBufUnavailable)
        }
        VideoBackendPreference::Vaapi if has_playable_vaapi_dma_buf => {
            Ok(VideoPipelinePlan::VaapiDmaBufWgpu {
                decoder_thread_config,
            })
        }
        VideoBackendPreference::Vaapi => {
            Err(VideoPipelineSelectionError::PreferredVaapiDmaBufUnavailable)
        }
    }
}

/// Проверяет только уже пересечённый system-level output.
fn is_playable_vaapi_dma_buf_output(output: &SupportedVideoOutput) -> bool {
    output.backend.as_str() == VAAPI_BACKEND_ID
        && output.frame_contract.validate().is_ok()
        && matches!(
            output.frame_contract.transfer_path,
            VideoFrameTransferPath::HardwareZeroCopy {
                handle: HardwareFrameHandle::DmaBuf { .. },
            }
        )
}

#[cfg(test)]
mod tests {
    use capability_core::{
        BackendCapabilities, BackendDriverInfo, BackendProbeStatus,
        CURRENT_CAPABILITY_SCHEMA_VERSION, SupportedVideoOutput, SystemCapabilities,
    };
    use codec_core::{
        BitDepth, ChromaSubsampling, DecodeBackendId, SupportedVideoDecodeFormat, VideoCodec,
        VideoProfile, Vp9Profile,
    };
    use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

    use super::*;

    fn current_vaapi_output() -> SupportedVideoOutput {
        SupportedVideoOutput {
            backend: DecodeBackendId::vaapi(),
            decode_format: vp9_profile0_format(),
            frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        }
    }

    fn non_vaapi_dma_buf_output() -> SupportedVideoOutput {
        SupportedVideoOutput {
            backend: DecodeBackendId::new("future_backend").expect("valid test backend id"),
            decode_format: vp9_profile0_format(),
            frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        }
    }

    fn host_upload_vaapi_output() -> SupportedVideoOutput {
        SupportedVideoOutput {
            backend: DecodeBackendId::vaapi(),
            decode_format: vp9_profile0_format(),
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
        }
    }

    fn vp9_profile0_format() -> SupportedVideoDecodeFormat {
        SupportedVideoDecodeFormat {
            codec: VideoCodec::Vp9,
            profile: VideoProfile::Vp9(Vp9Profile::Profile0),
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            max_width: Some(3840),
            max_height: Some(2160),
            max_fps: Some(60.0),
            hdr_input: false,
        }
    }

    fn capabilities_with_playable_outputs(
        playable_video_outputs: Vec<SupportedVideoOutput>,
    ) -> SystemCapabilities {
        SystemCapabilities {
            schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
            probed_at_unix_seconds: 1,
            video_backends: vec![BackendCapabilities {
                backend_id: DecodeBackendId::vaapi(),
                display_name: "Test VA-API".to_string(),
                status: BackendProbeStatus::Available,
                driver: BackendDriverInfo::default(),
                raw_supported_outputs: playable_video_outputs.clone(),
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                diagnostics: Vec::new(),
            }],
            render_backends: Vec::new(),
            playable_video_outputs,
        }
    }

    #[test]
    fn auto_selects_vaapi_with_current_capabilities() {
        let decoder_thread_config = PlayerVideoDecoderThreadConfig::default();
        let capabilities = capabilities_with_playable_outputs(vec![current_vaapi_output()]);

        let plan = select_video_pipeline_plan(
            VideoBackendPreference::Auto,
            Some(&capabilities),
            decoder_thread_config,
        )
        .expect("auto should select current VA-API path");

        assert_eq!(
            plan,
            VideoPipelinePlan::VaapiDmaBufWgpu {
                decoder_thread_config,
            }
        );
    }

    #[test]
    fn vaapi_preference_requires_vaapi_dma_buf_output() {
        let decoder_thread_config = PlayerVideoDecoderThreadConfig::default();
        let capabilities = capabilities_with_playable_outputs(vec![
            non_vaapi_dma_buf_output(),
            current_vaapi_output(),
        ]);

        let plan = select_video_pipeline_plan(
            VideoBackendPreference::Vaapi,
            Some(&capabilities),
            decoder_thread_config,
        )
        .expect("vaapi preference should select VA-API when present");

        assert_eq!(
            plan,
            VideoPipelinePlan::VaapiDmaBufWgpu {
                decoder_thread_config,
            }
        );
    }

    #[test]
    fn missing_vaapi_in_vaapi_preference_is_explicit_error() {
        let capabilities = capabilities_with_playable_outputs(vec![non_vaapi_dma_buf_output()]);

        let error = select_video_pipeline_plan(
            VideoBackendPreference::Vaapi,
            Some(&capabilities),
            PlayerVideoDecoderThreadConfig::default(),
        )
        .expect_err("vaapi preference must not fall back to another backend");

        assert_eq!(
            error,
            VideoPipelineSelectionError::PreferredVaapiDmaBufUnavailable
        );
    }

    #[test]
    fn vaapi_preference_rejects_vaapi_without_dma_buf_transfer() {
        let capabilities = capabilities_with_playable_outputs(vec![host_upload_vaapi_output()]);

        let error = select_video_pipeline_plan(
            VideoBackendPreference::Vaapi,
            Some(&capabilities),
            PlayerVideoDecoderThreadConfig::default(),
        )
        .expect_err("VA-API startup requires current DMA-BUF materializer path");

        assert_eq!(
            error,
            VideoPipelineSelectionError::PreferredVaapiDmaBufUnavailable
        );
    }

    #[test]
    fn missing_capability_snapshot_is_typed_error() {
        let error = select_video_pipeline_plan(
            VideoBackendPreference::Auto,
            None,
            PlayerVideoDecoderThreadConfig::default(),
        )
        .expect_err("selector cannot run before shell capability probe");

        assert_eq!(error, VideoPipelineSelectionError::CapabilitiesUnavailable);
    }
}
