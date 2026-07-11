//! Конкретный WGPU staging backend HostPlanar upload-а.
//!
//! Здесь сосредоточены allocation textures, mapped staging bands, alignment и
//! один batched submit на кадр. Materializer/pool зависят только от trait-а.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow, ensure};

use super::{HostPlanarUploadBackend, HostPlanarUploadLayout, HostPlanarUploadPlaneLayout};

pub(super) struct WgpuHostPlanarUploadBackend {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Копии plane→texture одного кадра батчатся в один encoder/submit.
    pending_upload_encoder: Option<wgpu::CommandEncoder>,
    /// Переиспользуемые mapped staging chunks: без per-frame allocate+zero-init.
    staging_belt: Option<wgpu::util::StagingBelt>,
}

impl WgpuHostPlanarUploadBackend {
    pub(super) fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self {
            device,
            queue,
            pending_upload_encoder: None,
            staging_belt: None,
        }
    }
}

impl HostPlanarUploadBackend for WgpuHostPlanarUploadBackend {
    type UploadedTextures = Arc<WgpuHostPlanarUploadedTextures>;

    fn allocate_textures(
        &mut self,
        layout: HostPlanarUploadLayout,
    ) -> Result<Self::UploadedTextures> {
        Ok(Arc::new(WgpuHostPlanarUploadedTextures::new(
            &self.device,
            layout,
        )))
    }

    fn upload_plane_block(
        &mut self,
        uploaded_textures: &Self::UploadedTextures,
        plane_index: usize,
        block_bytes: &[u8],
        stride: usize,
        visible_height: u32,
    ) -> Result<()> {
        let plane_layout = uploaded_textures.layout.plane(plane_index)?;
        let texture = uploaded_textures.plane_texture(plane_index)?;
        let visible_row_bytes = plane_layout.visible_row_bytes()?;

        ensure!(
            visible_height == plane_layout.height,
            "host-planar {:?} upload block height {} does not match texture height {}",
            plane_layout.role,
            visible_height,
            plane_layout.height
        );
        ensure!(
            stride >= visible_row_bytes,
            "host-planar {:?} upload stride {} is smaller than visible row bytes {}",
            plane_layout.role,
            stride,
            visible_row_bytes
        );

        let expected_block_bytes = stride
            .checked_mul(plane_layout.height.saturating_sub(1) as usize)
            .and_then(|rows| rows.checked_add(visible_row_bytes))
            .context("host-planar upload block length overflow")?;
        ensure!(
            block_bytes.len() == expected_block_bytes,
            "host-planar {:?} upload block has {} bytes, expected {}",
            plane_layout.role,
            block_bytes.len(),
            expected_block_bytes
        );

        // Staging belt + copy_buffer_to_texture вместо Queue::write_texture: memcpy 4K
        // plane (8-12МБ) на memory-bandwidth-bound CPU под нагрузкой декодера стоил до
        // 15-30мс одним потоком внутри write_texture. Полосная копия в переиспользуемый
        // mapped chunk срезает хвост p99, а GPU-копии батчатся в один submit на кадр.
        // Belt (а не create_buffer(mapped_at_creation) на кадр) — иначе wgpu каждый раз
        // zero-инициализирует буфер, что дороже самой копии.
        let staging_stride = u32::try_from(visible_row_bytes)
            .ok()
            .map(|row_bytes| row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT))
            .context("host-planar staging row bytes do not fit u32")?;
        let staging_len = (staging_stride as u64)
            .checked_mul(u64::from(plane_layout.height.saturating_sub(1)))
            .and_then(|rows| rows.checked_add(visible_row_bytes as u64))
            .context("host-planar staging length overflow")?
            .next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);

        let device = self.device.clone();
        let staging_belt = self.staging_belt.get_or_insert_with(|| {
            wgpu::util::StagingBelt::new(device, HOST_PLANAR_STAGING_BELT_CHUNK_BYTES)
        });
        let staging_size =
            wgpu::BufferSize::new(staging_len).context("host-planar staging length is zero")?;
        let staging_alignment =
            wgpu::BufferSize::new(u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT))
                .expect("copy alignment is non-zero");
        let staging_slice = staging_belt.allocate(staging_size, staging_alignment);
        {
            let mut mapped = staging_slice.get_mapped_range_mut();
            copy_plane_block_into_staging(
                block_bytes,
                mapped.slice(..),
                stride,
                staging_stride as usize,
                visible_row_bytes,
                plane_layout.height as usize,
            );
        }

        if self.pending_upload_encoder.is_none() {
            self.pending_upload_encoder = Some(self.device.create_command_encoder(
                &wgpu::CommandEncoderDescriptor {
                    label: Some("host-planar-frame-upload"),
                },
            ));
        }
        let encoder = self
            .pending_upload_encoder
            .as_mut()
            .expect("pending upload encoder installed above");
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: staging_slice.buffer(),
                layout: wgpu::TexelCopyBufferLayout {
                    offset: staging_slice.offset(),
                    bytes_per_row: Some(staging_stride),
                    rows_per_image: None,
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: plane_layout.width,
                height: plane_layout.height,
                depth_or_array_layers: 1,
            },
        );

        Ok(())
    }

    fn uploaded_textures_are_idle(&self, uploaded_textures: &Self::UploadedTextures) -> bool {
        Arc::strong_count(uploaded_textures) == 1
    }

    fn flush_plane_uploads(&mut self) -> Result<()> {
        if let Some(encoder) = self.pending_upload_encoder.take() {
            if let Some(staging_belt) = self.staging_belt.as_mut() {
                staging_belt.finish();
            }
            self.queue.submit(std::iter::once(encoder.finish()));
            if let Some(staging_belt) = self.staging_belt.as_mut() {
                // Возврат chunk-ов в belt: map_async завершится на обычных device polls
                // render-цикла, к следующему кадру chunk снова доступен без zero-init.
                staging_belt.recall();
            }
        }
        Ok(())
    }
}

