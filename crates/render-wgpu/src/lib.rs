//! WGPU render backend.
//!
//! Наружу crate отдаёт фасад `WgpuVideoRenderer` и backend-specific view wrapper.
//! Конкретный NV12 shader остаётся приватной деталью backend-а, чтобы app shell
//! не зависел от имени и внутренней структуры renderer implementation.

#![forbid(unsafe_code)]

use anyhow::{Result, bail, ensure};
use render_core::{
    ColorPipelineSettings, RenderCapabilities, RenderDiagnostics, RenderableFrame, VideoFrameFormat,
};
use video_core::{DecodedFrame, DecodedPixelFormat, FrameMemoryPath};

mod color_pipeline;
mod nv12_renderer;
mod p010_renderer;
mod shell;

use nv12_renderer::Nv12VideoRenderer;
use p010_renderer::P010VideoRenderer;

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

    /// P010 frame: отдельная luma plane и interleaved chroma plane в 10-bit контракте.
    P010 {
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
        frame: &DecodedFrame,
        y_view: &'frame wgpu::TextureView,
        uv_view: &'frame wgpu::TextureView,
    ) -> Result<Self> {
        frame.validate_contract()?;
        ensure!(
            frame.format == DecodedPixelFormat::Nv12,
            "from_decoded_nv12 received {} frame",
            frame.format
        );

        Ok(Self {
            metadata: renderable_metadata_from_decoded(frame, VideoFrameFormat::Nv12),
            planes: WgpuFramePlanes::Nv12 { y_view, uv_view },
        })
    }

    /// Собирает WGPU frame wrapper из P010 boundary frame и texture views.
    pub fn from_decoded_p010(
        frame: &DecodedFrame,
        y_view: &'frame wgpu::TextureView,
        uv_view: &'frame wgpu::TextureView,
    ) -> Result<Self> {
        validate_decoded_p010_frame(frame)?;

        Ok(Self {
            metadata: renderable_metadata_from_decoded(frame, VideoFrameFormat::P010),
            planes: WgpuFramePlanes::P010 { y_view, uv_view },
        })
    }
}

/// Проверяет P010 decoded frame до привязки backend-specific texture views.
fn validate_decoded_p010_frame(frame: &DecodedFrame) -> Result<()> {
    frame.validate_contract()?;
    ensure!(
        frame.format == DecodedPixelFormat::P010,
        "from_decoded_p010 received {} frame",
        frame.format
    );
    ensure!(
        frame.memory_path == FrameMemoryPath::DmaBufZeroCopy,
        "P010 WGPU boundary requires zero-copy memory path, got {}",
        frame.memory_path
    );

    Ok(())
}

/// Копирует renderer-neutral metadata из decoded frame без backend-specific handles.
fn renderable_metadata_from_decoded(
    frame: &DecodedFrame,
    format: VideoFrameFormat,
) -> RenderableFrame {
    RenderableFrame {
        handle: frame.texture_handle.0,
        pts: frame.pts,
        format,
        bit_depth: frame.bit_depth,
        chroma: frame.chroma,
        coded_width: frame.width,
        coded_height: frame.height,
        render_width: frame.render_width,
        render_height: frame.render_height,
        color: frame.color.clone(),
    }
}

/// Высокоуровневый WGPU video renderer.
pub struct WgpuVideoRenderer {
    /// Приватный renderer текущего MVP NV12 path.
    nv12_renderer: Nv12VideoRenderer,

    /// Приватный renderer P010 path с отдельным shader module.
    p010_renderer: P010VideoRenderer,

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
            p010_renderer: P010VideoRenderer::new(device, surface_format),
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
        self.p010_renderer.set_color_pipeline_settings(settings);
    }

    /// Обновляет размер swapchain для расчёта letterbox.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.nv12_renderer.set_window_size(width, height);
        self.p010_renderer.set_window_size(width, height);
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

        match select_renderer_dispatch(frame.metadata.format, frame.planes.kind())? {
            RendererDispatch::Nv12 => {
                let WgpuFramePlanes::Nv12 { y_view, uv_view } = &frame.planes else {
                    unreachable!("renderer dispatch was selected from plane kind");
                };
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
            RendererDispatch::P010 => {
                let WgpuFramePlanes::P010 { y_view, uv_view } = &frame.planes else {
                    unreachable!("renderer dispatch was selected from plane kind");
                };
                let active_color_path = self.p010_renderer.render_frame(
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
        }
    }
}

/// Упрощённый kind plane set без lifetime/GPU handles для dispatch logic и unit tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WgpuFramePlaneKind {
    /// Renderer-facing NV12 Y/UV plane pair.
    Nv12,

    /// Renderer-facing P010 Y/UV plane pair.
    P010,
}

