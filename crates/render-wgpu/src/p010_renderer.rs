use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail, ensure};
use codec_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, MatrixCoefficients, TransferFunction,
    VideoColorMetadata,
};
use render_core::{
    ActiveColorPath, ColorPipelineSettings, HdrToSdrSettings, RenderableFrame, VideoFrameFormat,
};

/// WGSL source dedicated to P010 rendering.
pub(crate) const P010_SHADER_SOURCE: &str = include_str!("../shaders/p010_bt2446c_to_sdr.wgsl");

/// Размер P010 uniform buffer-а, который должен совпадать с WGSL layout.
const P010_RENDERER_UNIFORM_SIZE: u64 = std::mem::size_of::<P010RendererUniforms>() as u64;

/// Shader mode для 10-bit SDR BT.709 P010 path.
const P010_SHADER_MODE_SDR_BT709: u32 = 0;

/// Shader mode для будущего HDR-to-SDR BT.2446-C P010 path.
const P010_SHADER_MODE_HDR_BT2446C: u32 = 1;

/// Uniform buffer для P010 renderer skeleton-а.
///
/// Layout держит 16-byte alignment WGSL uniform rules:
/// - две `vec2<f32>` в начале вместе занимают первые 16 байт;
/// - mode и HDR reference values представлены как `vec4`.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct P010RendererUniforms {
    /// Масштаб UV для letterbox.
    uv_scale: [f32; 2],

    /// Смещение UV для letterbox.
    uv_offset: [f32; 2],

    /// `x`: shader branch, `yzw`: reserved для стабильного layout.
    shader_mode: [u32; 4],

    /// `x`: SDR white nits, `y`: HDR peak nits, `zw`: reserved.
    hdr_reference_nits: [f32; 4],
}

/// Выбранный color branch внутри P010 renderer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum P010RenderColorPath {
    /// P010 10-bit SDR BT.709 path без HDR tone mapping.
    SdrBt709,

    /// P010 HDR path, зарезервированный под BT.2446-C shader implementation.
    HdrBt2446C,
}

impl P010RenderColorPath {
    /// Возвращает числовой режим, который читает WGSL shader.
    const fn shader_mode(self) -> u32 {
        match self {
            Self::SdrBt709 => P010_SHADER_MODE_SDR_BT709,
            Self::HdrBt2446C => P010_SHADER_MODE_HDR_BT2446C,
        }
    }
}

/// CPU-side данные, подготовленные для одного P010 draw call.
struct PreparedP010Render {
    /// Uniforms для текущего кадра.
    uniforms: P010RendererUniforms,

    /// Диагностический color path для UI/telemetry.
    active_path: ActiveColorPath,
}

/// Приватный renderer для P010 decoded frames.
pub(crate) struct P010VideoRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    color_settings: ColorPipelineSettings,
    hdr_to_sdr_settings: HdrToSdrSettings,
    window_size: (u32, u32),
}

/// Возвращает non-zero binding size для P010 uniform buffer-а.
fn p010_uniform_binding_size() -> NonZeroU64 {
    // Инвариант защищён layout test-ом `p010_renderer_uniforms_match_wgsl_uniform_layout`.
    NonZeroU64::new(P010_RENDERER_UNIFORM_SIZE)
        .expect("размер P010 renderer uniform buffer должен быть non-zero")
}

