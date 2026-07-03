use anyhow::{Context, Result, bail, ensure};
use codec_core::{BitDepth, ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction};
use render_core::{
    ActiveColorPath, ColorPipelineSettings, HdrReferenceDefaultDiagnostics, HdrToSdrSettings,
    RenderableFrame, SwapchainTransferMode,
};
use video_frame_contract::VideoFramePixelLayout;

use crate::color_pipeline::{COLOR_PIPELINE_UNIFORM_SIZE, prepare_nv12_color_pipeline};

use super::{VideoRenderPassContext, display_orientation_uv_transform, letterbox_scale_and_offset};

const HOST_YUV420_8BIT_SHADER_SOURCE: &str =
    include_str!("../../shaders/host_yuv420_8bit_to_rgba.wgsl");
const HOST_YUV420_16BIT_SHADER_SOURCE: &str =
    include_str!("../../shaders/host_yuv420_16bit_to_sdr.wgsl");
const HOST_YUV420_HIGH_BIT_UNIFORM_SIZE: u64 =
    std::mem::size_of::<HostYuv420HighBitUniforms>() as u64;
const HOST_YUV420_SHADER_MODE_SDR_BT709: u32 = 0;
const HOST_YUV420_SHADER_MODE_HDR_BT2446C: u32 = 1;
const HOST_YUV420_TRANSFER_MODE_SDR_BT709: u32 = 0;
const HOST_YUV420_TRANSFER_MODE_PQ: u32 = 1;
const HOST_YUV420_TRANSFER_MODE_HLG: u32 = 2;
const HDR_METADATA_MARKER_NOT_APPLICABLE: u32 = 0;
const HDR_METADATA_MARKER_CONFIRMED: u32 = 1;
const HDR_METADATA_MARKER_REFERENCE_DEFAULT: u32 = 2;

/// Uniform buffer для 10/12-bit HostPlanar YUV420 shader-а.
///
/// Layout повторяет WGSL uniform alignment: первые два `vec2<f32>` вместе занимают
/// 16 байт, остальные поля представлены `vec4`, чтобы offsets не зависели от
/// backend-а.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct HostYuv420HighBitUniforms {
    /// Масштаб UV для letterbox.
    uv_scale: [f32; 2],

    /// Смещение UV для letterbox.
    uv_offset: [f32; 2],

    /// Первая строка affine transform из display UV в source texture UV.
    orientation_transform_row0: [f32; 4],

    /// Вторая строка affine transform из display UV в source texture UV.
    orientation_transform_row1: [f32; 4],

    /// `x`: shader branch, `y`: transfer branch, `z`: source bit depth, `w`: reserved.
    shader_mode: [u32; 4],

    /// `x`: Y offset code, `y`: Y scale, `z/w`: допустимые Y code bounds.
    luma_range: [f32; 4],

    /// `x/z`: U/V center code, `y/w`: U/V scale.
    chroma_range: [f32; 4],

    /// `x`: SDR white, `y`: HDR peak, `z/w`: mastering max/min luminance.
    hdr_reference_nits: [f32; 4],

    /// `x`: MaxCLL, `y`: MaxFALL, `z/w`: reserved.
    content_light_levels: [f32; 4],

    /// `x/y`: mastering max/min markers, `z/w`: MaxCLL/MaxFALL markers.
    optional_metadata_markers: [u32; 4],
}

/// Диагностика HostPlanar render path-а без GPU handles.
pub(crate) struct HostPlanarYuvRenderFrameDiagnostics {
    /// Renderer-neutral описание выбранного color/HDR path-а.
    pub(crate) active_color_path: ActiveColorPath,