impl WgpuFramePlanes<'_> {
    /// Возвращает kind plane set без раскрытия backend-specific handles.
    pub(crate) const fn kind(&self) -> WgpuFramePlaneKind {
        match self {
            Self::Nv12 { .. } => WgpuFramePlaneKind::Nv12,
            Self::P010 { .. } => WgpuFramePlaneKind::P010,
        }
    }
}

/// Конкретный renderer path, выбранный facade-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RendererDispatch {
    /// Текущий SDR/NV12 renderer.
    Nv12,

    /// Отдельный P010 renderer.
    P010,
}

/// Выбирает renderer path по renderer-neutral format и kind plane set.
fn select_renderer_dispatch(
    format: VideoFrameFormat,
    plane_kind: WgpuFramePlaneKind,
) -> Result<RendererDispatch> {
    match (format, plane_kind) {
        (VideoFrameFormat::Nv12, WgpuFramePlaneKind::Nv12) => Ok(RendererDispatch::Nv12),
        (VideoFrameFormat::P010, WgpuFramePlaneKind::P010) => Ok(RendererDispatch::P010),
        (format, _) => bail!("WGPU renderer frame metadata/plane mismatch for format: {format}"),
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata};

    #[test]
    fn p010_frame_dispatches_to_p010_renderer_path() {
        let dispatch = select_renderer_dispatch(VideoFrameFormat::P010, WgpuFramePlaneKind::P010)
            .expect("P010 frame dispatches");

        assert_eq!(dispatch, RendererDispatch::P010);
    }

    #[test]
    fn nv12_frame_dispatches_to_nv12_renderer_path() {
        let dispatch = select_renderer_dispatch(VideoFrameFormat::Nv12, WgpuFramePlaneKind::Nv12)
            .expect("NV12 frame dispatches");

        assert_eq!(dispatch, RendererDispatch::Nv12);
    }

    #[test]
    fn metadata_plane_mismatch_is_rejected_before_renderer_call() {
        let error = select_renderer_dispatch(VideoFrameFormat::P010, WgpuFramePlaneKind::Nv12)
            .expect_err("P010 metadata must not use NV12 planes");

        assert!(
            error.to_string().contains("metadata/plane mismatch"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn p010_boundary_rejects_non_zero_copy_memory_path() {
        let frame = decoded_p010_test_frame(FrameMemoryPath::CpuUpload);

        let error = validate_decoded_p010_frame(&frame).expect_err("P010 CPU path rejected");

        assert!(
            error.to_string().contains("zero-copy"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn p010_storage_layouts_map_to_same_renderer_plane_kind() {
        let baseline_separate_layer_kind =
            p010_storage_layout_renderer_plane_kind(P010StorageLayout::BaselineSeparateLayer);
        let compatibility_composed_kind =
            p010_storage_layout_renderer_plane_kind(P010StorageLayout::CompatibilityComposed);

        assert_eq!(baseline_separate_layer_kind, WgpuFramePlaneKind::P010);
        assert_eq!(compatibility_composed_kind, WgpuFramePlaneKind::P010);
        assert_eq!(baseline_separate_layer_kind, compatibility_composed_kind);
    }

    /// Тестовое описание P010 storage layout до renderer boundary.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum P010StorageLayout {
        /// Baseline Phase 10 path: отдельные `R16Unorm` и `Rg16Unorm` textures.
        BaselineSeparateLayer,

        /// Compatibility path: plane views из composed `TextureFormat::P010`.
        CompatibilityComposed,
    }

    /// Документирует, что renderer видит только P010 Y/UV pair, а не storage layout.
    const fn p010_storage_layout_renderer_plane_kind(
        _storage_layout: P010StorageLayout,
    ) -> WgpuFramePlaneKind {
        WgpuFramePlaneKind::P010
    }

    fn decoded_p010_test_frame(memory_path: FrameMemoryPath) -> DecodedFrame {
        DecodedFrame {
            pts: Duration::ZERO,
            format: DecodedPixelFormat::P010,
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv420,
            memory_path,
            width: 1920,
            height: 1080,
            render_width: 1920,
            render_height: 1080,
            color: VideoColorMetadata::sdr_bt709_limited(),
            texture_handle: video_core::FrameTextureHandle(7),
        }
    }
}
