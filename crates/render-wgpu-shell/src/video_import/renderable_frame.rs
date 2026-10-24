//! Сборка wgpu-кадра для video pass-а из метаданных декодера и views ключа.
//!
//! Перенесено из `app-egui::frame_prepare::build_render_input_video_frame` (сессия 02 плана
//! `user/gles2-renderer/`): выбор шейдерного пути по формату кадра — знание рендера, а не
//! приложения. Тексты ошибок прежние.

use anyhow::{Result, anyhow};
use render_wgpu_video::{WgpuFrameTextureViews, WgpuRenderableFrame};
use video_core::{DecodedFrame, DecodedPixelFormat};

/// Собирает `WgpuRenderableFrame` из кадра декодера и views, полученных от материализатора.
///
/// Ошибка — views не того вида для формата кадра (например, NV12 без DMA-BUF views) или
/// формат вне production-пути (RGBA8). Кадр, принятый здесь, рисуется video pass-ом.
pub(crate) fn renderable_frame_from_views<'frame>(
    decoded_frame: &'frame DecodedFrame,
    texture_views: &'frame WgpuFrameTextureViews,
) -> Result<WgpuRenderableFrame<'frame>> {
    match decoded_frame.format() {
        DecodedPixelFormat::Nv12 => match texture_views.dma_buf_views() {
            Some(plane_views) => WgpuRenderableFrame::from_decoded_nv12(decoded_frame, plane_views),
            None => Err(anyhow!(
                "NV12 decoded video surface requires DMA-BUF Y/UV texture views"
            )),
        },
        DecodedPixelFormat::P010 => match texture_views.dma_buf_views() {
            Some(plane_views) => WgpuRenderableFrame::from_decoded_p010(decoded_frame, plane_views),
            None => Err(anyhow!(
                "P010 decoded video surface requires DMA-BUF Y/UV texture views"
            )),
        },
        DecodedPixelFormat::Rgba8 => Err(anyhow!(
            "RGBA8 decoded video surface is not a production zero-copy render path"
        )),
        host_planar_layout if host_planar_layout.is_host_planar() => {
            // Views вместе с кэшем bind group слота загрузки: рендерер не пересоздаёт
            // bind group на каждый кадр, пока слот держит те же текстуры.
            match texture_views.host_planar_plane_views() {
                Some(plane_views) => {
                    WgpuRenderableFrame::from_decoded_host_planar(decoded_frame, plane_views)
                }
                None => Err(anyhow!(
                    "{} decoded video surface requires HostPlanar Y/U/V texture views",
                    host_planar_layout
                )),
            }
        }
        unsupported_layout => Err(anyhow!(
            "{} decoded video surface is not a production zero-copy render path",
            unsupported_layout
        )),
    }
}