    /// Маркеры HDR reference defaults, если shader пошёл через HDR branch.
    pub(crate) hdr_reference_defaults: Option<HdrReferenceDefaultDiagnostics>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostYuv420ColorPath {
    SdrBt709,
    HdrBt2446C,
}

impl HostYuv420ColorPath {
    const fn shader_mode(self) -> u32 {
        match self {
            Self::SdrBt709 => HOST_YUV420_SHADER_MODE_SDR_BT709,
            Self::HdrBt2446C => HOST_YUV420_SHADER_MODE_HDR_BT2446C,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct HostYuv420RangeNormalization {
    luma_range: [f32; 4],
    chroma_range: [f32; 4],
}

struct HostYuv420OptionalMetadataUniforms {
    hdr_reference_nits: [f32; 4],
    content_light_levels: [f32; 4],
    optional_metadata_markers: [u32; 4],
    diagnostic_markers: Option<HdrReferenceDefaultDiagnostics>,
}

struct PreparedHostYuv420HighBitRender {
    uniforms: HostYuv420HighBitUniforms,
    active_path: ActiveColorPath,
    hdr_reference_defaults: Option<HdrReferenceDefaultDiagnostics>,
}

/// Renderer для HostPlanar YUV 4:2:0/4:2:2/4:4:4 path-а.
pub(crate) struct HostPlanarYuvVideoRenderer {
    /// Pipeline для R8Unorm Y/U/V textures.
    unorm8_pipeline: wgpu::RenderPipeline,

    /// Bind group layout для R8Unorm Y/U/V textures.
    unorm8_bind_group_layout: wgpu::BindGroupLayout,

    /// Uniform buffers 8-bit SDR path-а: отдельный на каждую pass-роль
    /// (main/overlay), см. `VideoRenderTargetLoad::uniform_slot`.
    unorm8_uniform_buffers: [wgpu::Buffer; super::VideoRenderTargetLoad::UNIFORM_SLOT_COUNT],

    /// Filtering sampler для 8-bit normalized plane textures.
    unorm8_sampler: wgpu::Sampler,

    /// Pipeline для R16Uint Y/U/V textures.
    uint16_pipeline: wgpu::RenderPipeline,

    /// Bind group layout для R16Uint Y/U/V textures.
    uint16_bind_group_layout: wgpu::BindGroupLayout,

    /// Uniform buffers 10/12-bit SDR/HDR path-а: отдельный на каждую pass-роль.
    uint16_uniform_buffers: [wgpu::Buffer; super::VideoRenderTargetLoad::UNIFORM_SLOT_COUNT],

    /// Live SDR color settings, общие с текущими NV12/P010 renderer-ами.
    color_settings: ColorPipelineSettings,

    /// Live HDR-to-SDR settings для high-bit host upload.
    hdr_to_sdr_settings: HdrToSdrSettings,
}

impl HostPlanarYuvVideoRenderer {
    /// Создаёт оба HostPlanar YUV420 pipeline-а.
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let unorm8_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("host yuv420 8-bit to rgba shader"),
            source: wgpu::ShaderSource::Wgsl(HOST_YUV420_8BIT_SHADER_SOURCE.into()),
        });
        let uint16_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("host yuv420 16-bit to sdr shader"),
            source: wgpu::ShaderSource::Wgsl(HOST_YUV420_16BIT_SHADER_SOURCE.into()),
        });
        let unorm8_uniform_buffers = super::create_pass_uniform_buffers(
            device,
            "host yuv420 8-bit uniform buffer",
            COLOR_PIPELINE_UNIFORM_SIZE,
        );
        let uint16_uniform_buffers = super::create_pass_uniform_buffers(
            device,
            "host yuv420 16-bit uniform buffer",
            HOST_YUV420_HIGH_BIT_UNIFORM_SIZE,
        );
        let unorm8_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("host yuv420 8-bit sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let unorm8_bind_group_layout =
            create_unorm8_bind_group_layout(device, "host yuv420 8-bit bind group layout");
        let uint16_bind_group_layout =
            create_uint16_bind_group_layout(device, "host yuv420 16-bit bind group layout");
        let unorm8_pipeline = create_host_yuv420_pipeline(
            device,
            "host yuv420 8-bit pipeline layout",
            "host yuv420 8-bit render pipeline",
            &unorm8_bind_group_layout,
            &unorm8_shader,
            surface_format,
        );
        let uint16_pipeline = create_host_yuv420_pipeline(
            device,
            "host yuv420 16-bit pipeline layout",
            "host yuv420 16-bit render pipeline",
            &uint16_bind_group_layout,
            &uint16_shader,
            surface_format,
        );

        Self {
            unorm8_pipeline,
            unorm8_bind_group_layout,
            unorm8_uniform_buffers,
            unorm8_sampler,
            uint16_pipeline,
            uint16_bind_group_layout,
            uint16_uniform_buffers,
            color_settings: ColorPipelineSettings::default(),
            hdr_to_sdr_settings: HdrToSdrSettings::default(),
        }
    }

    /// Применяет live color pipeline settings без пересоздания pipeline-а.
    pub fn set_color_pipeline_settings(&mut self, settings: ColorPipelineSettings) {
        self.color_settings = settings;
    }

    /// Применяет live HDR-to-SDR settings без пересоздания pipeline-а.
    pub fn set_hdr_to_sdr_settings(&mut self, settings: HdrToSdrSettings) {
        self.hdr_to_sdr_settings = settings;
    }

    /// Рендерит HostPlanar YUV frame из отдельных Y/U/V plane views.
    pub fn render_frame(
        &mut self,
        frame: &RenderableFrame,
        y_view: &wgpu::TextureView,
        u_view: &wgpu::TextureView,
        v_view: &wgpu::TextureView,
        pass_context: &mut VideoRenderPassContext<'_>,
    ) -> Result<HostPlanarYuvRenderFrameDiagnostics> {
        match frame.format {
            VideoFramePixelLayout::Yuv420Planar8
            | VideoFramePixelLayout::Yuv422Planar8
            | VideoFramePixelLayout::Yuv444Planar8 => {
                self.render_unorm8_frame(frame, y_view, u_view, v_view, pass_context)
            }
            VideoFramePixelLayout::Yuv420Planar10Le
            | VideoFramePixelLayout::Yuv420Planar12Le
            | VideoFramePixelLayout::Yuv422Planar10Le
            | VideoFramePixelLayout::Yuv422Planar12Le
            | VideoFramePixelLayout::Yuv444Planar10Le => {
                self.render_uint16_frame(frame, y_view, u_view, v_view, pass_context)
            }
            unsupported_format => {
                bail!("HostPlanar YUV renderer received unsupported format: {unsupported_format}")
            }
        }
    }

