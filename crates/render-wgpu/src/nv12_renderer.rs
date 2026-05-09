use anyhow::Result;
use std::num::NonZeroU64;

use render_core::{ActiveColorPath, ColorPipelineSettings, RenderableFrame};

use crate::color_pipeline::{COLOR_PIPELINE_UNIFORM_SIZE, prepare_nv12_color_pipeline};

/// Приватный renderer для NV12 decoded frames с YUV->RGB conversion.
pub(crate) struct Nv12VideoRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    color_settings: ColorPipelineSettings,
    window_size: (u32, u32),
}

/// Возвращает non-zero binding size для uniform buffer-а.
fn color_pipeline_uniform_binding_size() -> NonZeroU64 {
    // Инвариант защищён layout test-ом `color_pipeline_uniforms_match_wgsl_uniform_layout`.
    NonZeroU64::new(COLOR_PIPELINE_UNIFORM_SIZE)
        .expect("размер color pipeline uniform buffer должен быть non-zero")
}

impl Nv12VideoRenderer {
    /// Создаёт pipeline и immutable GPU state для NV12 shader path.
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nv12 to rgba shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/nv12_to_rgba.wgsl").into()),
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12 uniform buffer"),
            size: COLOR_PIPELINE_UNIFORM_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nv12 sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("nv12 bind group layout"),
            entries: &[
                // Uniform buffer с scale/offset для letterbox.
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: Some(color_pipeline_uniform_binding_size()),
                    },
                    count: None,
                },
                // Sampler для luma plane.
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Texture view для luma plane.
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
                // Sampler для chroma plane.
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Texture view для interleaved UV plane.
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
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
            label: Some("nv12 pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("nv12 render pipeline"),
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
            window_size: (1280, 720),
        }
    }

    /// Обновляет размер surface для расчёта letterbox.
    pub fn set_window_size(&mut self, width: u32, height: u32) {
        self.window_size = (width, height);
    }

    /// Обновляет SDR color settings без пересоздания GPU pipeline.
    pub fn set_color_pipeline_settings(&mut self, color_settings: ColorPipelineSettings) {
        self.color_settings = color_settings;
    }

    /// Рендерит NV12 frame с letterbox и YUV->RGB conversion.
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
        // Считаем отношение сторон видео и текущего окна.
        let video_aspect = frame.render_width as f32 / frame.render_height.max(1) as f32;
        let window_aspect = self.window_size.0 as f32 / self.window_size.1.max(1) as f32;

        let (scale_x, scale_y, offset_x, offset_y) = if video_aspect > window_aspect {
            // Видео шире окна: shader оставит чёрные полосы сверху и снизу.
            let scale = window_aspect / video_aspect;
            (1.0, scale, 0.0, (1.0 - scale) * 0.5)
        } else {
            // Видео выше окна: shader оставит чёрные полосы слева и справа.
            let scale = video_aspect / window_aspect;
            (scale, 1.0, (1.0 - scale) * 0.5, 0.0)
        };

        let prepared_color_pipeline = prepare_nv12_color_pipeline(
            frame,
            &self.color_settings,
            [scale_x, scale_y],
            [offset_x, offset_y],
        );

        queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&prepared_color_pipeline.uniforms),
        );

        // Bind group создаётся на кадр, потому что texture views приходят из decoder pool.
        // Позже это место можно заменить на pool/cache без изменения public facade.
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nv12 bind group"),
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
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(uv_view),
                },
            ],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("nv12 video pass"),
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

        Ok(prepared_color_pipeline.active_path)
    }
}

#[cfg(test)]
mod tests {
    /// Проверяем контракт NV12: первый байт chroma-пары — U, второй — V.
    #[test]
    fn nv12_shader_reads_uv_channels_in_nv12_order() {
        let shader_source = include_str!("../shaders/nv12_to_rgba.wgsl");

        assert!(
            shader_source.contains("let sampled_u = sampled_uv.r;"),
            "NV12 в Rg8Unorm должен читать U из .r"
        );
        assert!(
            shader_source.contains("let sampled_v = sampled_uv.g;"),
            "NV12 в Rg8Unorm должен читать V из .g"
        );
    }

    /// Проверяем, что shader contract больше не завязан на hardcoded BT.709 helper.
    #[test]
    fn nv12_shader_uses_uniform_color_pipeline_contract() {
        let shader_source = include_str!("../shaders/nv12_to_rgba.wgsl");

        assert!(
            !shader_source.contains("fn nv12_to_rgb"),
            "YUV->RGB conversion должен идти через generic uniform-driven helper"
        );
        assert!(
            shader_source.contains("yuv_to_rgb_row0"),
            "Shader должен читать matrix coefficients из uniforms"
        );
    }
}
