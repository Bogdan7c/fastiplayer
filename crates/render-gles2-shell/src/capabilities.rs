//! Что умеет GLES2-рендер на этом драйвере (`RenderCapabilities`).
//!
//! Инвариант README плана: возможности выводятся из реально найденных возможностей драйвера.
//! Чего нет в списке, того выбор декодера не предложит: честное «не поддерживается», а не
//! чёрный экран.
//! - База (любой GLES 2.0): программный декод 8 бит 4:2:0, 4:2:2, 4:4:4 (сессия 05).
//! - Уровень «Точность» (`highp` во фрагментном шейдере): ещё 10/12 бит матрицы v1 wgpu
//!   (4:2:0/4:2:2 10/12, 4:4:4 10) и HDR → SDR BT.2446-C для них (сессия 06). Без `highp` код
//!   до 4095 и тон-маппинг в fp16 не посчитать — этих контрактов нет, и `auto` их не предложит.
//! - Аппаратный zero-copy — сессия 07.

use gles2_egui::Gles2Capabilities;
use gles2_video::ShaderPrecision;
use render_core::{
    HdrOutputMode, HdrToneMappingOperator, P010RenderReadiness, RenderBackendKind,
    RenderCapabilities, UiCompositionMode,
};
use video_frame_contract::{VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath};

/// Контракты программного декода 8 бит: есть на любом драйвере GLES 2.0.
pub const GLES2_SOFTWARE_FRAME_CONTRACTS: [VideoFrameContract; 3] = [
    software_host_upload(VideoFramePixelLayout::Yuv420Planar8),
    software_host_upload(VideoFramePixelLayout::Yuv422Planar8),
    software_host_upload(VideoFramePixelLayout::Yuv444Planar8),
];

/// Контракты программного декода 10/12 бит (матрица v1 wgpu): только при `highp`.
pub const GLES2_HIGH_BIT_SOFTWARE_FRAME_CONTRACTS: [VideoFrameContract; 5] = [
    software_host_upload(VideoFramePixelLayout::Yuv420Planar10Le),
    software_host_upload(VideoFramePixelLayout::Yuv420Planar12Le),
    software_host_upload(VideoFramePixelLayout::Yuv422Planar10Le),
    software_host_upload(VideoFramePixelLayout::Yuv422Planar12Le),
    software_host_upload(VideoFramePixelLayout::Yuv444Planar10Le),
];

const fn software_host_upload(pixel_layout: VideoFramePixelLayout) -> VideoFrameContract {
    VideoFrameContract {
        pixel_layout,
        transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
    }
}

/// Возможности GLES2-рендера для найденного драйвера.
#[must_use]
pub fn gles2_render_capabilities(capabilities: &Gles2Capabilities) -> RenderCapabilities {
    gles2_render_capabilities_for(
        ShaderPrecision::for_capabilities(capabilities),
        capabilities.max_texture_size(),
    )
}

/// Возможности GLES2-рендера при точности шейдера `precision` и `GL_MAX_TEXTURE_SIZE`
/// `driver_texture_limit` (чистая функция: её проверяют и для драйвера без `highp`).
///
/// Предельный размер кадра — лимит драйвера; без `highp` ещё и 2048 (в fp16 номер текселя
/// точен только до этой ширины).
#[must_use]
pub fn gles2_render_capabilities_for(
    precision: ShaderPrecision,
    driver_texture_limit: u32,
) -> RenderCapabilities {
    let max_texture_size = precision
        .exact_texel_limit()
        .map_or(driver_texture_limit, |precision_limit| {
            driver_texture_limit.min(precision_limit)
        });
    let high_bit = precision.supports_high_bit_depth();
    let mut supported_frame_contracts = GLES2_SOFTWARE_FRAME_CONTRACTS.to_vec();
    if high_bit {
        supported_frame_contracts.extend(GLES2_HIGH_BIT_SOFTWARE_FRAME_CONTRACTS);
    }
    let (display_name, supported_hdr_to_sdr_operators) = if high_bit {
        (
            "OpenGL ES 2.0 HostPlanar YUV 8/10/12-bit renderer with BT.2446-C HDR-to-SDR",
            vec![HdrToneMappingOperator::Bt2446C],
        )
    } else {
        ("OpenGL ES 2.0 HostPlanar YUV 8-bit renderer", Vec::new())
    };
    RenderCapabilities {
        backend: RenderBackendKind::OpenGles,
        display_name: display_name.to_owned(),
        supported_frame_contracts,
        p010_render_readiness: P010RenderReadiness::Unavailable,
        supported_hdr_to_sdr_operators,
        hdr_output_mode: HdrOutputMode::SdrBt709Only,
        supports_hdr_to_sdr: high_bit,
        supports_native_hdr_output: false,
        max_texture_size: Some(max_texture_size),
        advanced_ui: true,
        ui_composition_mode: UiCompositionMode::Overlay,
        present_timing_metrics: true,
    }
}

#[cfg(test)]
mod tests;