impl P010VideoRenderer {
    /// Создаёт pipeline и immutable GPU state для P010 shader path.
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("p010 bt2446c to sdr shader"),
            source: wgpu::ShaderSource::Wgsl(P010_SHADER_SOURCE.into()),
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("p010 uniform buffer"),
            size: P010_RENDERER_UNIFORM_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("p010 sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("p010 bind group layout"),
            entries: &[
                // Uniform buffer держит letterbox state и HDR branch parameters.
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: Some(p010_uniform_binding_size()),
                    },
                    count: None,
                },
                // Один sampler используется для обеих normalized P010 plane views.
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Texture view для P010 luma plane: R16Unorm plane или Plane0 view.
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Texture view для P010 interleaved UV plane: Rg16Unorm plane или Plane1 view.
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("p010 pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("p010 render pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            bind_group_layout,
            uniform_buffer,
            sampler,
            color_settings: ColorPipelineSettings::default(),
            hdr_to_sdr_settings: HdrToSdrSettings::default(),
            window_size: (1280, 720),
        }
    }

    /// Обновляет размер surface для расчёта letterbox.
    pub fn set_window_size(&mut self, width: u32, height: u32) {
        self.window_size = (width, height);
    }

    /// Обновляет color settings, которые P010 diagnostics наследуют от общего renderer config.
    pub fn set_color_pipeline_settings(&mut self, color_settings: ColorPipelineSettings) {
        self.color_settings = color_settings;
    }

    /// Рендерит P010 frame через отдельный P010 shader module.
    pub fn render_frame(
        &mut self,
        frame: &RenderableFrame,
        y_view: &wgpu::TextureView,
        uv_view: &wgpu::TextureView,
        target: &wgpu::TextureView,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<ActiveColorPath> {
        let prepared_p010_render = prepare_p010_render(
            frame,
            &self.color_settings,
            self.hdr_to_sdr_settings,
            self.window_size,
        )?;
        log_first_p010_render_dispatch(frame, &prepared_p010_render);

        queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&prepared_p010_render.uniforms),
        );

        // Bind group создаётся на кадр, потому что plane views приходят из decoder pool.
        // Renderer получает только пару views и не знает, какой storage layout был импортирован.
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("p010 bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(uv_view),
                },
            ],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("p010 video pass"),
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

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);

        Ok(prepared_p010_render.active_path)
    }
}

/// Собирает CPU-side state для P010 render pass.
fn prepare_p010_render(
    frame: &RenderableFrame,
    color_settings: &ColorPipelineSettings,
    hdr_to_sdr_settings: HdrToSdrSettings,
    window_size: (u32, u32),
) -> Result<PreparedP010Render> {
    validate_p010_renderable_frame(frame)?;

    let color_path = select_p010_color_path(frame)?;
    let (uv_scale, uv_offset) = letterbox_scale_and_offset(frame, window_size);
    let active_path =
        active_color_path_for_p010(frame, color_settings, hdr_to_sdr_settings, color_path);

    Ok(PreparedP010Render {
        uniforms: P010RendererUniforms {
            uv_scale,
            uv_offset,
            shader_mode: [color_path.shader_mode(), 0, 0, 0],
            hdr_reference_nits: [
                hdr_to_sdr_settings.sdr_reference_white_nits,
                hdr_to_sdr_settings.hdr_reference_peak_nits,
                0.0,
                0.0,
            ],
        },
        active_path,
    })
}

/// Логирует первый P010 renderer dispatch для ручной проверки Phase 10 Session 2.
fn log_first_p010_render_dispatch(frame: &RenderableFrame, prepared_render: &PreparedP010Render) {
    static LOGGED_P010_RENDER_DISPATCH: AtomicBool = AtomicBool::new(false);

    if LOGGED_P010_RENDER_DISPATCH.swap(true, Ordering::Relaxed) {
        return;
    }

    tracing::info!(
        handle = frame.handle,
        coded_width = frame.coded_width,
        coded_height = frame.coded_height,
        active_color_path = %prepared_render.active_path.diagnostic_text(),
        shader_mode = prepared_render.uniforms.shader_mode[0],
        "P010 renderer dispatch selected"
    );
}

/// Проверяет renderer-neutral P010 contract перед созданием bind group.
fn validate_p010_renderable_frame(frame: &RenderableFrame) -> Result<()> {
    ensure!(
        frame.format == VideoFrameFormat::P010,
        "P010 renderer received {} frame",
        frame.format
    );
    ensure!(
        frame.bit_depth == BitDepth::Ten,
        "P010 renderer requires 10-bit input, got {}",
        frame.bit_depth
    );
    ensure!(
        frame.chroma == ChromaSubsampling::Yuv420,
        "P010 renderer requires 4:2:0 input, got {}",
        frame.chroma
    );
    ensure!(
        frame.has_display_size(),
        "P010 renderer received invalid display size: {}x{}",
        frame.render_width,
        frame.render_height
    );

    Ok(())
}

