use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail, ensure};
use codec_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction,
    VideoColorMetadata,
};
use render_core::{
    ActiveColorPath, ColorPipelineSettings, HdrMetadataDiagnosticMarker,
    HdrReferenceDefaultDiagnostics, HdrToSdrSettings, RenderableFrame, SwapchainTransferMode,
};
use video_frame_contract::VideoFramePixelLayout;

use super::VideoRenderPassContext;

/// WGSL source dedicated to P010 rendering.
pub(crate) const P010_SHADER_SOURCE: &str = include_str!("../../shaders/p010_bt2446c_to_sdr.wgsl");

/// Размер HDR/P010 uniform buffer-а, который должен совпадать с WGSL layout.
const HDR_COLOR_PIPELINE_UNIFORM_SIZE: u64 = std::mem::size_of::<HdrColorPipelineUniforms>() as u64;

/// Минимальный 10-bit code value.
const P010_10BIT_MIN_CODE_VALUE: f32 = 0.0;

/// Максимальный 10-bit code value.
const P010_10BIT_MAX_CODE_VALUE: f32 = 1023.0;

/// Limited-range luma black для 10-bit YUV.
const P010_LIMITED_LUMA_BLACK_CODE_VALUE: f32 = 64.0;

/// Limited-range luma white для 10-bit YUV.
const P010_LIMITED_LUMA_WHITE_CODE_VALUE: f32 = 940.0;

/// Limited/full chroma center для 10-bit YUV.
const P010_CHROMA_CENTER_CODE_VALUE: f32 = 512.0;

/// Limited-range chroma minimum для 10-bit YUV.
const P010_LIMITED_CHROMA_MIN_CODE_VALUE: f32 = 64.0;

/// Limited-range chroma maximum для 10-bit YUV.
const P010_LIMITED_CHROMA_MAX_CODE_VALUE: f32 = 960.0;

/// Marker для optional metadata, которая не применима к выбранному path.
const HDR_METADATA_MARKER_NOT_APPLICABLE: u32 = 0;

/// Marker для optional metadata, подтверждённой container/bitstream/backend-ом.
const HDR_METADATA_MARKER_CONFIRMED: u32 = 1;

/// Marker для documented reference default вместо stream metadata.
const HDR_METADATA_MARKER_REFERENCE_DEFAULT: u32 = 2;

/// Shader mode для 10-bit SDR BT.709 P010 path.
const P010_SHADER_MODE_SDR_BT709: u32 = 0;

/// Shader mode для HDR-to-SDR BT.2446-C P010 path.
const P010_SHADER_MODE_HDR_BT2446C: u32 = 1;

/// Transfer mode для P010 SDR BT.709 branch-а.
const P010_TRANSFER_MODE_SDR_BT709: u32 = 0;

/// Transfer mode для P010 HDR PQ branch-а.
const P010_TRANSFER_MODE_PQ: u32 = 1;

/// Transfer mode для P010 HDR HLG branch-а.
const P010_TRANSFER_MODE_HLG: u32 = 2;

/// Uniform buffer для P010/HDR color pipeline.
///
/// Layout держит 16-byte alignment WGSL uniform rules:
/// - две `vec2<f32>` в начале вместе занимают первые 16 байт;
/// - все остальные поля представлены как `vec4`, чтобы Rust/WGSL offsets не расходились.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct HdrColorPipelineUniforms {
    /// Масштаб UV для letterbox.
    uv_scale: [f32; 2],

    /// Смещение UV для letterbox.
    uv_offset: [f32; 2],

    /// Первая строка affine transform из display UV в source texture UV.
    orientation_transform_row0: [f32; 4],

    /// Вторая строка affine transform из display UV в source texture UV.
    orientation_transform_row1: [f32; 4],

    /// `x`: shader branch, `y`: transfer mode, `z/w`: reserved для стабильного layout.
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

/// Выбранный color branch внутри P010 renderer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum P010RenderColorPath {
    /// P010 10-bit SDR BT.709 path без HDR tone mapping.
    SdrBt709,

    /// P010 HDR path через BT.2446-C shader implementation.
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

/// Возвращает typed transfer selector для WGSL shader-а.
fn p010_transfer_shader_mode(transfer: TransferFunction) -> u32 {
    match transfer {
        TransferFunction::Bt709 | TransferFunction::Srgb | TransferFunction::Unknown => {
            P010_TRANSFER_MODE_SDR_BT709
        }
        TransferFunction::Pq => P010_TRANSFER_MODE_PQ,
        TransferFunction::Hlg => P010_TRANSFER_MODE_HLG,
    }
}

/// CPU-side данные, подготовленные для одного P010 draw call.
#[derive(Debug)]
struct PreparedP010Render {
    /// Uniforms для текущего кадра.
    uniforms: HdrColorPipelineUniforms,

    /// Диагностический color path для UI/telemetry.
    active_path: ActiveColorPath,