    fn render_unorm8_frame(
        &mut self,
        frame: &RenderableFrame,
        y_view: &wgpu::TextureView,
        u_view: &wgpu::TextureView,
        v_view: &wgpu::TextureView,
        pass_context: &mut VideoRenderPassContext<'_>,
    ) -> Result<HostPlanarYuvRenderFrameDiagnostics> {
        validate_host_yuv420_8bit_frame(frame)?;

        let (uv_scale, uv_offset) = letterbox_scale_and_offset(frame, pass_context.viewport.size());
        let orientation_transform = display_orientation_uv_transform(frame.display_orientation);
        let prepared_color_pipeline = prepare_nv12_color_pipeline(
            frame,
            &self.color_settings,
            uv_scale,
            uv_offset,
            orientation_transform,
        );

        let uniform_buffer = &self.unorm8_uniform_buffers[pass_context.target_load.uniform_slot()];
        pass_context.queue.write_buffer(
            uniform_buffer,
            0,
            bytemuck::bytes_of(&prepared_color_pipeline.uniforms),
        );

        let bind_group = pass_context
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("host yuv420 8-bit bind group"),
                layout: &self.unorm8_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.unorm8_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(y_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(u_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(v_view),
                    },
                ],
            });

        draw_host_yuv420_pass(
            pass_context,
            "host yuv420 8-bit video pass",
            &self.unorm8_pipeline,
            &bind_group,
        );

        Ok(HostPlanarYuvRenderFrameDiagnostics {
            active_color_path: prepared_color_pipeline.active_path,
            hdr_reference_defaults: None,
        })
    }

    fn render_uint16_frame(
        &mut self,
        frame: &RenderableFrame,
        y_view: &wgpu::TextureView,
        u_view: &wgpu::TextureView,
        v_view: &wgpu::TextureView,
        pass_context: &mut VideoRenderPassContext<'_>,
    ) -> Result<HostPlanarYuvRenderFrameDiagnostics> {
        let prepared_render = prepare_host_yuv420_high_bit_render(
            frame,
            &self.color_settings,
            self.hdr_to_sdr_settings,
            pass_context.viewport.size(),
        )?;

        let uniform_buffer = &self.uint16_uniform_buffers[pass_context.target_load.uniform_slot()];
        pass_context.queue.write_buffer(
            uniform_buffer,
            0,
            bytemuck::bytes_of(&prepared_render.uniforms),
        );

        let bind_group = pass_context
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("host yuv420 16-bit bind group"),
                layout: &self.uint16_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(y_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(u_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(v_view),
                    },
                ],
            });

        draw_host_yuv420_pass(
            pass_context,
            "host yuv420 16-bit video pass",
            &self.uint16_pipeline,
            &bind_group,
        );

        Ok(HostPlanarYuvRenderFrameDiagnostics {
            active_color_path: prepared_render.active_path,
            hdr_reference_defaults: prepared_render.hdr_reference_defaults,
        })
    }
}

fn create_unorm8_bind_group_layout(
    device: &wgpu::Device,
    label: &'static str,
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[
            uniform_bind_group_layout_entry(0, COLOR_PIPELINE_UNIFORM_SIZE),
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            filterable_float_texture_entry(2),
            filterable_float_texture_entry(3),
            filterable_float_texture_entry(4),
        ],
    })
}

fn create_uint16_bind_group_layout(
    device: &wgpu::Device,
    label: &'static str,
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[
            uniform_bind_group_layout_entry(0, HOST_YUV420_HIGH_BIT_UNIFORM_SIZE),
            uint_texture_entry(1),
            uint_texture_entry(2),
            uint_texture_entry(3),
        ],
    })
}

fn uniform_bind_group_layout_entry(binding: u32, size: u64) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: Some(
                wgpu::BufferSize::new(size).expect("uniform buffer size must be non-zero"),
            ),
        },
        count: None,
    }
}

fn filterable_float_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn uint_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Uint,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn create_host_yuv420_pipeline(
    device: &wgpu::Device,
    pipeline_layout_label: &'static str,
    pipeline_label: &'static str,
    bind_group_layout: &wgpu::BindGroupLayout,
    shader: &wgpu::ShaderModule,
    surface_format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(pipeline_layout_label),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(pipeline_label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
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
    })
}

fn draw_host_yuv420_pass(
    pass_context: &mut VideoRenderPassContext<'_>,
    label: &'static str,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
) {
    let mut pass = pass_context
        .encoder
        .begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: pass_context.target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: pass_context.target_load.as_wgpu_load_op(),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.set_viewport(
        pass_context.viewport.x as f32,
        pass_context.viewport.y as f32,
        pass_context.viewport.width as f32,
        pass_context.viewport.height as f32,
        0.0,
        1.0,
    );
    for draw_rect in &pass_context.draw_rects {
        pass.set_scissor_rect(draw_rect.x, draw_rect.y, draw_rect.width, draw_rect.height);
        pass.draw(0..3, 0..1);
    }
}

fn prepare_host_yuv420_high_bit_render(
    frame: &RenderableFrame,
    color_settings: &ColorPipelineSettings,
    hdr_to_sdr_settings: HdrToSdrSettings,
    viewport_size: (u32, u32),
) -> Result<PreparedHostYuv420HighBitRender> {
    validate_host_yuv420_high_bit_frame(frame)?;

    let color_path = select_host_yuv420_color_path(frame)?;
    validate_hdr_to_sdr_settings_for_host_yuv420(hdr_to_sdr_settings, color_path)?;
    let (uv_scale, uv_offset) = letterbox_scale_and_offset(frame, viewport_size);
    let orientation_transform = display_orientation_uv_transform(frame.display_orientation);
    let active_path =
        active_color_path_for_host_yuv420(frame, color_settings, hdr_to_sdr_settings, color_path);
    let bit_depth = host_yuv420_shader_bit_depth(frame)?;
    let range_normalization = host_yuv420_range_normalization(bit_depth, frame.color.range)?;
    let optional_metadata = host_yuv420_optional_metadata_uniforms(
        frame.color.hdr_metadata.as_ref(),
        hdr_to_sdr_settings,
        color_path,
    );

    Ok(PreparedHostYuv420HighBitRender {
        uniforms: HostYuv420HighBitUniforms {
            uv_scale,
            uv_offset,
            orientation_transform_row0: orientation_transform[0],
            orientation_transform_row1: orientation_transform[1],
            shader_mode: [
                color_path.shader_mode(),
                host_yuv420_transfer_shader_mode(frame.color.transfer),
                bit_depth,
                0,
            ],
            luma_range: range_normalization.luma_range,
            chroma_range: range_normalization.chroma_range,
            hdr_reference_nits: optional_metadata.hdr_reference_nits,
            content_light_levels: optional_metadata.content_light_levels,
            optional_metadata_markers: optional_metadata.optional_metadata_markers,
        },
        active_path,
        hdr_reference_defaults: optional_metadata.diagnostic_markers,
    })
}