/// Выбирает P010 color branch из typed metadata.
pub(crate) fn select_p010_color_path(frame: &RenderableFrame) -> Result<P010RenderColorPath> {
    validate_p010_renderable_frame(frame)?;

    if is_sdr_bt709_p010(&frame.color) {
        return Ok(P010RenderColorPath::SdrBt709);
    }

    if is_phase10_hdr_p010(&frame.color) {
        return Ok(P010RenderColorPath::HdrBt2446C);
    }

    bail!(
        "unsupported P010 color metadata: primaries={:?}, matrix={:?}, transfer={:?}, hdr_metadata={}",
        frame.color.primaries,
        frame.color.matrix,
        frame.color.transfer,
        frame.color.hdr_metadata.is_some()
    );
}

/// Проверяет P010 SDR BT.709 path, который не должен идти через BT.2446-C.
fn is_sdr_bt709_p010(color: &VideoColorMetadata) -> bool {
    color.hdr_metadata.is_none()
        && color.primaries == ColorPrimaries::Bt709
        && color.matrix == MatrixCoefficients::Bt709
        && color.transfer == TransferFunction::Bt709
}

/// Проверяет Phase 10 HDR candidate branch для будущего BT.2446-C shader-а.
fn is_phase10_hdr_p010(color: &VideoColorMetadata) -> bool {
    color.primaries == ColorPrimaries::Bt2020
        && color.matrix == MatrixCoefficients::Bt2020
        && matches!(color.transfer, TransferFunction::Pq | TransferFunction::Hlg)
}

/// Строит active path так, чтобы SDR BT.709 P010 не получал HDR-to-SDR marker.
fn active_color_path_for_p010(
    frame: &RenderableFrame,
    color_settings: &ColorPipelineSettings,
    hdr_to_sdr_settings: HdrToSdrSettings,
    color_path: P010RenderColorPath,
) -> ActiveColorPath {
    match color_path {
        P010RenderColorPath::SdrBt709 => ActiveColorPath::from_frame(frame, color_settings),
        P010RenderColorPath::HdrBt2446C => ActiveColorPath::from_frame_with_hdr_to_sdr(
            frame,
            color_settings,
            Some(hdr_to_sdr_settings),
        ),
    }
}

