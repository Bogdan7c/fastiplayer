//! Финальный GPU-контур desktop-окна поверх video и egui композиции.

/// Намерение app-слоя о форме текущего кадра в логических UI points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowCornerMask {
    radius_points: f32,
}

impl WindowCornerMask {
    /// Создаёт полностью прямоугольный контур без дополнительного GPU-pass-а.
    #[must_use]
    pub const fn square() -> Self {
        Self { radius_points: 0.0 }
    }

    /// Создаёт скруглённый контур; отрицательные и нечисловые значения безопасно отключаются.
    #[must_use]
    pub fn rounded_in_points(radius_points: f32) -> Self {
        let radius_points = if radius_points.is_finite() {
            radius_points.max(0.0)
        } else {
            0.0
        };
        Self { radius_points }
    }

    /// Возвращает радиус в той же логической системе координат, что и egui.
    #[must_use]
    pub const fn radius_points(self) -> f32 {
        self.radius_points
    }
}

/// Фактическое кодирование RGB относительно alpha, подтверждённое surface capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SurfaceAlphaEncoding {
    /// RGB уже умножен на alpha до передачи desktop compositor-у.
    Premultiplied,
    /// RGB остаётся прямым, а desktop compositor умножает его на alpha.
    Postmultiplied,
}

/// Renderer лёгкого fullscreen-pass-а, который умножает готовый кадр на coverage контура.
pub(crate) struct WindowCornerMaskRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
}

