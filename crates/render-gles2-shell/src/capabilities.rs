//! Что умеет GLES2-рендер на этом драйвере (`RenderCapabilities`).
//!
//! Инвариант README плана: возможности выводятся из реально найденных возможностей драйвера.
//! Сессия 05 объявляет ровно три контракта программного декода 8 бит; высокая битность — 06,
//! аппаратный zero-copy — 07. Чего нет в списке, того выбор декодера не предложит: честное «не
//! поддерживается», а не чёрный экран.

use gles2_egui::Gles2Capabilities;
use gles2_video::ShaderPrecision;
use render_core::{
    HdrOutputMode, P010RenderReadiness, RenderBackendKind, RenderCapabilities, UiCompositionMode,
};
use video_frame_contract::{VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath};

/// Контракты программного декода, которые GLES2-рендер принимает в сессии 05: планарный YUV
/// 8 бит 4:2:0, 4:2:2 и 4:4:4 с загрузкой из памяти процесса.
pub const GLES2_SOFTWARE_FRAME_CONTRACTS: [VideoFrameContract; 3] = [
    software_host_upload(VideoFramePixelLayout::Yuv420Planar8),
    software_host_upload(VideoFramePixelLayout::Yuv422Planar8),
    software_host_upload(VideoFramePixelLayout::Yuv444Planar8),
];

const fn software_host_upload(pixel_layout: VideoFramePixelLayout) -> VideoFrameContract {
    VideoFrameContract {
        pixel_layout,
        transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
    }
}

/// Возможности GLES2-рендера для найденного драйвера.
///
/// Предельный размер кадра — `GL_MAX_TEXTURE_SIZE`; без `highp` во фрагментном шейдере ещё и
/// 2048 (в fp16 номер текселя точен только до этой ширины). HDR и 10/12 бит не объявляются.
#[must_use]
pub fn gles2_render_capabilities(capabilities: &Gles2Capabilities) -> RenderCapabilities {
    let driver_limit = capabilities.max_texture_size();
    let max_texture_size = ShaderPrecision::for_capabilities(capabilities)
        .exact_texel_limit()
        .map_or(driver_limit, |precision_limit| {
            driver_limit.min(precision_limit)
        });
    RenderCapabilities {
        backend: RenderBackendKind::OpenGles,
        display_name: "OpenGL ES 2.0 HostPlanar YUV 8-bit renderer".to_owned(),
        supported_frame_contracts: GLES2_SOFTWARE_FRAME_CONTRACTS.to_vec(),
        p010_render_readiness: P010RenderReadiness::Unavailable,
        supported_hdr_to_sdr_operators: Vec::new(),
        hdr_output_mode: HdrOutputMode::SdrBt709Only,
        supports_hdr_to_sdr: false,
        supports_native_hdr_output: false,
        max_texture_size: Some(max_texture_size),
        advanced_ui: true,
        ui_composition_mode: UiCompositionMode::Overlay,
        present_timing_metrics: true,
    }
}