fn validate_host_yuv420_8bit_frame(frame: &RenderableFrame) -> Result<()> {
    ensure!(
        matches!(
            frame.format,
            VideoFramePixelLayout::Yuv420Planar8
                | VideoFramePixelLayout::Yuv422Planar8
                | VideoFramePixelLayout::Yuv444Planar8
        ),
        "HostPlanar YUV 8-bit renderer received {}",
        frame.format
    );
    ensure!(
        frame.bit_depth == BitDepth::Eight,
        "HostPlanar YUV 8-bit renderer received {:?}",
        frame.bit_depth
    );
    validate_host_planar_yuv_chroma(frame)?;
    Ok(())
}

fn validate_host_yuv420_high_bit_frame(frame: &RenderableFrame) -> Result<()> {
    ensure!(
        matches!(
            frame.format,
            VideoFramePixelLayout::Yuv420Planar10Le
                | VideoFramePixelLayout::Yuv420Planar12Le
                | VideoFramePixelLayout::Yuv422Planar10Le
                | VideoFramePixelLayout::Yuv422Planar12Le
                | VideoFramePixelLayout::Yuv444Planar10Le
        ),
        "HostPlanar YUV high-bit renderer received {}",
        frame.format
    );
    let expected_bit_depth = host_planar_yuv_expected_bit_depth(frame.format)
        .with_context(|| format!("HostPlanar YUV renderer received {}", frame.format))?;
    ensure!(
        frame.bit_depth == expected_bit_depth,
        "HostPlanar YUV renderer received {:?} bit depth for {}",
        frame.bit_depth,
        frame.format
    );
    validate_host_planar_yuv_chroma(frame)?;
    Ok(())
}

fn host_planar_yuv_expected_bit_depth(format: VideoFramePixelLayout) -> Option<BitDepth> {
    match format {
        VideoFramePixelLayout::Yuv420Planar8
        | VideoFramePixelLayout::Yuv422Planar8
        | VideoFramePixelLayout::Yuv444Planar8 => Some(BitDepth::Eight),
        VideoFramePixelLayout::Yuv420Planar10Le
        | VideoFramePixelLayout::Yuv422Planar10Le
        | VideoFramePixelLayout::Yuv444Planar10Le => Some(BitDepth::Ten),
        VideoFramePixelLayout::Yuv420Planar12Le | VideoFramePixelLayout::Yuv422Planar12Le => {
            Some(BitDepth::Twelve)
        }
        _ => None,
    }
}

fn validate_host_planar_yuv_chroma(frame: &RenderableFrame) -> Result<()> {
    let expected_chroma = host_planar_yuv_expected_chroma(frame.format)
        .with_context(|| format!("HostPlanar YUV renderer received {}", frame.format))?;

    ensure!(
        frame.chroma == expected_chroma,
        "HostPlanar YUV renderer received {:?} chroma for {}",
        frame.chroma,
        frame.format
    );

    Ok(())
}

fn host_planar_yuv_expected_chroma(
    format: VideoFramePixelLayout,
) -> Option<codec_core::ChromaSubsampling> {
    match format {
        VideoFramePixelLayout::Yuv420Planar8
        | VideoFramePixelLayout::Yuv420Planar10Le
        | VideoFramePixelLayout::Yuv420Planar12Le => Some(codec_core::ChromaSubsampling::Yuv420),
        VideoFramePixelLayout::Yuv422Planar8
        | VideoFramePixelLayout::Yuv422Planar10Le
        | VideoFramePixelLayout::Yuv422Planar12Le => Some(codec_core::ChromaSubsampling::Yuv422),
        VideoFramePixelLayout::Yuv444Planar8 | VideoFramePixelLayout::Yuv444Planar10Le => {
            Some(codec_core::ChromaSubsampling::Yuv444)
        }
        _ => None,
    }
}

fn select_host_yuv420_color_path(frame: &RenderableFrame) -> Result<HostYuv420ColorPath> {
    validate_host_yuv420_high_bit_frame(frame)?;

    if is_sdr_bt709_host_yuv420(&frame.color) {
        validate_host_yuv420_sdr_bt709_metadata(&frame.color)?;
        return Ok(HostYuv420ColorPath::SdrBt709);
    }

    if frame.color.requires_hdr_processing() {
        validate_host_yuv420_hdr_core_metadata(&frame.color)?;
        return Ok(HostYuv420ColorPath::HdrBt2446C);
    }

    bail!(
        "unsupported HostPlanar YUV420 color metadata: primaries={:?}, matrix={:?}, transfer={:?}, hdr_metadata={}",
        frame.color.primaries,
        frame.color.matrix,
        frame.color.transfer,
        frame.color.hdr_metadata.is_some()
    );
}

fn is_sdr_bt709_host_yuv420(color: &codec_core::VideoColorMetadata) -> bool {
    !color.requires_hdr_processing()
        && color.primaries == ColorPrimaries::Bt709
        && color.matrix == MatrixCoefficients::Bt709
        && color.transfer == TransferFunction::Bt709
}

fn validate_host_yuv420_sdr_bt709_metadata(color: &codec_core::VideoColorMetadata) -> Result<()> {
    ensure!(
        matches!(color.range, ColorRange::Limited | ColorRange::Full),
        "HostPlanar YUV420 SDR BT.709 requires explicit limited/full color range"
    );
    ensure!(
        !color.requires_hdr_processing(),
        "HostPlanar YUV420 SDR BT.709 path must not carry HDR transfer metadata"
    );

    Ok(())
}