/// Размер chunk-а staging belt: вмещает все планы 4K-кадра одним chunk-ом.
const HOST_PLANAR_STAGING_BELT_CHUNK_BYTES: u64 = 16 * 1024 * 1024;

/// Полоса меньше этого объёма копируется без отдельного потока.
const HOST_PLANAR_STAGING_COPY_BAND_MIN_BYTES: usize = 1024 * 1024;

/// Верхний предел потоков полосной staging-копии одного plane.
const HOST_PLANAR_STAGING_COPY_MAX_THREADS: usize = 4;

/// Дизъюнктная полоса mapped staging, передаваемая scoped copy-потоку.
///
/// # Safety-обоснование
/// `WriteOnly<'_, [u8]>` указывает в host-mapped память staging buffer-а; полосы
/// получены через `split_at` и не пересекаются, а запись mapped-байтов в wgpu не
/// привязана к конкретному потоку (mapping/unmap остаются на вызывающем потоке).
/// `Send` у `WriteOnly<[u8]>` отсутствует только из-за `Sized`-bound generic-а.
struct StagingCopyBand<'a>(wgpu::WriteOnly<'a, [u8]>);

// SAFETY: полосы дизъюнктны и указывают в host-mapped память; см. док выше.
unsafe impl Send for StagingCopyBand<'_> {}

