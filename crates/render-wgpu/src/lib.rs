//! WGPU render backend.
//!
//! Наружу crate отдаёт фасад `WgpuVideoRenderer` и backend-specific view wrapper.
//! Конкретный NV12 shader остаётся приватной деталью backend-а, чтобы app shell
//! не зависел от имени и внутренней структуры renderer implementation.

#![forbid(unsafe_code)]

use anyhow::{Result, bail, ensure};
use codec_core::{ColorPrimaries, MatrixCoefficients, TransferFunction};
use render_core::{
    ColorPipelineSettings, RenderCapabilities, RenderDiagnostics, RenderableFrame, VideoFrameFormat,
};
use video_core::{DecodedFrame, DecodedPixelFormat, FrameMemoryPath};

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

        Ok(Self {
            metadata: renderable_metadata_from_decoded(frame, VideoFrameFormat::P010),
            planes: WgpuFramePlanes::P010 { y_view, uv_view },
        })
    }
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

    /// Снимок возможностей backend-а для capability report и stream selection.
    capabilities: RenderCapabilities,

    /// Последняя renderer-neutral диагностика без GPU handles.
    diagnostics: RenderDiagnostics,

    /// Уже логировали P010 boundary frame, который пока нельзя вывести на экран.
    p010_render_unavailable_logged: bool,
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
            p010_render_unavailable_logged: false,
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
            (VideoFrameFormat::P010, WgpuFramePlanes::P010 { .. }) => {
                if !self.p010_render_unavailable_logged {
                    self.p010_render_unavailable_logged = true;
                    tracing::warn!("{}", p010_boundary_manual_diagnostic_text(&frame.metadata));
                }
                clear_to_black(target, encoder);
                Ok(false)
            }
            (format, _) => {
                bail!("WGPU renderer frame metadata/plane mismatch for format: {format}");
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

/// Формирует финальную строку ручной Phase 9 диагностики для P010 boundary.
///
/// Сейчас единственный P010 producer в проекте - VP9 Profile 2. Когда AV1/H.265
/// начнут отдавать P010, этот текст нужно заменить codec-aware diagnostic-ом.
fn p010_boundary_manual_diagnostic_text(frame: &RenderableFrame) -> String {
    format!(
        "P010 zero-copy boundary verified: VP9 Profile2 {} {} {} {}\nHDR-to-SDR renderer unavailable until Phase 10",
        frame.bit_depth,
        bt2020_boundary_label(frame),
        hdr_transfer_contract_label(frame.color.transfer),
        chroma_contract_label(frame.chroma),
    )
}

/// Возвращает BT.2020 label только когда primaries и matrix совпали со strict core.
fn bt2020_boundary_label(frame: &RenderableFrame) -> &'static str {
    if frame.color.primaries == ColorPrimaries::Bt2020
        && frame.color.matrix == MatrixCoefficients::Bt2020
    {
        "BT.2020"
    } else {
        "non-BT.2020"
    }
}

/// Возвращает stable HDR transfer contract label для ручной Phase 9 проверки.
fn hdr_transfer_contract_label(transfer: TransferFunction) -> &'static str {
    match transfer {
        TransferFunction::Pq | TransferFunction::Hlg => "PQ/HLG",
        TransferFunction::Bt709 => "BT.709",
        TransferFunction::Srgb => "sRGB",
        TransferFunction::Unknown => "unknown-transfer",
    }
}

/// Возвращает chroma label в форме, ожидаемой manual diagnostics.
fn chroma_contract_label(chroma: codec_core::ChromaSubsampling) -> &'static str {
    match chroma {
        codec_core::ChromaSubsampling::Yuv420 => "YUV420",
        codec_core::ChromaSubsampling::Yuv422 => "YUV422",
        codec_core::ChromaSubsampling::Yuv444 => "YUV444",
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use codec_core::{
        BitDepth, ChromaSubsampling, ColorMetadataOrigin, ColorPrimaries, ColorRange,
        MatrixCoefficients, TransferFunction, VideoColorMetadata,
    };

    #[test]
    fn p010_boundary_manual_diagnostic_uses_phase9_contract_text_for_pq() {
        let frame = p010_test_frame(TransferFunction::Pq);

        assert_eq!(
            p010_boundary_manual_diagnostic_text(&frame),
            "P010 zero-copy boundary verified: VP9 Profile2 10-bit BT.2020 PQ/HLG YUV420\nHDR-to-SDR renderer unavailable until Phase 10"
        );
    }

    #[test]
    fn p010_boundary_manual_diagnostic_uses_same_contract_text_for_hlg() {
        let frame = p010_test_frame(TransferFunction::Hlg);

        assert_eq!(
            p010_boundary_manual_diagnostic_text(&frame),
            "P010 zero-copy boundary verified: VP9 Profile2 10-bit BT.2020 PQ/HLG YUV420\nHDR-to-SDR renderer unavailable until Phase 10"
        );
    }

    /// Создаёт renderer-neutral P010 frame без GPU resources.
    fn p010_test_frame(transfer: TransferFunction) -> RenderableFrame {
        RenderableFrame {
            handle: 1,
            pts: Duration::ZERO,
            format: VideoFrameFormat::P010,
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv420,
            coded_width: 3840,
            coded_height: 2160,
            render_width: 3840,
            render_height: 2160,
            color: VideoColorMetadata {
                range: ColorRange::Limited,
                matrix: MatrixCoefficients::Bt2020,
                primaries: ColorPrimaries::Bt2020,
                transfer,
                hdr_metadata: None,
                origin: ColorMetadataOrigin::Container,
                confidence: codec_core::ColorMetadataConfidence::Hint,
            },
        }
    }
}