fn validate_host_yuv420_hdr_core_metadata(color: &codec_core::VideoColorMetadata) -> Result<()> {
    ensure!(
        matches!(color.transfer, TransferFunction::Pq | TransferFunction::Hlg),
        "HostPlanar YUV420 HDR requires PQ or HLG transfer, got {:?}",
        color.transfer
    );
    ensure!(
        color.primaries == ColorPrimaries::Bt2020,
        "HostPlanar YUV420 HDR requires BT.2020 primaries, got {:?}",
        color.primaries
    );
    ensure!(
        color.matrix == MatrixCoefficients::Bt2020,
        "HostPlanar YUV420 HDR requires BT.2020 matrix, got {:?}",
        color.matrix
    );
    ensure!(
        matches!(color.range, ColorRange::Limited | ColorRange::Full),
        "HostPlanar YUV420 HDR requires explicit limited/full color range"
    );

    if let Some(hdr_metadata) = &color.hdr_metadata {
        ensure!(
            hdr_metadata.color_primaries == color.primaries,
            "HostPlanar YUV420 HDR mastering primaries {:?} do not match core primaries {:?}",
            hdr_metadata.color_primaries,
            color.primaries
        );
        ensure!(
            hdr_metadata.transfer_function == color.transfer,
            "HostPlanar YUV420 HDR mastering transfer {:?} does not match core transfer {:?}",
            hdr_metadata.transfer_function,
            color.transfer
        );
    }

    Ok(())
}

fn validate_hdr_to_sdr_settings_for_host_yuv420(
    settings: HdrToSdrSettings,
    color_path: HostYuv420ColorPath,
) -> Result<()> {
    if color_path == HostYuv420ColorPath::SdrBt709 {
        return Ok(());
    }

    ensure!(
        settings.is_phase10_bt2446_c_sdr_bt709(),
        "HostPlanar YUV420 HDR path requires Phase 10 BT.2446-C SDR BT.709 settings"
    );

    Ok(())
}

fn active_color_path_for_host_yuv420(
    frame: &RenderableFrame,
    color_settings: &ColorPipelineSettings,
    hdr_to_sdr_settings: HdrToSdrSettings,
    color_path: HostYuv420ColorPath,
) -> ActiveColorPath {
    match color_path {
        HostYuv420ColorPath::SdrBt709 => ActiveColorPath::from_frame(frame, color_settings),
        HostYuv420ColorPath::HdrBt2446C => {
            let hdr_color_settings = ColorPipelineSettings {
                swapchain_transfer: SwapchainTransferMode::ExplicitShaderOetf,
                ..*color_settings
            };

            ActiveColorPath::from_frame_with_hdr_to_sdr(
                frame,
                &hdr_color_settings,
                Some(hdr_to_sdr_settings),
            )
        }
    }
}

fn host_yuv420_range_normalization(
    bit_depth: u32,
    range: ColorRange,
) -> Result<HostYuv420RangeNormalization> {
    ensure!(
        matches!(bit_depth, 8 | 10 | 12),
        "HostPlanar YUV normalization supports 8/10/12-bit, got {bit_depth}"
    );

    let max_code = ((1u32 << bit_depth) - 1) as f32;
    let shift = bit_depth - 8;
    let limited_luma_black = (16u32 << shift) as f32;
    let limited_luma_white = (235u32 << shift) as f32;
    let limited_chroma_min = (16u32 << shift) as f32;
    let limited_chroma_max = (240u32 << shift) as f32;
    let chroma_center = (128u32 << shift) as f32;

    match range {
        ColorRange::Limited => Ok(HostYuv420RangeNormalization {
            luma_range: [
                limited_luma_black,
                1.0 / (limited_luma_white - limited_luma_black),
                limited_luma_black,
                limited_luma_white,
            ],
            chroma_range: [
                chroma_center,
                1.0 / (limited_chroma_max - limited_chroma_min),
                chroma_center,
                1.0 / (limited_chroma_max - limited_chroma_min),
            ],
        }),
        ColorRange::Full => Ok(HostYuv420RangeNormalization {
            luma_range: [0.0, 1.0 / max_code, 0.0, max_code],
            chroma_range: [chroma_center, 1.0 / max_code, chroma_center, 1.0 / max_code],
        }),
        ColorRange::Unknown => {
            bail!("HostPlanar YUV renderer requires explicit limited/full color range")
        }
    }
}

fn host_yuv420_optional_metadata_uniforms(
    hdr_metadata: Option<&codec_core::HdrMetadata>,
    hdr_to_sdr_settings: HdrToSdrSettings,
    color_path: HostYuv420ColorPath,
) -> HostYuv420OptionalMetadataUniforms {
    if color_path == HostYuv420ColorPath::SdrBt709 {
        return HostYuv420OptionalMetadataUniforms {
            hdr_reference_nits: [
                hdr_to_sdr_settings.sdr_reference_white_nits,
                hdr_to_sdr_settings.hdr_reference_peak_nits,
                0.0,
                0.0,
            ],
            content_light_levels: [0.0, 0.0, 0.0, 0.0],
            optional_metadata_markers: [HDR_METADATA_MARKER_NOT_APPLICABLE; 4],
            diagnostic_markers: None,
        };
    }

    let reference_peak_nits = hdr_to_sdr_settings.hdr_reference_peak_nits;
    let (mastering_max_luminance, mastering_max_marker) = optional_f32_or_reference_default(
        hdr_metadata.and_then(|metadata| metadata.max_luminance_nits),
        reference_peak_nits,
    );
    let (mastering_min_luminance, mastering_min_marker) = optional_f32_or_reference_default(
        hdr_metadata.and_then(|metadata| metadata.min_luminance_nits),
        0.0,
    );
    let (max_content_light_level, max_content_light_level_marker) =
        optional_u32_or_reference_default(
            hdr_metadata.and_then(|metadata| metadata.max_content_light_level_nits),
            reference_peak_nits,
        );
    let (max_frame_average_light_level, max_frame_average_light_level_marker) =
        optional_u32_or_reference_default(
            hdr_metadata.and_then(|metadata| metadata.max_frame_average_light_level_nits),
            reference_peak_nits,
        );

    HostYuv420OptionalMetadataUniforms {
        hdr_reference_nits: [
            hdr_to_sdr_settings.sdr_reference_white_nits,
            hdr_to_sdr_settings.hdr_reference_peak_nits,
            mastering_max_luminance,
            mastering_min_luminance,
        ],
        content_light_levels: [
            max_content_light_level,
            max_frame_average_light_level,
            0.0,
            0.0,
        ],
        optional_metadata_markers: [
            mastering_max_marker,
            mastering_min_marker,
            max_content_light_level_marker,
            max_frame_average_light_level_marker,
        ],
        diagnostic_markers: Some(HdrReferenceDefaultDiagnostics {
            mastering_max_luminance: marker_from_uniform_value(mastering_max_marker),
            mastering_min_luminance: marker_from_uniform_value(mastering_min_marker),
            max_content_light_level: marker_from_uniform_value(max_content_light_level_marker),
            max_frame_average_light_level: marker_from_uniform_value(
                max_frame_average_light_level_marker,
            ),
        }),
    }
}

