//! Конкретный WGPU staging backend HostPlanar upload-а.
//!
//! Здесь сосредоточены allocation textures, копия плоскостей в mapped staging,
//! alignment и запись GPU-копий кадра в один command buffer. Сам backend этот
//! command buffer не отправляет: он ждёт submit-а кадра (см. `pending_upload`).
//! Materializer/pool зависят только от trait-а.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow, ensure};

use crate::video::FrameBindGroupCache;

use super::direct_upload::DirectHostPlanarUpload;
use super::host_memory_import::{HostMemoryImportStats, HostMemoryImporter};
use super::pending_upload::PendingHostPlanarUpload;
use super::{
    DirectFrameUploadRecording, HostPlanarUploadBackend, HostPlanarUploadLayout,
    HostPlanarUploadPlaneLayout,
};

pub(super) struct WgpuHostPlanarUploadBackend {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Копии plane→texture текущего кадра, которые ещё пишутся в encoder.
    recording: Option<FrameUploadRecording>,
    /// Последняя записанная копия: если её не забрал нарисованный кадр, она уходит
    /// в очередь отдельным submit-ом перед следующей загрузкой.
    last_recorded_upload: Option<Arc<PendingHostPlanarUpload>>,
    /// Переиспользуемые mapped staging chunks: без per-frame allocate+zero-init.
    staging_belt: Option<wgpu::util::StagingBelt>,
    /// Импорт памяти кадров декодера в GPU (cpu-parity 08b); `None` — только staging.
    host_memory_importer: Option<HostMemoryImporter>,
    /// Счётчики путей загрузки кадров.
    upload_path_stats: HostPlanarUploadPathStats,
    /// Причина первого кадра с блоком, не подошедшего для прямой копии, уже в логе.
    unsuitable_direct_upload_reported: bool,
}

/// Сколько кадров загружено каждым путём и как работает импорт.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostPlanarUploadPathStats {
    /// Кадры, скопированные GPU прямо из памяти декодера (без CPU-копии).
    pub frames_without_cpu_copy: u64,
    /// Кадры, скопированные на CPU в staging.
    pub frames_through_staging: u64,
    /// Кадры с импортируемым блоком, который не подошёл для прямой копии.
    pub unsuitable_direct_uploads: u64,
    /// Счётчики импортёра (нули, если импорт недоступен).
    pub host_memory_import: HostMemoryImportStats,
}

/// GPU-копии плоскостей одного кадра в один слот, пока они пишутся в encoder.
struct FrameUploadRecording {
    encoder: wgpu::CommandEncoder,
    /// Ожидающая загрузка слота-получателя: туда уйдёт готовый command buffer.
    destination: Arc<PendingHostPlanarUpload>,
}

