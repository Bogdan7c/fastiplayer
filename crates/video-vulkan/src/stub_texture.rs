use std::collections::HashMap;

pub struct StubTextureCache {
    device: std::sync::Arc<wgpu::Device>,
    queue: std::sync::Arc<wgpu::Queue>,
    textures: HashMap<(u32, u32), wgpu::Texture>,
    next_id: u64,
}

impl StubTextureCache {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self {
            device: std::sync::Arc::new(device),
            queue: std::sync::Arc::new(queue),
            textures: HashMap::new(),
            next_id: 1,
        }
    }

    pub fn get_or_create(&mut self, width: u32, height: u32) -> u64 {
        let key = (width, height);
        if !self.textures.contains_key(&key) {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&format!("stub texture {}x{}", width, height)),
                size: wgpu::Extent3d {
                    width: width.max(1),
                    height: height.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });

            let data = vec![128u8; (width.max(1) * height.max(1) * 4) as usize];
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width.max(1) * 4),
                    rows_per_image: Some(height.max(1)),
                },
                wgpu::Extent3d {
                    width: width.max(1),
                    height: height.max(1),
                    depth_or_array_layers: 1,
                },
            );

            self.textures.insert(key, texture);
        }

        let id = self.next_id;
        self.next_id += 1;
        id
    }
}