/// Считает letterbox scale/offset в тех же координатах, что и NV12 shader.
fn letterbox_scale_and_offset(
    frame: &RenderableFrame,
    window_size: (u32, u32),
) -> ([f32; 2], [f32; 2]) {
    let video_aspect = frame.render_width as f32 / frame.render_height.max(1) as f32;
    let window_aspect = window_size.0 as f32 / window_size.1.max(1) as f32;

    if video_aspect > window_aspect {
        let scale_y = window_aspect / video_aspect;
        ([1.0, scale_y], [0.0, (1.0 - scale_y) * 0.5])
    } else {
        let scale_x = video_aspect / window_aspect;
        ([scale_x, 1.0], [(1.0 - scale_x) * 0.5, 0.0])
    }
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};
    use std::time::Duration;

    use codec_core::{
        ColorMetadataConfidence, ColorMetadataOrigin, ColorRange, HdrMetadata, MatrixCoefficients,
    };
    use render_core::{HdrToneMappingOperator, SwapchainTransferMode};

    use super::*;

    #[test]
    fn p010_renderer_uniforms_match_wgsl_uniform_layout() {
        assert_eq!(align_of::<P010RendererUniforms>(), 16);
        assert_eq!(
            size_of::<P010RendererUniforms>(),
            P010_RENDERER_UNIFORM_SIZE as usize
        );
        assert_eq!(offset_of!(P010RendererUniforms, uv_scale), 0);
        assert_eq!(offset_of!(P010RendererUniforms, uv_offset), 8);
        assert_eq!(offset_of!(P010RendererUniforms, shader_mode), 16);
        assert_eq!(offset_of!(P010RendererUniforms, hdr_reference_nits), 32);
    }

    #[test]
    fn p010_sdr_bt709_uses_non_hdr_color_branch() {
        let frame = p010_test_frame(VideoColorMetadata::sdr_bt709_limited());

        let color_path = select_p010_color_path(&frame).expect("P010 SDR BT.709 accepted");
        let active_path = active_color_path_for_p010(
            &frame,
            &ColorPipelineSettings::default(),
            HdrToSdrSettings::default(),
            color_path,
        );

        assert_eq!(color_path, P010RenderColorPath::SdrBt709);
        assert_eq!(active_path.hdr_to_sdr, None);
        assert!(!active_path.diagnostic_text().contains("bt2446-c"));
    }

    #[test]
    fn p010_hdr_pq_uses_hdr_color_branch() {
        let frame = p010_test_frame(p010_hdr_bt2020_color(TransferFunction::Pq));
        let color_settings = ColorPipelineSettings {
            swapchain_transfer: SwapchainTransferMode::ExplicitShaderOetf,
            ..ColorPipelineSettings::default()
        };

        let color_path = select_p010_color_path(&frame).expect("P010 HDR accepted");
        let active_path = active_color_path_for_p010(
            &frame,
            &color_settings,
            HdrToSdrSettings::default(),
            color_path,
        );

        assert_eq!(color_path, P010RenderColorPath::HdrBt2446C);
        assert_eq!(
            active_path.hdr_to_sdr.map(|settings| settings.operator),
            Some(HdrToneMappingOperator::Bt2446C)
        );
        assert!(active_path.diagnostic_text().contains("bt2446-c"));
    }

    #[test]
    fn p010_wide_gamut_sdr_is_rejected_until_gamut_mapping_is_explicit() {
        let frame = p010_test_frame(VideoColorMetadata {
            range: ColorRange::Limited,
            matrix: MatrixCoefficients::Bt2020,
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Bt709,
            hdr_metadata: None,
            origin: ColorMetadataOrigin::Container,
            confidence: ColorMetadataConfidence::Hint,
        });

        let error = select_p010_color_path(&frame).expect_err("BT.2020 SDR P010 is not implicit");

        assert!(
            error
                .to_string()
                .contains("unsupported P010 color metadata"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn p010_shader_source_is_dedicated_file() {
        let nv12_shader_source = include_str!("../shaders/nv12_to_rgba.wgsl");

        assert!(!P010_SHADER_SOURCE.trim().is_empty());
        assert_ne!(P010_SHADER_SOURCE, nv12_shader_source);
        assert!(P010_SHADER_SOURCE.contains("p010"));
    }

    fn p010_test_frame(color: VideoColorMetadata) -> RenderableFrame {
        RenderableFrame {
            handle: 42,
            pts: Duration::ZERO,
            format: VideoFrameFormat::P010,
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv420,
            coded_width: 1920,
            coded_height: 1080,
            render_width: 1920,
            render_height: 1080,
            color,
        }
    }

    fn p010_hdr_bt2020_color(transfer: TransferFunction) -> VideoColorMetadata {
        VideoColorMetadata {
            range: ColorRange::Limited,
            matrix: MatrixCoefficients::Bt2020,
            primaries: ColorPrimaries::Bt2020,
            transfer,
            hdr_metadata: Some(HdrMetadata {
                color_primaries: ColorPrimaries::Bt2020,
                transfer_function: transfer,
                max_luminance_nits: Some(1_000.0),
                min_luminance_nits: Some(0.01),
                max_content_light_level_nits: Some(1_000),
                max_frame_average_light_level_nits: Some(400),
            }),
            origin: ColorMetadataOrigin::Bitstream,
            confidence: ColorMetadataConfidence::Confirmed,
        }
    }
}