    /// Renderer-neutral markers optional HDR metadata.
    hdr_reference_defaults: Option<HdrReferenceDefaultDiagnostics>,
}

/// Диагностика одного P010 draw call без GPU handles.
pub(crate) struct P010RenderFrameDiagnostics {
    /// Диагностический color path для UI/telemetry.
    pub active_color_path: ActiveColorPath,

    /// Source markers optional HDR metadata для HDR-to-SDR path.
    pub hdr_reference_defaults: Option<HdrReferenceDefaultDiagnostics>,
}

/// Приватный renderer для P010 decoded frames.
pub(crate) struct P010VideoRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    /// Отдельный uniform buffer на каждую pass-роль (main/overlay), см.
    /// `VideoRenderTargetLoad::uniform_slot`.
    uniform_buffers: [wgpu::Buffer; super::VideoRenderTargetLoad::UNIFORM_SLOT_COUNT],
    sampler: wgpu::Sampler,
    color_settings: ColorPipelineSettings,
    hdr_to_sdr_settings: HdrToSdrSettings,
}

/// Возвращает non-zero binding size для P010 uniform buffer-а.
fn p010_uniform_binding_size() -> NonZeroU64 {
    // Инвариант защищён layout test-ом `hdr_color_pipeline_uniforms_match_wgsl_uniform_layout`.
    NonZeroU64::new(HDR_COLOR_PIPELINE_UNIFORM_SIZE)
        .expect("размер HDR color pipeline uniform buffer должен быть non-zero")
}

impl P010VideoRenderer {
    /// Создаёт pipeline и immutable GPU state для P010 shader path.
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("p010 bt2446c to sdr shader"),
            source: wgpu::ShaderSource::Wgsl(P010_SHADER_SOURCE.into()),
        });

        let uniform_buffers = super::create_pass_uniform_buffers(
            device,
            "p010 uniform buffer",
            HDR_COLOR_PIPELINE_UNIFORM_SIZE,
        );

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
            uniform_buffers,
            sampler,
            color_settings: ColorPipelineSettings::default(),
            hdr_to_sdr_settings: HdrToSdrSettings::default(),
        }
    }

    /// Обновляет color settings, которые P010 diagnostics наследуют от общего renderer config.
    pub fn set_color_pipeline_settings(&mut self, color_settings: ColorPipelineSettings) {
        self.color_settings = color_settings;
    }

    /// Обновляет HDR-to-SDR settings, полученные из валидированного config.
    pub fn set_hdr_to_sdr_settings(&mut self, hdr_to_sdr_settings: HdrToSdrSettings) {
        self.hdr_to_sdr_settings = hdr_to_sdr_settings;
    }

    /// Рендерит P010 frame через отдельный P010 shader module.
    pub fn render_frame(
        &mut self,
        frame: &RenderableFrame,
        y_view: &wgpu::TextureView,
        uv_view: &wgpu::TextureView,
        pass_context: &mut VideoRenderPassContext<'_>,
    ) -> Result<P010RenderFrameDiagnostics> {
        let prepared_p010_render = prepare_p010_render(
            frame,
            &self.color_settings,
            self.hdr_to_sdr_settings,
            pass_context.viewport.size(),
        )?;
        log_first_p010_render_dispatch(frame, &prepared_p010_render);

        let uniform_buffer = &self.uniform_buffers[pass_context.target_load.uniform_slot()];
        pass_context.queue.write_buffer(
            uniform_buffer,
            0,
            bytemuck::bytes_of(&prepared_p010_render.uniforms),
        );

        // Bind group создаётся на кадр, потому что plane views приходят из decoder pool.
        // Renderer получает только пару views и не знает, какой storage layout был импортирован.
        let bind_group = pass_context
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("p010 bind group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buffer.as_entire_binding(),
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

        let mut pass = pass_context
            .encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("p010 video pass"),
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

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
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

        Ok(P010RenderFrameDiagnostics {
            active_color_path: prepared_p010_render.active_path,
            hdr_reference_defaults: prepared_p010_render.hdr_reference_defaults,
        })
    }
}