fn optional_f32_or_reference_default(value: Option<f32>, reference_default: f32) -> (f32, u32) {
    value.map_or(
        (reference_default, HDR_METADATA_MARKER_REFERENCE_DEFAULT),
        |confirmed_value| (confirmed_value, HDR_METADATA_MARKER_CONFIRMED),
    )
}

fn optional_u32_or_reference_default(value: Option<u32>, reference_default: f32) -> (f32, u32) {
    value.map_or(
        (reference_default, HDR_METADATA_MARKER_REFERENCE_DEFAULT),
        |confirmed_value| (confirmed_value as f32, HDR_METADATA_MARKER_CONFIRMED),
    )
}

fn marker_from_uniform_value(value: u32) -> render_core::HdrMetadataDiagnosticMarker {
    match value {
        HDR_METADATA_MARKER_CONFIRMED => render_core::HdrMetadataDiagnosticMarker::Confirmed,
        HDR_METADATA_MARKER_REFERENCE_DEFAULT => {
            render_core::HdrMetadataDiagnosticMarker::ReferenceDefault
        }
        _ => render_core::HdrMetadataDiagnosticMarker::NotApplicable,
    }
}

const fn host_yuv420_transfer_shader_mode(transfer: TransferFunction) -> u32 {
    match transfer {
        TransferFunction::Pq => HOST_YUV420_TRANSFER_MODE_PQ,
        TransferFunction::Hlg => HOST_YUV420_TRANSFER_MODE_HLG,
        _ => HOST_YUV420_TRANSFER_MODE_SDR_BT709,
    }
}