/// Копирует plane block в mapped staging полосами в несколько потоков.
///
/// Одиночный memcpy 4K-плоскости упирается в memory bandwidth и под нагрузкой
/// software-декодера растягивается до десятков миллисекунд; полосы делят копию
/// между ядрами и держат стадию upload в бюджете кадра.
fn copy_plane_block_into_staging(
    block_bytes: &[u8],
    staging: wgpu::WriteOnly<'_, [u8]>,
    src_stride: usize,
    dst_stride: usize,
    visible_row_bytes: usize,
    visible_height: usize,
) {
    if visible_height == 0 || visible_row_bytes == 0 {
        return;
    }

    let total_bytes = visible_row_bytes.saturating_mul(visible_height);
    let band_count = (total_bytes / HOST_PLANAR_STAGING_COPY_BAND_MIN_BYTES)
        .clamp(1, HOST_PLANAR_STAGING_COPY_MAX_THREADS)
        .min(visible_height);
    let rows_per_band = visible_height.div_ceil(band_count);

    std::thread::scope(|scope| {
        let mut staging_rest = staging;
        let mut row_start = 0usize;
        while row_start < visible_height {
            let band_rows = rows_per_band.min(visible_height - row_start);
            let is_last_band = row_start + band_rows == visible_height;
            let dst_band;
            if is_last_band {
                dst_band = staging_rest;
                staging_rest = wgpu::WriteOnly::from_mut(&mut []);
            } else {
                let (band, rest) = staging_rest.split_at(band_rows * dst_stride);
                dst_band = band;
                staging_rest = rest;
            }

            let base_row = row_start;
            let dst_band = StagingCopyBand(dst_band);
            let copy_band = move || {
                // Перенос всей обёртки одним place-выражением: precise capture поля .0
                // обошёл бы Send impl обёртки (деструктуризация в pattern не спасает).
                let band = dst_band;
                let StagingCopyBand(mut dst_band) = band;
                for row in 0..band_rows {
                    let src_offset = (base_row + row) * src_stride;
                    let dst_offset = row * dst_stride;
                    dst_band
                        .slice(dst_offset..dst_offset + visible_row_bytes)
                        .copy_from_slice(&block_bytes[src_offset..src_offset + visible_row_bytes]);
                }
            };
            if is_last_band {
                // Последняя полоса на текущем потоке: scope join не ждёт лишний spawn.
                copy_band();
            } else {
                scope.spawn(copy_band);
            }

            row_start += band_rows;
        }
    });
}

pub(super) struct WgpuHostPlanarUploadedTextures {
    layout: HostPlanarUploadLayout,
    y_texture: wgpu::Texture,
    u_texture: wgpu::Texture,
    v_texture: wgpu::Texture,
    pub(super) y_view: wgpu::TextureView,
    pub(super) u_view: wgpu::TextureView,
    pub(super) v_view: wgpu::TextureView,
}

impl WgpuHostPlanarUploadedTextures {
    fn new(device: &wgpu::Device, layout: HostPlanarUploadLayout) -> Self {
        let y_texture =
            create_upload_plane_texture(device, "host planar Y upload texture", layout.planes[0]);
        let u_texture =
            create_upload_plane_texture(device, "host planar U upload texture", layout.planes[1]);
        let v_texture =
            create_upload_plane_texture(device, "host planar V upload texture", layout.planes[2]);
        let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("host planar Y upload texture view"),
            ..Default::default()
        });
        let u_view = u_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("host planar U upload texture view"),
            ..Default::default()
        });
        let v_view = v_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("host planar V upload texture view"),
            ..Default::default()
        });

        Self {
            layout,
            y_texture,
            u_texture,
            v_texture,
            y_view,
            u_view,
            v_view,
        }
    }

    fn plane_texture(&self, plane_index: usize) -> Result<&wgpu::Texture> {
        match plane_index {
            0 => Ok(&self.y_texture),
            1 => Ok(&self.u_texture),
            2 => Ok(&self.v_texture),
            _ => Err(anyhow!(
                "host-planar upload plane index {plane_index} is out of bounds"
            )),
        }
    }
}

fn create_upload_plane_texture(
    device: &wgpu::Device,
    label: &'static str,
    plane_layout: HostPlanarUploadPlaneLayout,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: plane_layout.width,
            height: plane_layout.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: plane_layout.texture_format.wgpu_format(),
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}