/// Собирает CPU-side state для P010 render pass.
fn prepare_p010_render(
    frame: &RenderableFrame,
    color_settings: &ColorPipelineSettings,
    hdr_to_sdr_settings: HdrToSdrSettings,
    viewport_size: (u32, u32),
) -> Result<PreparedP010Render> {
    validate_p010_renderable_frame(frame)?;

    let color_path = select_p010_color_path(frame)?;
    validate_hdr_to_sdr_settings_for_p010(hdr_to_sdr_settings, color_path)?;
    let (uv_scale, uv_offset) = super::letterbox_scale_and_offset(frame, viewport_size);
    let orientation_transform = super::display_orientation_uv_transform(frame.display_orientation);
    let active_path =
        active_color_path_for_p010(frame, color_settings, hdr_to_sdr_settings, color_path);
    let range_normalization = p010_range_normalization(frame.color.range)?;
    let optional_metadata = hdr_optional_metadata_uniforms(
        frame.color.hdr_metadata.as_ref(),
        hdr_to_sdr_settings,
        color_path,
    );

    Ok(PreparedP010Render {
        uniforms: HdrColorPipelineUniforms {
            uv_scale,
            uv_offset,
            orientation_transform_row0: orientation_transform[0],
            orientation_transform_row1: orientation_transform[1],
            shader_mode: [
                color_path.shader_mode(),
                p010_transfer_shader_mode(frame.color.transfer),
                0,
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

/// Range normalization values, already arranged for WGSL uniform fields.
#[derive(Debug, Clone, Copy, PartialEq)]
struct P010RangeNormalization {
    /// Luma code offset, scale, and clamp bounds.
    luma_range: [f32; 4],

    /// Chroma center and scale for U/V channels.
    chroma_range: [f32; 4],
}

/// Optional HDR metadata values plus source markers for shader diagnostics.
#[derive(Debug, Clone, Copy, PartialEq)]
struct HdrOptionalMetadataUniforms {
    /// Reference white/peak and mastering display luminance values.
    hdr_reference_nits: [f32; 4],

    /// Content light level values.
    content_light_levels: [f32; 4],

    /// Per-field markers: confirmed stream metadata or reference defaults.
    optional_metadata_markers: [u32; 4],

    /// Renderer-neutral source markers для UI diagnostics.
    diagnostic_markers: Option<HdrReferenceDefaultDiagnostics>,
}

/// Проверяет HDR settings именно в той ветке, где renderer обязан tone-map-ить HDR.
fn validate_hdr_to_sdr_settings_for_p010(
    hdr_to_sdr_settings: HdrToSdrSettings,
    color_path: P010RenderColorPath,
) -> Result<()> {
    if color_path == P010RenderColorPath::SdrBt709 {
        return Ok(());
    }

    ensure!(
        hdr_to_sdr_settings.is_phase10_bt2446_c_sdr_bt709(),
        "P010 HDR renderer requires enabled BT.2446-C SDR BT.709 settings"
    );

    Ok(())
}

/// Возвращает P010 normalization в 10-bit code values, не в 8-bit и не в normalized 0..1.
fn p010_range_normalization(range: ColorRange) -> Result<P010RangeNormalization> {
    match range {
        ColorRange::Limited => Ok(P010RangeNormalization {
            luma_range: [
                P010_LIMITED_LUMA_BLACK_CODE_VALUE,
                1.0 / (P010_LIMITED_LUMA_WHITE_CODE_VALUE - P010_LIMITED_LUMA_BLACK_CODE_VALUE),
                P010_LIMITED_LUMA_BLACK_CODE_VALUE,
                P010_LIMITED_LUMA_WHITE_CODE_VALUE,
            ],
            chroma_range: [
                P010_CHROMA_CENTER_CODE_VALUE,
                1.0 / (P010_LIMITED_CHROMA_MAX_CODE_VALUE - P010_LIMITED_CHROMA_MIN_CODE_VALUE),
                P010_CHROMA_CENTER_CODE_VALUE,
                1.0 / (P010_LIMITED_CHROMA_MAX_CODE_VALUE - P010_LIMITED_CHROMA_MIN_CODE_VALUE),
            ],
        }),
        ColorRange::Full => Ok(P010RangeNormalization {
            luma_range: [
                P010_10BIT_MIN_CODE_VALUE,
                1.0 / (P010_10BIT_MAX_CODE_VALUE - P010_10BIT_MIN_CODE_VALUE),
                P010_10BIT_MIN_CODE_VALUE,
                P010_10BIT_MAX_CODE_VALUE,
            ],
            chroma_range: [
                P010_CHROMA_CENTER_CODE_VALUE,
                1.0 / (P010_10BIT_MAX_CODE_VALUE - P010_10BIT_MIN_CODE_VALUE),
                P010_CHROMA_CENTER_CODE_VALUE,
                1.0 / (P010_10BIT_MAX_CODE_VALUE - P010_10BIT_MIN_CODE_VALUE),
            ],
        }),
        ColorRange::Unknown => bail!("P010 renderer requires explicit limited/full color range"),
    }
}

/// Собирает optional HDR metadata, не превращая reference defaults в confirmed metadata.
fn hdr_optional_metadata_uniforms(
    hdr_metadata: Option<&codec_core::HdrMetadata>,
    hdr_to_sdr_settings: HdrToSdrSettings,
    color_path: P010RenderColorPath,
) -> HdrOptionalMetadataUniforms {
    if color_path == P010RenderColorPath::SdrBt709 {
        return HdrOptionalMetadataUniforms {
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

    HdrOptionalMetadataUniforms {
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

/// Переводит shader-facing marker в renderer-neutral diagnostics enum.
fn marker_from_uniform_value(marker: u32) -> HdrMetadataDiagnosticMarker {
    match marker {
        HDR_METADATA_MARKER_CONFIRMED => HdrMetadataDiagnosticMarker::Confirmed,
        HDR_METADATA_MARKER_REFERENCE_DEFAULT => HdrMetadataDiagnosticMarker::ReferenceDefault,
        _ => HdrMetadataDiagnosticMarker::NotApplicable,
    }
}

/// Возвращает confirmed f32 metadata или reference default с явным marker-ом.
fn optional_f32_or_reference_default(value: Option<f32>, reference_default: f32) -> (f32, u32) {
    match value {
        Some(value) if value.is_finite() => (value, HDR_METADATA_MARKER_CONFIRMED),
        _ => (reference_default, HDR_METADATA_MARKER_REFERENCE_DEFAULT),
    }
}

/// Возвращает confirmed u32 metadata или reference default с явным marker-ом.
fn optional_u32_or_reference_default(value: Option<u32>, reference_default: f32) -> (f32, u32) {
    match value {
        Some(value) => (value as f32, HDR_METADATA_MARKER_CONFIRMED),
        None => (reference_default, HDR_METADATA_MARKER_REFERENCE_DEFAULT),
    }
}

/// Логирует первый P010 renderer dispatch для ручной проверки Phase 10 Session 3.
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
        transfer_mode = prepared_render.uniforms.shader_mode[1],
        transfer = ?frame.color.transfer,
        primaries = ?frame.color.primaries,
        matrix = ?frame.color.matrix,
        range = ?frame.color.range,
        optional_metadata_markers = ?prepared_render.uniforms.optional_metadata_markers,
        hdr_reference_defaults = ?prepared_render.hdr_reference_defaults,
        "P010 renderer dispatch selected"
    );
}

/// Проверяет renderer-neutral P010 contract перед созданием bind group.
fn validate_p010_renderable_frame(frame: &RenderableFrame) -> Result<()> {
    ensure!(
        frame.format == VideoFramePixelLayout::P010,
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
        validate_p010_sdr_bt709_metadata(&frame.color)?;
        return Ok(P010RenderColorPath::SdrBt709);
    }

    if is_hdr_p010_candidate(&frame.color) {
        validate_phase10_hdr_core_metadata(&frame.color)?;
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
    !color.requires_hdr_processing()
        && color.primaries == ColorPrimaries::Bt709
        && color.matrix == MatrixCoefficients::Bt709
        && color.transfer == TransferFunction::Bt709
}

/// Проверяет, что SDR P010 имеет явный range и не нуждается в HDR processing.
fn validate_p010_sdr_bt709_metadata(color: &VideoColorMetadata) -> Result<()> {
    ensure!(
        matches!(color.range, ColorRange::Limited | ColorRange::Full),
        "P010 SDR BT.709 requires explicit limited/full color range"
    );
    ensure!(
        !color.requires_hdr_processing(),
        "P010 SDR BT.709 path must not carry HDR transfer metadata"
    );

    Ok(())
}

/// Определяет HDR candidate без принятия неизвестных core полей.
fn is_hdr_p010_candidate(color: &VideoColorMetadata) -> bool {
    color.requires_hdr_processing()
}

/// Проверяет strict Phase 10 HDR core metadata перед render pass.
fn validate_phase10_hdr_core_metadata(color: &VideoColorMetadata) -> Result<()> {
    ensure!(
        matches!(color.transfer, TransferFunction::Pq | TransferFunction::Hlg),
        "P010 HDR requires PQ or HLG transfer, got {:?}",
        color.transfer
    );
    ensure!(
        color.primaries == ColorPrimaries::Bt2020,
        "P010 HDR requires BT.2020 primaries, got {:?}",
        color.primaries
    );
    ensure!(
        color.matrix == MatrixCoefficients::Bt2020,
        "P010 HDR requires BT.2020 matrix, got {:?}",
        color.matrix
    );
    ensure!(
        matches!(color.range, ColorRange::Limited | ColorRange::Full),
        "P010 HDR requires explicit limited/full color range"
    );

    if let Some(hdr_metadata) = &color.hdr_metadata {
        ensure!(
            hdr_metadata.color_primaries == color.primaries,
            "P010 HDR mastering primaries {:?} do not match core primaries {:?}",
            hdr_metadata.color_primaries,
            color.primaries
        );
        ensure!(
            hdr_metadata.transfer_function == color.transfer,
            "P010 HDR mastering transfer {:?} does not match core transfer {:?}",
            hdr_metadata.transfer_function,
            color.transfer
        );
    }

    Ok(())
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
        P010RenderColorPath::HdrBt2446C => {
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

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};
    use std::time::Duration;

    use bytemuck::Zeroable;
    use codec_core::{
        ColorMetadataConfidence, ColorMetadataOrigin, ColorRange, HdrMetadata, MatrixCoefficients,
        VideoDisplayOrientation,
    };
    use render_core::HdrToneMappingOperator;

    use super::*;

    #[test]
    fn hdr_color_pipeline_uniforms_match_wgsl_uniform_layout() {
        let uniforms = HdrColorPipelineUniforms::zeroed();

        assert_eq!(align_of::<HdrColorPipelineUniforms>(), 16);
        assert_eq!(
            size_of::<HdrColorPipelineUniforms>(),
            HDR_COLOR_PIPELINE_UNIFORM_SIZE as usize
        );
        assert_eq!(size_of::<HdrColorPipelineUniforms>() % 16, 0);
        assert_eq!(offset_of!(HdrColorPipelineUniforms, uv_scale), 0);
        assert_eq!(offset_of!(HdrColorPipelineUniforms, uv_offset), 8);
        assert_eq!(
            offset_of!(HdrColorPipelineUniforms, orientation_transform_row0),
            16
        );
        assert_eq!(
            offset_of!(HdrColorPipelineUniforms, orientation_transform_row1),
            32
        );
        assert_eq!(offset_of!(HdrColorPipelineUniforms, shader_mode), 48);
        assert_eq!(offset_of!(HdrColorPipelineUniforms, luma_range), 64);
        assert_eq!(offset_of!(HdrColorPipelineUniforms, chroma_range), 80);
        assert_eq!(offset_of!(HdrColorPipelineUniforms, hdr_reference_nits), 96);
        assert_eq!(
            offset_of!(HdrColorPipelineUniforms, content_light_levels),
            112
        );
        assert_eq!(
            offset_of!(HdrColorPipelineUniforms, optional_metadata_markers),
            128
        );
        assert_eq!(
            bytemuck::bytes_of(&uniforms).len() as u64,
            HDR_COLOR_PIPELINE_UNIFORM_SIZE
        );
    }

    #[test]
    fn p010_sdr_bt709_uses_non_hdr_color_branch_without_hdr_optional_defaults() {
        let frame = p010_test_frame(p010_sdr_bt709_limited_color());

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

        let prepared_render = prepare_p010_render(
            &frame,
            &ColorPipelineSettings::default(),
            HdrToSdrSettings::default(),
            (1920, 1080),
        )
        .expect("P010 SDR BT.709 uniforms accepted");

        assert_eq!(
            prepared_render.uniforms.shader_mode,
            [
                P010_SHADER_MODE_SDR_BT709,
                P010_TRANSFER_MODE_SDR_BT709,
                0,
                0,
            ]
        );
        assert_eq!(
            prepared_render.uniforms.optional_metadata_markers,
            [HDR_METADATA_MARKER_NOT_APPLICABLE; 4]
        );
        assert_eq!(prepared_render.uniforms.content_light_levels, [0.0; 4]);
        assert_eq!(prepared_render.hdr_reference_defaults, None);
    }

    #[test]
    fn p010_sdr_bt709_with_non_hdr_side_metadata_stays_sdr() {
        let mut color = p010_sdr_bt709_limited_color();
        color.hdr_metadata = Some(HdrMetadata {
            color_primaries: ColorPrimaries::Bt709,
            transfer_function: TransferFunction::Bt709,
            max_luminance_nits: None,
            min_luminance_nits: None,
            max_content_light_level_nits: Some(300),
            max_frame_average_light_level_nits: Some(120),
        });
        let frame = p010_test_frame(color);

        let color_path = select_p010_color_path(&frame)
            .expect("P010 SDR BT.709 side metadata must not select HDR path");
        let prepared_render = prepare_p010_render(
            &frame,
            &ColorPipelineSettings::default(),
            HdrToSdrSettings::default(),
            (1920, 1080),
        )
        .expect("P010 SDR BT.709 with non-HDR side metadata accepted");

        assert_eq!(color_path, P010RenderColorPath::SdrBt709);
        assert_eq!(
            prepared_render.uniforms.shader_mode,
            [
                P010_SHADER_MODE_SDR_BT709,
                P010_TRANSFER_MODE_SDR_BT709,
                0,
                0,
            ]
        );
        assert_eq!(prepared_render.hdr_reference_defaults, None);
    }

    #[test]
    fn p010_hdr_pq_uses_hdr_color_branch() {
        let frame = p010_test_frame(p010_hdr_bt2020_color(TransferFunction::Pq));

        let color_path = select_p010_color_path(&frame).expect("P010 HDR accepted");
        let active_path = active_color_path_for_p010(
            &frame,
            &ColorPipelineSettings::default(),
            HdrToSdrSettings::default(),
            color_path,
        );

        assert_eq!(color_path, P010RenderColorPath::HdrBt2446C);
        assert_eq!(
            active_path.hdr_to_sdr.map(|settings| settings.operator),
            Some(HdrToneMappingOperator::Bt2446C)
        );
        assert!(active_path.diagnostic_text().contains("bt2446-c"));
        assert_eq!(
            active_path.diagnostic_text(),
            "P010 10-bit BT.2020 PQ limited -> SDR BT.709 bt2446-c explicit-shader-oetf"
        );

        let prepared_render = prepare_p010_render(
            &frame,
            &ColorPipelineSettings::default(),
            HdrToSdrSettings::default(),
            (1920, 1080),
        )
        .expect("P010 PQ HDR uniforms accepted");

        assert_eq!(
            prepared_render.uniforms.shader_mode,
            [P010_SHADER_MODE_HDR_BT2446C, P010_TRANSFER_MODE_PQ, 0, 0]
        );
    }

    #[test]
    fn p010_hdr_hlg_uses_hdr_color_branch() {
        let frame = p010_test_frame(p010_hdr_bt2020_color(TransferFunction::Hlg));

        let color_path = select_p010_color_path(&frame).expect("P010 HLG HDR accepted");

        assert_eq!(color_path, P010RenderColorPath::HdrBt2446C);

        let prepared_render = prepare_p010_render(
            &frame,
            &ColorPipelineSettings::default(),
            HdrToSdrSettings::default(),
            (1920, 1080),
        )
        .expect("P010 HLG HDR uniforms accepted");

        assert_eq!(
            prepared_render.uniforms.shader_mode,
            [P010_SHADER_MODE_HDR_BT2446C, P010_TRANSFER_MODE_HLG, 0, 0]
        );
    }

    #[test]
    fn missing_maxcll_maxfall_use_reference_default_markers() {
        let mut color = p010_hdr_bt2020_color(TransferFunction::Pq);
        color.hdr_metadata = Some(HdrMetadata {
            color_primaries: ColorPrimaries::Bt2020,
            transfer_function: TransferFunction::Pq,
            max_luminance_nits: Some(1_000.0),
            min_luminance_nits: Some(0.01),
            max_content_light_level_nits: None,
            max_frame_average_light_level_nits: None,
        });
        let frame = p010_test_frame(color);

        let prepared_render = prepare_p010_render(
            &frame,
            &ColorPipelineSettings::default(),
            HdrToSdrSettings::default(),
            (1920, 1080),
        )
        .expect("P010 HDR uniforms accepted with reference optional defaults");

        assert_eq!(
            prepared_render.uniforms.optional_metadata_markers,
            [
                HDR_METADATA_MARKER_CONFIRMED,
                HDR_METADATA_MARKER_CONFIRMED,
                HDR_METADATA_MARKER_REFERENCE_DEFAULT,
                HDR_METADATA_MARKER_REFERENCE_DEFAULT,
            ]
        );
        assert_eq!(
            prepared_render.uniforms.content_light_levels[0],
            HdrToSdrSettings::default().hdr_reference_peak_nits
        );
        assert_eq!(
            prepared_render.uniforms.content_light_levels[1],
            HdrToSdrSettings::default().hdr_reference_peak_nits
        );
        let hdr_reference_defaults = prepared_render
            .hdr_reference_defaults
            .expect("HDR reference markers exposed to diagnostics");
        assert!(hdr_reference_defaults.has_reference_defaults());
        assert!(
            hdr_reference_defaults
                .diagnostic_text()
                .contains("reference-default")
        );
        assert_eq!(
            frame
                .color
                .hdr_metadata
                .as_ref()
                .and_then(|metadata| metadata.max_content_light_level_nits),
            None
        );
    }

    #[test]
    fn disabled_hdr_to_sdr_settings_reject_hdr_p010_render() {
        let frame = p010_test_frame(p010_hdr_bt2020_color(TransferFunction::Pq));
        let disabled_settings = HdrToSdrSettings {
            enabled: false,
            ..HdrToSdrSettings::default()
        };

        let error = prepare_p010_render(
            &frame,
            &ColorPipelineSettings::default(),
            disabled_settings,
            (1920, 1080),
        )
        .expect_err("disabled HDR-to-SDR settings reject HDR P010");

        assert!(
            error.to_string().contains("BT.2446-C SDR BT.709"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn p010_limited_range_normalization_uses_10bit_code_values() {
        let normalization =
            p010_range_normalization(ColorRange::Limited).expect("limited range accepted");

        assert_eq!(
            normalization.luma_range,
            [
                P010_LIMITED_LUMA_BLACK_CODE_VALUE,
                1.0 / (P010_LIMITED_LUMA_WHITE_CODE_VALUE - P010_LIMITED_LUMA_BLACK_CODE_VALUE),
                P010_LIMITED_LUMA_BLACK_CODE_VALUE,
                P010_LIMITED_LUMA_WHITE_CODE_VALUE,
            ]
        );
        assert_eq!(
            normalization.chroma_range,
            [
                P010_CHROMA_CENTER_CODE_VALUE,
                1.0 / (P010_LIMITED_CHROMA_MAX_CODE_VALUE - P010_LIMITED_CHROMA_MIN_CODE_VALUE),
                P010_CHROMA_CENTER_CODE_VALUE,
                1.0 / (P010_LIMITED_CHROMA_MAX_CODE_VALUE - P010_LIMITED_CHROMA_MIN_CODE_VALUE),
            ]
        );
    }

    #[test]
    fn p010_hdr_missing_core_metadata_is_rejected_before_render() {
        let cases = [
            (
                hdr_color_with_core_fields(
                    ColorRange::Limited,
                    MatrixCoefficients::Bt2020,
                    ColorPrimaries::Bt2020,
                    TransferFunction::Unknown,
                ),
                "requires PQ or HLG transfer",
            ),
            (
                hdr_color_with_core_fields(
                    ColorRange::Limited,
                    MatrixCoefficients::Bt2020,
                    ColorPrimaries::Unknown,
                    TransferFunction::Pq,
                ),
                "requires BT.2020 primaries",
            ),
            (
                hdr_color_with_core_fields(
                    ColorRange::Limited,
                    MatrixCoefficients::Unknown,
                    ColorPrimaries::Bt2020,
                    TransferFunction::Pq,
                ),
                "requires BT.2020 matrix",
            ),
            (
                hdr_color_with_core_fields(
                    ColorRange::Unknown,
                    MatrixCoefficients::Bt2020,
                    ColorPrimaries::Bt2020,
                    TransferFunction::Pq,
                ),
                "requires explicit limited/full color range",
            ),
        ];

        for (color, expected_error_fragment) in cases {
            let frame = p010_test_frame(color);

            let error = prepare_p010_render(
                &frame,
                &ColorPipelineSettings::default(),
                HdrToSdrSettings::default(),
                (1920, 1080),
            )
            .expect_err("P010 HDR with missing core metadata must be rejected before render");

            assert!(
                error.to_string().contains(expected_error_fragment),
                "expected `{expected_error_fragment}` in error, got: {error}"
            );
        }
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
        let nv12_shader_source = include_str!("../../shaders/nv12_to_rgba.wgsl");

        assert!(!P010_SHADER_SOURCE.trim().is_empty());
        assert_ne!(P010_SHADER_SOURCE, nv12_shader_source);
        assert!(P010_SHADER_SOURCE.contains("p010"));
    }

    #[test]
    fn p010_shader_source_parses_and_validates_as_wgsl() {
        let shader_module =
            naga::front::wgsl::parse_str(P010_SHADER_SOURCE).expect("P010 WGSL source must parse");

        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&shader_module)
        .expect("P010 WGSL source must validate");
    }

    #[test]
    fn p010_shader_reads_y_from_plane0_and_uv_from_plane1() {
        assert!(
            P010_SHADER_SOURCE.contains("@group(0) @binding(2)\nvar p010_y_texture"),
            "P010 plane 0 must be the Y texture binding"
        );
        assert!(
            P010_SHADER_SOURCE.contains("@group(0) @binding(3)\nvar p010_uv_texture"),
            "P010 plane 1 must be the interleaved UV texture binding"
        );
        assert!(
            P010_SHADER_SOURCE.contains("textureSample(p010_y_texture, p010_sampler, source_uv).r"),
            "P010 Y must be read from the luma plane"
        );
        assert!(
            P010_SHADER_SOURCE
                .contains("textureSample(p010_uv_texture, p010_sampler, source_uv).rg"),
            "P010 UV must be read from the chroma plane"
        );
    }

    #[test]
    fn p010_shader_contains_pq_and_hlg_transfer_modes() {
        assert_eq!(
            shader_u32_const(P010_SHADER_SOURCE, "P010_TRANSFER_MODE_PQ"),
            P010_TRANSFER_MODE_PQ
        );
        assert_eq!(
            shader_u32_const(P010_SHADER_SOURCE, "P010_TRANSFER_MODE_HLG"),
            P010_TRANSFER_MODE_HLG
        );

        let eotf_body = wgsl_function_body(P010_SHADER_SOURCE, "apply_hdr_eotf");

        assert!(eotf_body.contains("uniforms.shader_mode.y == P010_TRANSFER_MODE_PQ"));
        assert!(eotf_body.contains("uniforms.shader_mode.y == P010_TRANSFER_MODE_HLG"));
        assert!(eotf_body.contains("pq_eotf_nits"));
        assert!(eotf_body.contains("hlg_eotf_rgb_nits"));
    }

    #[test]
    fn p010_sdr_shader_branch_does_not_call_bt2446c_functions() {
        let sdr_body = wgsl_function_body(P010_SHADER_SOURCE, "p010_sdr_bt709_to_rgb");
        let fragment_body = wgsl_function_body(P010_SHADER_SOURCE, "fs_main");
        let sdr_return = fragment_body
            .find("return vec4<f32>(p010_sdr_bt709_to_rgb(normalized_yuv), 1.0);")
            .expect("fragment shader must return from SDR branch directly");
        let hdr_call = fragment_body
            .find("p010_hdr_bt2446c_to_sdr(normalized_yuv)")
            .expect("fragment shader must call HDR BT.2446-C branch");

        assert!(!sdr_body.contains("bt2446c"));
        assert!(!sdr_body.contains("apply_hdr_eotf"));
        assert!(
            sdr_return < hdr_call,
            "SDR branch must return before any HDR BT.2446-C call"
        );
    }

    #[test]
    fn p010_shader_does_not_call_nv12_sdr_helpers() {
        for forbidden_helper in [
            "normalize_yuv_sample(",
            "convert_yuv_to_rgb(",
            "apply_sdr_adjustments(",
        ] {
            assert!(
                !P010_SHADER_SOURCE.contains(forbidden_helper),
                "P010 shader must not call or mention NV12 SDR helper `{forbidden_helper}`"
            );
        }
    }

    #[test]
    fn p010_shader_contains_bt2446c_method_c_stages() {
        for required_stage in [
            "bt2020_yuv_to_rgb",
            "apply_hdr_eotf",
            "apply_bt2446c_crosstalk",
            "bt2020_linear_rgb_to_xyz",
            "xyz_to_chromaticity",
            "bt2446c_tone_map_luminance_nits",
            "xyz_from_luminance_and_chromaticity",
            "xyz_to_bt709_linear_rgb",
            "apply_bt2446c_inverse_crosstalk",
            "encode_sdr_bt709_output",
        ] {
            assert!(
                P010_SHADER_SOURCE.contains(required_stage),
                "P010 shader is missing BT.2446-C stage `{required_stage}`"
            );
        }
    }

    fn p010_test_frame(color: VideoColorMetadata) -> RenderableFrame {
        RenderableFrame {
            handle: 42,
            pts: Duration::ZERO,
            format: VideoFramePixelLayout::P010,
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv420,
            coded_width: 1920,
            coded_height: 1080,
            render_width: 1920,
            render_height: 1080,
            display_orientation: VideoDisplayOrientation::Identity,
            color,
        }
    }

    fn p010_sdr_bt709_limited_color() -> VideoColorMetadata {
        VideoColorMetadata {
            range: ColorRange::Limited,
            matrix: MatrixCoefficients::Bt709,
            primaries: ColorPrimaries::Bt709,
            transfer: TransferFunction::Bt709,
            hdr_metadata: None,
            origin: ColorMetadataOrigin::Container,
            confidence: ColorMetadataConfidence::Confirmed,
        }
    }

    fn p010_hdr_bt2020_color(transfer: TransferFunction) -> VideoColorMetadata {
        let mut color = hdr_color_with_core_fields(
            ColorRange::Limited,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            transfer,
        );
        color.hdr_metadata = Some(HdrMetadata {
            color_primaries: ColorPrimaries::Bt2020,
            transfer_function: transfer,
            max_luminance_nits: Some(1_000.0),
            min_luminance_nits: Some(0.01),
            max_content_light_level_nits: Some(1_000),
            max_frame_average_light_level_nits: Some(400),
        });
        color
    }

    fn hdr_color_with_core_fields(
        range: ColorRange,
        matrix: MatrixCoefficients,
        primaries: ColorPrimaries,
        transfer: TransferFunction,
    ) -> VideoColorMetadata {
        VideoColorMetadata {
            range,
            matrix,
            primaries,
            transfer,
            hdr_metadata: Some(HdrMetadata {
                color_primaries: primaries,
                transfer_function: if transfer == TransferFunction::Unknown {
                    TransferFunction::Pq
                } else {
                    transfer
                },
                max_luminance_nits: None,
                min_luminance_nits: None,
                max_content_light_level_nits: None,
                max_frame_average_light_level_nits: None,
            }),
            origin: ColorMetadataOrigin::Bitstream,
            confidence: ColorMetadataConfidence::Confirmed,
        }
    }

    fn shader_u32_const(shader_source: &str, const_name: &str) -> u32 {
        let prefix = format!("const {const_name}: u32 = ");
        let line = shader_source
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with(&prefix))
            .unwrap_or_else(|| panic!("WGSL const `{const_name}` not found"));
        let value = line
            .trim_start_matches(&prefix)
            .trim_end_matches(';')
            .trim_end_matches('u');

        value
            .parse()
            .unwrap_or_else(|error| panic!("WGSL const `{const_name}` is not u32: {error}"))
    }

    fn wgsl_function_body<'source>(
        shader_source: &'source str,
        function_name: &str,
    ) -> &'source str {
        let signature = format!("fn {function_name}(");
        let signature_start = shader_source
            .find(&signature)
            .unwrap_or_else(|| panic!("WGSL function `{function_name}` not found"));
        let body_start = shader_source[signature_start..]
            .find('{')
            .map(|offset| signature_start + offset)
            .unwrap_or_else(|| panic!("WGSL function `{function_name}` has no body"));
        let mut brace_depth = 0usize;

        for (offset, character) in shader_source[body_start..].char_indices() {
            match character {
                '{' => brace_depth += 1,
                '}' => {
                    brace_depth -= 1;
                    if brace_depth == 0 {
                        return &shader_source[body_start..=body_start + offset];
                    }
                }
                _ => {}
            }
        }

        panic!("WGSL function `{function_name}` body is not closed")
    }
}
