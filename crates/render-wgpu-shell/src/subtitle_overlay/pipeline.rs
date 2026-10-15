//! Создание GPU-объектов overlay-а субтитров: pipeline, layout, атлас, instance buffer.
//!
//! Здесь только «как устроены ресурсы»; решения «когда загружать» живут в `mod.rs`.

use super::instances::INSTANCE_STRIDE_BYTES;

/// Размер uniform-а: `screen_size` (2 x f32) + `canvas_origin` (2 x f32).
pub(super) const UNIFORM_SIZE_BYTES: u64 = 16;

/// Формат атласа: один 8-битный канал покрытия, как в `SubtitleBitmap::coverage`.
const ATLAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

/// Атрибуты инстанса; смещения должны совпадать с `BitmapInstance::append_bytes`.
const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 3] = [
    // canvas_rect
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x4,
        offset: 0,
        shader_location: 0,
    },
    // atlas_origin
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Uint32x2,
        offset: 16,
        shader_location: 1,
    },
    // color (прямой RGBA, нормализуется в 0..1)
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Unorm8x4,
        offset: 24,
        shader_location: 2,
    },
];

/// Pipeline и layout bind group-ы (создаются один раз под формат surface).
pub(super) struct OverlayPipeline {
    pub(super) pipeline: wgpu::RenderPipeline,
    pub(super) bind_group_layout: wgpu::BindGroupLayout,
}

impl OverlayPipeline {
    /// Строит pipeline с premultiplied-смешиванием под `target_format`.
    ///
    /// Шейдер отдаёт `vec4(rgb * a, a)`; blend `One / OneMinusSrcAlpha` для цвета и альфы —
    /// классический «over» для premultiplied. Video pass пишет в surface альфу 1.0, поэтому
    /// после нашего pass-а альфа остаётся ровно 1.0 (`a + 1 * (1 - a)`), а RGB уже в той
    /// форме, которую ждут и premultiplied, и postmultiplied режимы surface (при альфе 1
    /// они совпадают). Угловая маска окна идёт строго после и сама умножает на свой coverage.
    /// Смешивание делается в гамма-пространстве (surface не sRGB) — так же, как у
    /// плееров на libass; линейное смешивание сделало бы края текста заметно тоньше.
    pub(super) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("subtitle overlay shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("subtitle_overlay.wgsl").into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("subtitle overlay bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    // textureLoad не требует sampler-а и фильтрации.
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("subtitle overlay pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("subtitle overlay pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: INSTANCE_STRIDE_BYTES as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &INSTANCE_ATTRIBUTES,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

/// Атлас масок на GPU вместе с bind group-ой (она ссылается на view этой текстуры).
pub(super) struct AtlasGpu {
    pub(super) texture: wgpu::Texture,
    pub(super) bind_group: wgpu::BindGroup,
    /// Сторона квадратной текстуры.
    pub(super) side: u32,
}

impl AtlasGpu {
    /// Создаёт текстуру `side` x `side` и привязывает её вместе с uniform-ом.
    pub(super) fn new(
        device: &wgpu::Device,
        bind_group_layout: &wgpu::BindGroupLayout,
        uniform_buffer: &wgpu::Buffer,
        side: u32,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("subtitle overlay coverage atlas"),
            size: wgpu::Extent3d {
                width: side,
                height: side,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: ATLAS_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("subtitle overlay bind group"),
            layout: bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
            ],
        });
        Self {
            texture,
            bind_group,
            side,
        }
    }
}

/// Создаёт uniform-буфер overlay-а.
pub(super) fn create_uniform_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("subtitle overlay uniforms"),
        size: UNIFORM_SIZE_BYTES,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Создаёт instance buffer на `capacity_instances` инстансов.
pub(super) fn create_instance_buffer(
    device: &wgpu::Device,
    capacity_instances: u64,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("subtitle overlay instances"),
        size: capacity_instances * INSTANCE_STRIDE_BYTES as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