impl WgpuHostPlanarUploadBackend {
    pub(super) fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self {
            device: device.clone(),
            queue,
            recording: None,
            last_recorded_upload: None,
            staging_belt: None,
            host_memory_importer: HostMemoryImporter::new(&device),
            upload_path_stats: HostPlanarUploadPathStats::default(),
            unsuitable_direct_upload_reported: false,
        }
    }

    /// Счётчики путей загрузки (тесты и диагностика).
    pub(super) fn upload_path_stats(&self) -> HostPlanarUploadPathStats {
        HostPlanarUploadPathStats {
            host_memory_import: self
                .host_memory_importer
                .as_ref()
                .map(HostMemoryImporter::stats)
                .unwrap_or_default(),
            ..self.upload_path_stats
        }
    }

    /// Готовит запись копий в слот `destination`: продолжает текущую или начинает новую.
    ///
    /// Плоскости одного кадра приходят подряд в один слот, поэтому запись продолжается,
    /// пока слот тот же. Новый слот означает новый кадр:
    /// - недописанная запись прошлого кадра (upload упал посреди плоскостей) выбрасывается:
    ///   у её слота уже нет handle-а, его частично записанные pixels никто не покажет;
    /// - невостребованная копия прошлого кадра отправляется в очередь раньше новой.
    fn ensure_frame_upload_recording(&mut self, destination: &Arc<PendingHostPlanarUpload>) {
        let continues_current_frame = self
            .recording
            .as_ref()
            .is_some_and(|recording| Arc::ptr_eq(&recording.destination, destination));
        if continues_current_frame {
            return;
        }
        self.recording = None;
        self.submit_unclaimed_upload();
        self.recording = Some(FrameUploadRecording {
            encoder: self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("host-planar-frame-upload"),
                }),
            destination: Arc::clone(destination),
        });
    }

    /// Отправляет последнюю копию отдельным submit-ом, если её не забрал ни один кадр.
    ///
    /// В обычном воспроизведении копию забирает submit кадра и здесь ничего не
    /// происходит. Отдельный submit нужен, когда кадр загрузили, но не нарисовали
    /// (рендер упал, кадр вытеснен другим до показа): копия всё равно должна попасть
    /// в очередь, иначе слот останется без pixels, а staging-чанки — без возврата.
    fn submit_unclaimed_upload(&mut self) {
        let unclaimed_commands = self
            .last_recorded_upload
            .take()
            .and_then(|pending_upload| pending_upload.take_unclaimed());
        if let Some(commands) = unclaimed_commands {
            self.queue.submit(std::iter::once(commands));
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

    fn supports_direct_frame_upload(&self) -> bool {
        self.host_memory_importer.is_some()
    }

    /// Пишет копии plane-ов из импортированного блока кадра в текстуры слота.
    ///
    /// Копия идёт в тот же command buffer кадра, что и копия через staging, и уходит в
    /// очередь тем же путём (submit кадра или невостребованная копия перед следующей
    /// загрузкой), поэтому инварианты `pending_upload` не меняются.
    fn record_direct_frame_upload(
        &mut self,
        uploaded_textures: &Self::UploadedTextures,
        upload: &DirectHostPlanarUpload,
    ) -> Result<DirectFrameUploadRecording> {
        let Some(importer) = self.host_memory_importer.as_mut() else {
            return Ok(DirectFrameUploadRecording::NotRecorded);
        };
        let Some(source_buffer) = importer.frame_source_buffer(upload) else {
            return Ok(DirectFrameUploadRecording::NotRecorded);
        };

        self.ensure_frame_upload_recording(&uploaded_textures.pending_upload);
        let recording = self
            .recording
            .as_mut()
            .context("host-planar upload recording was not started")?;
        for plane_index in 0..uploaded_textures.layout.planes.len() {
            let plane_layout = uploaded_textures.layout.plane(plane_index)?;
            let plane_copy = upload
                .plane(plane_index)
                .with_context(|| format!("direct upload plan has no plane {plane_index}"))?;
            recording.encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &source_buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: plane_copy.offset,
                        bytes_per_row: Some(plane_copy.bytes_per_row),
                        rows_per_image: None,
                    },
                },
                wgpu::TexelCopyTextureInfo {
                    texture: uploaded_textures.plane_texture(plane_index)?,
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
        }
        // Обёртка отпускается сразу: wgpu держит её до выполнения submit-а с копией, а
        // потом её callback отпустит кадр и импорт.
        drop(source_buffer);
        self.upload_path_stats.frames_without_cpu_copy += 1;
        Ok(DirectFrameUploadRecording::Recorded)
    }

    fn note_unsuitable_direct_upload(&mut self, reason: &anyhow::Error) {
        self.upload_path_stats.unsuitable_direct_uploads += 1;
        if !self.unsuitable_direct_upload_reported {
            self.unsuitable_direct_upload_reported = true;
            tracing::warn!(
                reason = %reason,
                "Software video frame cannot be uploaded from decoder memory; using the staging copy"
            );
        }
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

        // Staging belt + copy_buffer_to_texture вместо Queue::write_texture: копия идёт
        // в переиспользуемый mapped chunk, а GPU-копии всех плоскостей кадра пишутся в
        // один command buffer, который уходит в очередь вместе с кадром.
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

        // Запись начинается до выделения staging: невостребованная копия прошлого кадра
        // уходит в очередь раньше, и её staging-чанк быстрее возвращается в belt.
        self.ensure_frame_upload_recording(&uploaded_textures.pending_upload);
        if plane_index == 0 {
            self.upload_path_stats.frames_through_staging += 1;
        }

        let device = self.device.clone();
        let staging_belt = self.staging_belt.get_or_insert_with(|| {
            wgpu::util::StagingBelt::new(device, HOST_PLANAR_STAGING_BELT_CHUNK_BYTES)
        });
        let staging_size =
            wgpu::BufferSize::new(staging_len).context("host-planar staging length is zero")?;
        let staging_alignment = const {
            wgpu::BufferSize::new(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64)
                .expect("copy alignment is non-zero")
        };
        let staging_slice = staging_belt.allocate(staging_size, staging_alignment);
        {
            // wgpu 30 возвращает ошибку маппинга как `Result` (в wgpu 29 это была паника
            // внутри wgpu). Свежий slice из belt всегда mapped, поэтому на рабочем пути
            // поведение прежнее; при нарушении инварианта кадр получает типизированную
            // ошибку upload-а вместо паники.
            let mut mapped = staging_slice
                .get_mapped_range_mut()
                .context("host-planar staging belt slice is not mapped for writing")?;
            copy_plane_block_into_staging(
                block_bytes,
                mapped.slice(..),
                stride,
                staging_stride as usize,
                visible_row_bytes,
                plane_layout.height as usize,
            );
        }

        // Один encoder копит все плоскости кадра до передачи в submit кадра.
        let recording = self
            .recording
            .as_mut()
            .context("host-planar upload recording was not started")?;
        recording.encoder.copy_buffer_to_texture(
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

    /// Закрывает запись копий кадра и кладёт готовый command buffer в слот-получатель.
    ///
    /// Submit-а здесь нет: копию отправит shell вместе с кадром, который рисует слот
    /// (или сам backend перед следующей загрузкой, если кадр не нарисуют).
    fn flush_plane_uploads(&mut self) -> Result<()> {
        let Some(FrameUploadRecording {
            encoder,
            destination,
        }) = self.recording.take()
        else {
            return Ok(());
        };
        // Обычно невостребованная копия уже ушла в начале записи. Сюда она доходит, только
        // если запись продолжила недописанный кадр в том же слоте (upload упал посреди
        // плоскостей): тогда начала записи не было, а прежняя копия старше новой.
        self.submit_unclaimed_upload();
        if let Some(staging_belt) = self.staging_belt.as_mut() {
            // Чанки belt-а вернутся (re-map) после submit-а именно этого command buffer-а
            // и завершения GPU-копии — кто бы его ни отправил: shell с кадром или backend.
            staging_belt.finish_and_recall_on_submit(&encoder);
        }
        if let Some(older_commands) = destination.store_recorded_commands(encoder.finish()) {
            // По протоколу недостижимо: невостребованная копия уходит в очередь выше.
            // Если всё же осталась, она старше только что записанной и обязана
            // выполниться раньше — отправляем её сейчас, новая ещё ждёт submit-а.
            self.queue.submit(std::iter::once(older_commands));
        }
        self.last_recorded_upload = Some(destination);
        Ok(())
    }
}

/// Размер chunk-а staging belt: вмещает все планы 4K-кадра одним chunk-ом.
const HOST_PLANAR_STAGING_BELT_CHUNK_BYTES: u64 = 16 * 1024 * 1024;

/// Копирует видимые строки plane block-а в mapped staging на текущем потоке.
///
/// Копия намеренно однопоточная. Она упирается в пропускную способность памяти, а не в
/// ядра: замер cpu-parity 07 (AV1 4K60, Ryzen 7 7840HS) — копия в 4 потока давала
/// `video_prepare` 1,07 мс против 1,12 мс одним потоком, а создание потоков на каждый
/// кадр стоило ~8 п.п. ядра всего процесса. Строки копируются по одной, потому что
/// stride источника (FFmpeg) и staging (выравнивание 256 байт) в общем случае разные.
fn copy_plane_block_into_staging(
    block_bytes: &[u8],
    mut staging: wgpu::WriteOnly<'_, [u8]>,
    src_stride: usize,
    dst_stride: usize,
    visible_row_bytes: usize,
    visible_height: usize,
) {
    for row in 0..visible_height {
        let src_offset = row * src_stride;
        let dst_offset = row * dst_stride;
        staging
            .slice(dst_offset..dst_offset + visible_row_bytes)
            .copy_from_slice(&block_bytes[src_offset..src_offset + visible_row_bytes]);
    }
}

pub(super) struct WgpuHostPlanarUploadedTextures {
    /// Кэш bind group-ов рендерера для этого слота; умирает вместе со слотом.
    /// Объявлен первым: bind group-ы отпускаются раньше views/textures.
    pub(super) bind_groups: FrameBindGroupCache,
    /// Записанная, но ещё не отправленная GPU-копия плоскостей в этот слот.
    pub(super) pending_upload: Arc<PendingHostPlanarUpload>,
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
            bind_groups: FrameBindGroupCache::default(),
            pending_upload: Arc::default(),
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

#[cfg(test)]
mod tests;