fn host_yuv420_shader_bit_depth(frame: &RenderableFrame) -> Result<u32> {
    match frame.bit_depth {
        BitDepth::Eight => Ok(8),
        BitDepth::Ten => Ok(10),
        BitDepth::Twelve => Ok(12),
    }
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};

    use bytemuck::Zeroable;
    use codec_core::{
        ChromaSubsampling, ColorPrimaries, ColorRange, HdrMetadata, MatrixCoefficients,
        TransferFunction, VideoColorMetadata, VideoDisplayOrientation,
    };
    use render_core::RenderableFrame;

    use super::*;

    #[test]
    fn host_yuv420_high_bit_uniforms_match_wgsl_uniform_layout() {
        let uniforms = HostYuv420HighBitUniforms::zeroed();

        assert_eq!(align_of::<HostYuv420HighBitUniforms>(), 16);
        assert_eq!(size_of::<HostYuv420HighBitUniforms>(), 144);
        assert_eq!(size_of::<HostYuv420HighBitUniforms>() % 16, 0);
        assert_eq!(offset_of!(HostYuv420HighBitUniforms, uv_scale), 0);
        assert_eq!(offset_of!(HostYuv420HighBitUniforms, uv_offset), 8);
        assert_eq!(
            offset_of!(HostYuv420HighBitUniforms, orientation_transform_row0),
            16
        );
        assert_eq!(
            offset_of!(HostYuv420HighBitUniforms, orientation_transform_row1),
            32
        );
        assert_eq!(offset_of!(HostYuv420HighBitUniforms, shader_mode), 48);
        assert_eq!(offset_of!(HostYuv420HighBitUniforms, luma_range), 64);
        assert_eq!(offset_of!(HostYuv420HighBitUniforms, chroma_range), 80);
        assert_eq!(
            offset_of!(HostYuv420HighBitUniforms, hdr_reference_nits),
            96
        );
        assert_eq!(
            offset_of!(HostYuv420HighBitUniforms, content_light_levels),
            112
        );
        assert_eq!(
            offset_of!(HostYuv420HighBitUniforms, optional_metadata_markers),
            128
        );
        assert_eq!(
            bytemuck::bytes_of(&uniforms).len() as u64,
            HOST_YUV420_HIGH_BIT_UNIFORM_SIZE
        );
    }

    #[test]
    fn host_yuv420_shader_sources_parse_as_wgsl() {
        naga::front::wgsl::parse_str(HOST_YUV420_8BIT_SHADER_SOURCE)
            .expect("8-bit host YUV420 shader parses");
        naga::front::wgsl::parse_str(HOST_YUV420_16BIT_SHADER_SOURCE)
            .expect("16-bit host YUV420 shader parses");
    }

    #[test]
    fn host_yuv420_shaders_use_expected_texture_contracts() {
        assert!(HOST_YUV420_8BIT_SHADER_SOURCE.contains("var y_texture: texture_2d<f32>;"));
        assert!(
            HOST_YUV420_8BIT_SHADER_SOURCE
                .contains("source_uv_to_chroma_uv(textureDimensions(u_texture)")
        );
        assert!(
            HOST_YUV420_8BIT_SHADER_SOURCE
                .contains("source_uv_to_chroma_uv(textureDimensions(v_texture)")
        );
        assert!(HOST_YUV420_8BIT_SHADER_SOURCE.contains("source_uv_to_chroma_uv"));
        assert!(HOST_YUV420_8BIT_SHADER_SOURCE.contains("axis_subsampling_factor"));
        assert!(HOST_YUV420_8BIT_SHADER_SOURCE.contains("chroma_axis_size == luma_axis_size"));
        assert!(HOST_YUV420_16BIT_SHADER_SOURCE.contains("var y_texture: texture_2d<u32>;"));
        assert!(HOST_YUV420_16BIT_SHADER_SOURCE.contains("textureLoad(y_texture"));
        assert!(HOST_YUV420_16BIT_SHADER_SOURCE.contains("source_uv_to_chroma_texel"));
        assert!(HOST_YUV420_16BIT_SHADER_SOURCE.contains("luma_texel / subsampling_factor"));
        assert!(HOST_YUV420_16BIT_SHADER_SOURCE.contains("uniforms.shader_mode.z"));
    }

    #[test]
    fn known_yuv420_bt709_limited_samples_render_expected_rgb() {
        let range =
            host_yuv420_range_normalization(10, ColorRange::Limited).expect("10-bit limited range");

        assert_rgb_close(
            apply_bt709_uniforms_to_code_sample(&range, 64.0, 512.0, 512.0),
            [0.0, 0.0, 0.0],
        );
        assert_rgb_close(
            apply_bt709_uniforms_to_code_sample(&range, 940.0, 512.0, 512.0),
            [1.0, 1.0, 1.0],
        );
    }

    #[test]
    fn sdr_bt709_limited_and_full_metadata_use_different_code_ranges() {
        let limited =
            host_yuv420_range_normalization(8, ColorRange::Limited).expect("8-bit limited range");
        let full = host_yuv420_range_normalization(8, ColorRange::Full).expect("8-bit full range");

        let limited_gray = apply_bt709_uniforms_to_code_sample(&limited, 128.0, 128.0, 128.0);
        let full_gray = apply_bt709_uniforms_to_code_sample(&full, 128.0, 128.0, 128.0);

        assert!(limited_gray[0] > full_gray[0]);
        assert_rgb_close(full_gray, [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0]);
    }

    #[test]
    fn host_yuv420_10_and_12_bit_normalization_uses_source_bit_depth() {
        let eight_bit =
            host_yuv420_range_normalization(8, ColorRange::Limited).expect("8-bit normalization");
        let ten_bit =
            host_yuv420_range_normalization(10, ColorRange::Limited).expect("10-bit normalization");
        let twelve_bit =
            host_yuv420_range_normalization(12, ColorRange::Limited).expect("12-bit normalization");

        assert_eq!(eight_bit.luma_range[0], 16.0);
        assert_eq!(eight_bit.luma_range[3], 235.0);
        assert_eq!(eight_bit.chroma_range[0], 128.0);
        assert_eq!(ten_bit.luma_range[0], 64.0);
        assert_eq!(ten_bit.luma_range[3], 940.0);
        assert_eq!(ten_bit.chroma_range[0], 512.0);
        assert_eq!(twelve_bit.luma_range[0], 256.0);
        assert_eq!(twelve_bit.luma_range[3], 3760.0);
        assert_eq!(twelve_bit.chroma_range[0], 2048.0);
        assert_rgb_close(
            apply_bt709_uniforms_to_code_sample(&twelve_bit, 3760.0, 2048.0, 2048.0),
            [1.0, 1.0, 1.0],
        );
    }

    #[test]
    fn host_yuv420_high_bit_prepare_sets_bit_depth_in_shader_mode() {
        let frame = host_yuv420_test_frame(VideoFramePixelLayout::Yuv420Planar12Le);
        let prepared = prepare_host_yuv420_high_bit_render(
            &frame,
            &ColorPipelineSettings::default(),
            HdrToSdrSettings::default(),
            (1920, 1080),
        )
        .expect("12-bit SDR frame prepares");

        assert_eq!(
            prepared.uniforms.shader_mode[0],
            HOST_YUV420_SHADER_MODE_SDR_BT709
        );
        assert_eq!(prepared.uniforms.shader_mode[2], 12);
        assert_eq!(prepared.uniforms.luma_range[0], 256.0);
        assert_eq!(prepared.uniforms.luma_range[3], 3760.0);
    }

    #[test]
    fn host_yuv420_hdr_host_upload_uses_gpu_tone_mapping_with_hdr_metadata() {
        for (transfer, transfer_mode) in [
            (TransferFunction::Pq, HOST_YUV420_TRANSFER_MODE_PQ),
            (TransferFunction::Hlg, HOST_YUV420_TRANSFER_MODE_HLG),
        ] {
            let mut frame = host_yuv420_test_frame(VideoFramePixelLayout::Yuv420Planar10Le);
            frame.color = host_yuv420_hdr_bt2020_color(transfer);

            let prepared = prepare_host_yuv420_high_bit_render(
                &frame,
                &ColorPipelineSettings::default(),
                HdrToSdrSettings::default(),
                (1920, 1080),
            )
            .expect("HDR host-upload frame should prepare through GPU tone mapping");

            assert_eq!(
                prepared.uniforms.shader_mode[0],
                HOST_YUV420_SHADER_MODE_HDR_BT2446C
            );
            assert_eq!(prepared.uniforms.shader_mode[1], transfer_mode);
            assert!(prepared.active_path.hdr_to_sdr.is_some());
            assert_eq!(prepared.uniforms.hdr_reference_nits[2], 1_000.0);
            assert_eq!(prepared.uniforms.hdr_reference_nits[3], 0.005);
            assert_eq!(prepared.uniforms.content_light_levels[0], 1_000.0);
            assert_eq!(prepared.uniforms.content_light_levels[1], 400.0);
            assert_eq!(
                prepared.uniforms.optional_metadata_markers,
                [HDR_METADATA_MARKER_CONFIRMED; 4]
            );
        }
    }

    #[test]
    fn host_planar_yuv_renderer_accepts_v1_software_matrix() {
        let supported_formats = [
            (
                VideoFramePixelLayout::Yuv420Planar8,
                BitDepth::Eight,
                ChromaSubsampling::Yuv420,
            ),
            (
                VideoFramePixelLayout::Yuv420Planar10Le,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
            ),
            (
                VideoFramePixelLayout::Yuv420Planar12Le,
                BitDepth::Twelve,
                ChromaSubsampling::Yuv420,
            ),
            (
                VideoFramePixelLayout::Yuv422Planar8,
                BitDepth::Eight,
                ChromaSubsampling::Yuv422,
            ),
            (
                VideoFramePixelLayout::Yuv422Planar10Le,
                BitDepth::Ten,
                ChromaSubsampling::Yuv422,
            ),
            (
                VideoFramePixelLayout::Yuv422Planar12Le,
                BitDepth::Twelve,
                ChromaSubsampling::Yuv422,
            ),
            (
                VideoFramePixelLayout::Yuv444Planar8,
                BitDepth::Eight,
                ChromaSubsampling::Yuv444,
            ),
            (
                VideoFramePixelLayout::Yuv444Planar10Le,
                BitDepth::Ten,
                ChromaSubsampling::Yuv444,
            ),
        ];

        for (format, expected_bit_depth, expected_chroma) in supported_formats {
            let frame = host_yuv420_test_frame(format);

            assert_eq!(frame.bit_depth, expected_bit_depth);
            assert_eq!(frame.chroma, expected_chroma);

            if expected_bit_depth == BitDepth::Eight {
                validate_host_yuv420_8bit_frame(&frame)
                    .expect("8-bit host-planar YUV frame is accepted");
            } else {
                let prepared = prepare_host_yuv420_high_bit_render(
                    &frame,
                    &ColorPipelineSettings::default(),
                    HdrToSdrSettings::default(),
                    (1920, 1080),
                )
                .expect("high-bit host-planar YUV frame is accepted");

                assert_eq!(
                    prepared.uniforms.shader_mode[2],
                    bit_depth_code_for_tests(expected_bit_depth)
                );
            }
        }
    }

    #[test]
    fn host_planar_yuv_renderer_rejects_metadata_chroma_mismatch() {
        let mut frame = host_yuv420_test_frame(VideoFramePixelLayout::Yuv422Planar10Le);
        frame.chroma = ChromaSubsampling::Yuv420;

        let error = validate_host_yuv420_high_bit_frame(&frame)
            .expect_err("YUV422 layout with YUV420 metadata must be rejected");

        assert!(
            error.to_string().contains("chroma"),
            "unexpected error: {error}"
        );
    }

    fn host_yuv420_test_frame(format: VideoFramePixelLayout) -> RenderableFrame {
        let bit_depth =
            host_planar_yuv_expected_bit_depth(format).expect("test format is host-planar YUV");
        let chroma = host_planar_yuv_expected_chroma(format).expect("test format has chroma");

        RenderableFrame {
            handle: 1,
            pts: std::time::Duration::ZERO,
            format,
            bit_depth,
            chroma,
            coded_width: 1920,
            coded_height: 1080,
            render_width: 1920,
            render_height: 1080,
            display_orientation: VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
        }
    }

    fn host_yuv420_hdr_bt2020_color(transfer: TransferFunction) -> VideoColorMetadata {
        let mut color = VideoColorMetadata::bitstream(
            ColorRange::Limited,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            transfer,
        );
        color.hdr_metadata = Some(HdrMetadata {
            color_primaries: ColorPrimaries::Bt2020,
            transfer_function: transfer,
            max_luminance_nits: Some(1_000.0),
            min_luminance_nits: Some(0.005),
            max_content_light_level_nits: Some(1_000),
            max_frame_average_light_level_nits: Some(400),
        });
        color
    }

    fn bit_depth_code_for_tests(bit_depth: BitDepth) -> u32 {
        match bit_depth {
            BitDepth::Eight => 8,
            BitDepth::Ten => 10,
            BitDepth::Twelve => 12,
        }
    }

    fn apply_bt709_uniforms_to_code_sample(
        range: &HostYuv420RangeNormalization,
        y_code: f32,
        u_code: f32,
        v_code: f32,
    ) -> [f32; 3] {
        let normalized_y = (y_code - range.luma_range[0]) * range.luma_range[1];
        let normalized_u = (u_code - range.chroma_range[0]) * range.chroma_range[1];
        let normalized_v = (v_code - range.chroma_range[2]) * range.chroma_range[3];

        [
            (normalized_y + 1.5748 * normalized_v).clamp(0.0, 1.0),
            (normalized_y - 0.187_324_27 * normalized_u - 0.468_124_27 * normalized_v)
                .clamp(0.0, 1.0),
            (normalized_y + 1.8556 * normalized_u).clamp(0.0, 1.0),
        ]
    }

    fn assert_rgb_close(actual: [f32; 3], expected: [f32; 3]) {
        for (actual_channel, expected_channel) in actual.into_iter().zip(expected) {
            assert!(
                (actual_channel - expected_channel).abs() < 0.0001,
                "actual={actual:?}, expected={expected:?}"
            );
        }
    }
}
