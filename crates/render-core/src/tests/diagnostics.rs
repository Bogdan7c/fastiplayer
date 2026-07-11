use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata};
use video_frame_contract::VideoFramePixelLayout;

use super::*;
use crate::*;
#[test]
fn render_diagnostics_exposes_active_color_path_without_gpu_handles() {
    let settings = ColorPipelineSettings::default();
    let active_path = ActiveColorPath::from_parts(
        VideoFramePixelLayout::Nv12,
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
        VideoColorMetadata::sdr_bt709_limited(),
        &settings,
    );
    let diagnostics = RenderDiagnostics {
        active_color_path: Some(active_path),
        ..RenderDiagnostics::default()
    };

    assert_eq!(
        diagnostics.active_color_path_text().as_deref(),
        Some("NV12 8-bit BT.709 limited -> SDR BT.709 preserve-current-unorm")
    );
}
