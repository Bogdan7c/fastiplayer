//! WGPU render backend.
//!
//! Наружу crate отдаёт фасад `WgpuVideoRenderer` и backend-specific view wrapper.
//! Конкретный NV12 shader остаётся приватной деталью backend-а, чтобы app shell
//! не зависел от имени и внутренней структуры renderer implementation.

#![forbid(unsafe_code)]

use anyhow::{Result, bail};
use codec_core::{BitDepth, ChromaSubsampling};
use render_core::{
    ColorPipelineSettings, RenderCapabilities, RenderDiagnostics, RenderableFrame, VideoFrameFormat,
};

mod color_pipeline;
mod nv12_renderer;
mod shell;

use nv12_renderer::Nv12VideoRenderer;

pub use shell::{GpuContext, RenderFrameDropReason, RenderFrameOutcome, Renderer};

/// Backend-specific texture resources для одного кадра.
pub enum WgpuFramePlanes<'frame> {
    /// NV12 frame: отдельная luma plane и interleaved chroma plane.
    Nv12 {
        /// Texture view с Y/luma plane.
        y_view: &'frame wgpu::TextureView,

        /// Texture view с interleaved UV/chroma plane.
        uv_view: &'frame wgpu::TextureView,
    },
}

/// Кадр, готовый к рендерингу через WGPU backend.
pub struct WgpuRenderableFrame<'frame> {
    /// Renderer-neutral metadata, используемая capability layer и diagnostics.
    pub metadata: RenderableFrame,

    /// WGPU texture views, соответствующие `metadata.format`.
    pub planes: WgpuFramePlanes<'frame>,
}

impl<'frame> WgpuRenderableFrame<'frame> {
    /// Собирает WGPU frame wrapper из decoded NV12 frame и texture views.
    #[must_use]
    pub fn from_decoded_nv12(
        frame: &video_core::DecodedFrame,
        y_view: &'frame wgpu::TextureView,
        uv_view: &'frame wgpu::TextureView,
    ) -> Self {
        Self {
            metadata: RenderableFrame {
                handle: frame.texture_handle.0,
                pts: frame.pts,
                format: VideoFrameFormat::Nv12,
                bit_depth: BitDepth::Eight,
                chroma: ChromaSubsampling::Yuv420,
                coded_width: frame.width,
                coded_height: frame.height,
                render_width: frame.render_width,
                render_height: frame.render_height,
                color: frame.color.clone(),
            },
            planes: WgpuFramePlanes::Nv12 { y_view, uv_view },
        }
    }
}

/// Высокоуровневый WGPU video renderer.
pub struct WgpuVideoRenderer {
    /// Приватный renderer текущего MVP NV12 path.
    nv12_renderer: Nv12VideoRenderer,

    /// Снимок возможностей backend-а для capability report и stream selection.
    capabilities: RenderCapabilities,

    /// Последняя renderer-neutral диагностика без GPU handles.
    diagnostics: RenderDiagnostics,
}

impl WgpuVideoRenderer {
    /// Создаёт WGPU renderer для заданного swapchain format.
    #[must_use]
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let max_texture_size = Some(device.limits().max_texture_dimension_2d);

        Self {
            nv12_renderer: Nv12VideoRenderer::new(device, surface_format),
            capabilities: RenderCapabilities::wgpu_nv12(max_texture_size),
            diagnostics: RenderDiagnostics::default(),
        }
    }

    /// Возвращает renderer capabilities без доступа к backend internals.
    #[must_use]
    pub const fn capabilities(&self) -> &RenderCapabilities {
        &self.capabilities
    }

    /// Возвращает последнюю диагностику renderer-а без backend-specific handles.
    #[must_use]
    pub const fn diagnostics(&self) -> &RenderDiagnostics {
        &self.diagnostics
    }

    /// Обновляет color pipeline settings для всех текущих video paths.
    pub fn set_color_pipeline_settings(&mut self, settings: ColorPipelineSettings) {
        self.nv12_renderer.set_color_pipeline_settings(settings);
    }

    /// Обновляет размер swapchain для расчёта letterbox.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.nv12_renderer.set_window_size(width, height);
    }

    /// Рендерит video frame или очищает target в чёрный цвет, если кадра нет.
    ///
    /// Возвращает `true`, если video pass реально нарисовал кадр.
    pub fn render_or_clear(
        &mut self,
        frame: Option<&WgpuRenderableFrame<'_>>,
        target: &wgpu::TextureView,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<bool> {
        let Some(frame) = frame else {
            self.diagnostics.active_color_path = None;
            clear_to_black(target, encoder);
            return Ok(false);
        };
        self.diagnostics.active_color_path = None;

        if !frame.metadata.has_display_size() {
            bail!(
                "renderable frame has invalid display size: {}x{}",
                frame.metadata.render_width,
                frame.metadata.render_height
            );
        }

        match (&frame.metadata.format, &frame.planes) {
            (VideoFrameFormat::Nv12, WgpuFramePlanes::Nv12 { y_view, uv_view }) => {
                let active_color_path = self.nv12_renderer.render_frame(
                    &frame.metadata,
                    y_view,
                    uv_view,
                    target,
                    encoder,
                    device,
                    queue,
                )?;
                self.diagnostics.active_color_path = Some(active_color_path);
                Ok(true)
            }
            (format, _) => {
                bail!("WGPU renderer received unsupported frame format: {format}");
            }
        }
    }
}

/// Очищает swapchain target в чёрный цвет, когда video frame ещё не готов.
pub fn clear_to_black(target: &wgpu::TextureView, encoder: &mut wgpu::CommandEncoder) {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("clear to black pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}