impl WindowCornerMaskRenderer {
    /// Создаёт pipeline под фактический формат swapchain и его alpha-семантику.
    pub(crate) fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        alpha_encoding: SurfaceAlphaEncoding,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("window corner mask shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("window_corner_mask.wgsl").into()),
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("window corner mask uniforms"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("window corner mask bind group layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("window corner mask bind group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("window corner mask pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let destination_rgb_factor = match alpha_encoding {
            SurfaceAlphaEncoding::Premultiplied => wgpu::BlendFactor::SrcAlpha,
            SurfaceAlphaEncoding::Postmultiplied => wgpu::BlendFactor::One,
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("window corner mask pipeline"),
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
                    format: target_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::Zero,
                            dst_factor: destination_rgb_factor,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::Zero,
                            dst_factor: wgpu::BlendFactor::SrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            bind_group,
            uniform_buffer,
        }
    }

    /// Записывает финальную маску; `false` означает осознанно пропущенный square-pass.
    pub(crate) fn render(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        screen_size: [u32; 2],
        pixels_per_point: f32,
        mask: WindowCornerMask,
    ) -> bool {
        let surface_width = screen_size[0] as f32;
        let surface_height = screen_size[1] as f32;
        let physical_radius = (mask.radius_points() * pixels_per_point)
            .clamp(0.0, surface_width.min(surface_height) * 0.5);
        if physical_radius <= 0.0 || surface_width <= 0.0 || surface_height <= 0.0 {
            return false;
        }
        let uniform_values = [surface_width, surface_height, physical_radius, 1.0_f32];
        let uniform_bytes = uniform_values
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect::<Vec<_>>();
        queue.write_buffer(&self.uniform_buffer, 0, &uniform_bytes);
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("window corner mask pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        render_pass.set_pipeline(&self.pipeline);
        render_pass.set_bind_group(0, &self.bind_group, &[]);
        render_pass.draw(0..3, 0..1);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    const TARGET_SIZE: u32 = 64;
    const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
    const GPU_TEST_TIMEOUT: Duration = Duration::from_secs(10);

    #[test]
    fn invalid_radius_becomes_square_without_leaking_nan_to_gpu() {
        assert_eq!(
            WindowCornerMask::rounded_in_points(f32::NAN),
            WindowCornerMask::square()
        );
        assert_eq!(
            WindowCornerMask::rounded_in_points(-4.0),
            WindowCornerMask::square()
        );
    }

    /// Исполняет настоящий render pass и возвращает RGBA readback либо `None` без GPU adapter-а.
    fn render_mask_to_readback(
        alpha_encoding: SurfaceAlphaEncoding,
        mask: WindowCornerMask,
    ) -> Option<(Vec<u8>, bool)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("window corner mask test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .expect("create window corner mask test device");
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("window corner mask test target"),
            size: wgpu::Extent3d {
                width: TARGET_SIZE,
                height: TARGET_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bytes_per_row = TARGET_SIZE * 4;
        let padded_bytes_per_row = bytes_per_row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("window corner mask test readback"),
            size: u64::from(padded_bytes_per_row) * u64::from(TARGET_SIZE),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let renderer = WindowCornerMaskRenderer::new(&device, TARGET_FORMAT, alpha_encoding);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("window corner mask test encoder"),
        });
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("window corner mask test source frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.25,
                        g: 0.5,
                        b: 0.75,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let pass_executed = renderer.render(
            &queue,
            &mut encoder,
            &target,
            [TARGET_SIZE, TARGET_SIZE],
            1.0,
            mask,
        );
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(TARGET_SIZE),
                },
            },
            wgpu::Extent3d {
                width: TARGET_SIZE,
                height: TARGET_SIZE,
                depth_or_array_layers: 1,
            },
        );
        let submission = queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        let (sender, receiver) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).expect("send mask readback result");
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(GPU_TEST_TIMEOUT),
            })
            .expect("wait for window mask GPU pass");
        receiver
            .recv_timeout(GPU_TEST_TIMEOUT)
            .expect("receive mask readback callback")
            .expect("map mask readback");
        let mapped = slice.get_mapped_range();
        let padded_stride = usize::try_from(padded_bytes_per_row).expect("padded stride");
        let visible_stride = usize::try_from(bytes_per_row).expect("visible stride");
        let pixels = mapped
            .chunks_exact(padded_stride)
            .flat_map(|row| row[..visible_stride].iter().copied())
            .collect::<Vec<_>>();
        drop(mapped);
        readback.unmap();
        Some((pixels, pass_executed))
    }

    /// Читает один RGBA пиксель из плотно упакованного результата.
    fn pixel(readback: &[u8], x: usize, y: usize) -> [u8; 4] {
        let offset = (y * TARGET_SIZE as usize + x) * 4;
        readback[offset..offset + 4]
            .try_into()
            .expect("RGBA pixel is complete")
    }

    /// Реальный GPU-pass вырезает угол, сохраняет центр и создаёт antialiased границу.
    #[test]
    fn gpu_mask_preserves_alpha_encoding_and_square_path() {
        let Some((premultiplied, pass_executed)) = render_mask_to_readback(
            SurfaceAlphaEncoding::Premultiplied,
            WindowCornerMask::rounded_in_points(12.0),
        ) else {
            return;
        };
        assert!(pass_executed);
        assert_eq!(pixel(&premultiplied, 0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&premultiplied, 32, 32), [64, 128, 191, 255]);
        assert_eq!(pixel(&premultiplied, 24, 4), [64, 128, 191, 255]);
        assert!(
            premultiplied
                .chunks_exact(4)
                .any(|rgba| rgba[3] > 0 && rgba[3] < 255)
        );

        let (postmultiplied, _) = render_mask_to_readback(
            SurfaceAlphaEncoding::Postmultiplied,
            WindowCornerMask::rounded_in_points(12.0),
        )
        .expect("same adapter remains available");
        assert_eq!(pixel(&postmultiplied, 0, 0), [64, 128, 191, 0]);
        assert_eq!(pixel(&postmultiplied, 32, 32), [64, 128, 191, 255]);

        let (maximum_radius, _) = render_mask_to_readback(
            SurfaceAlphaEncoding::Premultiplied,
            WindowCornerMask::rounded_in_points(24.0),
        )
        .expect("same adapter remains available");
        assert_eq!(pixel(&maximum_radius, 14, 12), [64, 128, 191, 255]);
        assert_eq!(pixel(&maximum_radius, 50, 12), [64, 128, 191, 255]);
        assert_eq!(pixel(&maximum_radius, 32, 56), [64, 128, 191, 255]);

        let (square, square_pass_executed) = render_mask_to_readback(
            SurfaceAlphaEncoding::Premultiplied,
            WindowCornerMask::square(),
        )
        .expect("same adapter remains available");
        assert!(!square_pass_executed);
        assert_eq!(pixel(&square, 0, 0), [64, 128, 191, 255]);
    }
}
