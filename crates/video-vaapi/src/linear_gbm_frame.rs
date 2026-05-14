use std::cell::RefCell;
use std::fs::File;
use std::iter::zip;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::ptr;
use std::rc::Rc;
use std::slice;
use std::sync::Arc;

use cros_codecs::libva::{
    Display, ExternalBufferDescriptor, MemoryType, Surface, UsageHint, VADRMPRIMESurfaceDescriptor,
    VADRMPRIMESurfaceDescriptorLayer, VADRMPRIMESurfaceDescriptorObject,
};
use cros_codecs::video_frame::{ReadMapping, VideoFrame, WriteMapping};
use cros_codecs::{DecodedFormat, Fourcc, Resolution};
use gbm_sys::{
    gbm_bo, gbm_bo_create, gbm_bo_destroy, gbm_bo_flags, gbm_bo_get_fd, gbm_bo_get_height,
    gbm_bo_get_modifier, gbm_bo_get_offset, gbm_bo_get_plane_count, gbm_bo_get_stride_for_plane,
    gbm_bo_get_width, gbm_bo_map, gbm_bo_transfer_flags, gbm_bo_unmap, gbm_create_device,
    gbm_device, gbm_device_destroy,
};
use nix::libc;

/// Путь к render node, который обычно доступен пользователю без root.
const RENDER_DRI_PATH: &str = "/dev/dri/renderD128";

/// Флаг GBM для буферов, пригодных как output VA-API decoder.
const GBM_BO_USE_HW_VIDEO_DECODER: u32 = 1 << 13;

/// Локальный GBM device для выделения output кадров.
#[derive(Debug)]
pub struct LinearGbmDevice {
    /// Raw GBM device pointer. Валиден пока жив `_device_file`.
    device: *mut gbm_device,
    /// File держит render node открытым на весь lifetime GBM device.
    _device_file: File,
}

impl LinearGbmDevice {
    /// Открывает render node и создаёт GBM device.
    pub fn open_default() -> anyhow::Result<Arc<Self>> {
        let device_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(Path::new(RENDER_DRI_PATH))
            .map_err(|e| anyhow::anyhow!("Failed to open {}: {}", RENDER_DRI_PATH, e))?;

        // SAFETY: fd валиден, потому что `device_file` открыт успешно и хранится в структуре.
        let device = unsafe { gbm_create_device(device_file.as_raw_fd()) };
        if device.is_null() {
            anyhow::bail!("Failed to create GBM device from {}", RENDER_DRI_PATH);
        }

        Ok(Arc::new(Self {
            device,
            _device_file: device_file,
        }))
    }

    /// Создаёт linear NV12 frame для VA-API decode output.
    pub fn new_nv12_frame(
        self: &Arc<Self>,
        resolution: Resolution,
    ) -> anyhow::Result<LinearGbmVideoFrame> {
        LinearGbmVideoFrame::new(self.clone(), resolution)
    }
}

impl Drop for LinearGbmDevice {
    fn drop(&mut self) {
        // SAFETY: `device` создан через `gbm_create_device` и освобождается ровно один раз.
        unsafe { gbm_device_destroy(self.device) };
    }
}

// SAFETY: device используется только decoder thread'ом, а lifetime защищён `Arc`.
unsafe impl Send for LinearGbmDevice {}
unsafe impl Sync for LinearGbmDevice {}

/// Output frame, который VA-API импортирует как DRM PRIME surface.
///
/// Отличие от `GenericDmaVideoFrame`: CPU readback выполняется через `gbm_bo_map`,
/// поэтому драйвер корректно обрабатывает выбранный layout буфера.
#[derive(Debug)]
pub struct LinearGbmVideoFrame {
    /// Формат кадра. Для текущего decoder path всегда NV12.
    fourcc: Fourcc,
    /// Видимое/coded разрешение output кадра.
    resolution: Resolution,
    /// Raw GBM buffer object с linear layout.
    bo: *mut gbm_bo,
    /// GBM device должен жить дольше BO.
    _device: Arc<LinearGbmDevice>,
}

impl LinearGbmVideoFrame {
    /// Создаёт linear NV12 BO с флагами, совместимыми с Intel VA-API decode.
    fn new(device: Arc<LinearGbmDevice>, resolution: Resolution) -> anyhow::Result<Self> {
        let usage = gbm_bo_flags::GBM_BO_USE_LINEAR | GBM_BO_USE_HW_VIDEO_DECODER;
        let fourcc = Fourcc::from(b"NV12");

        let bo = allocate_linear_decode_bo(&device, resolution, fourcc, usage)?;

        let frame = Self {
            fourcc,
            resolution,
            bo,
            _device: device,
        };
        frame
            .validate_frame()
            .map_err(|e| anyhow::anyhow!("Invalid linear GBM frame: {}", e))?;

        tracing::info!(
            width = frame.resolution.width,
            height = frame.resolution.height,
            bo_width = frame.bo_width(),
            bo_height = frame.bo_height(),
            modifier = frame.modifier(),
            pitches = ?frame.get_plane_pitch(),
            offsets = ?frame.plane_offsets(),
            "Linear GBM frame allocated"
        );

        Ok(frame)
    }

    /// Возвращает ширину BO, фактически выбранную GBM.
    fn bo_width(&self) -> u32 {
        // SAFETY: `bo` валиден пока жив self.
        unsafe { gbm_bo_get_width(self.bo) }
    }

    /// Возвращает высоту BO, фактически выбранную GBM.
    fn bo_height(&self) -> u32 {
        // SAFETY: `bo` валиден пока жив self.
        unsafe { gbm_bo_get_height(self.bo) }
    }

    /// Возвращает DRM modifier BO.
    fn modifier(&self) -> u64 {
        // SAFETY: `bo` валиден пока жив self.
        unsafe { gbm_bo_get_modifier(self.bo) }
    }

    /// Возвращает количество плоскостей, которое сообщает GBM.
    fn plane_count(&self) -> usize {
        // SAFETY: `bo` валиден пока жив self.
        let count = unsafe { gbm_bo_get_plane_count(self.bo) };
        count.max(1) as usize
    }

    /// Возвращает offset плоскостей. Если GBM сообщает один plane, строим NV12 layout вручную.
    fn plane_offsets(&self) -> Vec<usize> {
        if self.plane_count() >= 2 {
            return (0..self.num_planes())
                .map(|plane_index| {
                    // SAFETY: `bo` валиден, plane index ограничен `num_planes`.
                    unsafe { gbm_bo_get_offset(self.bo, plane_index as libc::c_int) as usize }
                })
                .collect();
        }

        let y_stride = self.get_plane_pitch()[0];
        vec![0, y_stride * self.resolution.height as usize]
    }

    /// Маппит BO через GBM и возвращает указатели на начало каждой плоскости.
    fn map_helper(&self, is_writable: bool) -> Result<LinearGbmMapping<'_>, String> {
        let permissions = if is_writable {
            gbm_bo_transfer_flags::GBM_BO_TRANSFER_READ_WRITE
        } else {
            gbm_bo_transfer_flags::GBM_BO_TRANSFER_READ
        };

        let mut mapped_stride = 0;
        let mut map_data = ptr::null_mut();

        // SAFETY: `bo` валиден; GBM возвращает map pointer или null при ошибке.
        let mapped_memory = unsafe {
            gbm_bo_map(
                self.bo,
                0,
                0,
                self.bo_width(),
                self.bo_height(),
                permissions,
                &mut mapped_stride,
                &mut map_data,
            )
        };
        if mapped_memory.is_null() {
            return Err("Failed to map linear GBM buffer".to_string());
        }

        let plane_offsets = self.plane_offsets();
        let plane_sizes = self.get_plane_size();
        let plane_ptrs = plane_offsets
            .iter()
            .map(|offset| {
                // SAFETY: offset получен из GBM или рассчитан внутри размера BO.
                unsafe { (mapped_memory as *mut u8).add(*offset) }
            })
            .collect();

        Ok(LinearGbmMapping {
            bo: self.bo,
            map_data,
            plane_ptrs,
            plane_sizes,
            is_writable,
            _frame: self,
        })
    }
}

/// Выделяет GBM BO для NV12 decode output.
///
/// На Intel UHD 620 Mesa часто не создаёт `NV12 + LINEAR + HW_VIDEO_DECODER`,
/// но создаёт linear `R8` BO достаточного размера. VA-API получает descriptor
/// как NV12 с ручными offset/stride, поэтому это остаётся валидным output buffer.
fn allocate_linear_decode_bo(
    device: &LinearGbmDevice,
    resolution: Resolution,
    fourcc: Fourcc,
    usage: u32,
) -> anyhow::Result<*mut gbm_bo> {
    // SAFETY: GBM device валиден; width/height/fourcc/usage передаются как plain values.
    let nv12_bo = unsafe {
        gbm_bo_create(
            device.device,
            resolution.width,
            resolution.height,
            u32::from(fourcc),
            usage,
        )
    };
    if !nv12_bo.is_null() {
        tracing::info!("Allocated native linear NV12 GBM BO");
        return Ok(nv12_bo);
    }

    // Fallback повторяет старый рабочий allocator: один R8 BO, внутри которого
    // вручную размещаются Y и UV плоскости NV12.
    let fallback_height = resolution
        .height
        .checked_mul(2)
        .ok_or_else(|| anyhow::anyhow!("GBM R8 fallback height overflow"))?;

    // SAFETY: GBM device валиден; R8 BO используется как contiguous byte storage.
    let r8_bo = unsafe {
        gbm_bo_create(
            device.device,
            resolution.width,
            fallback_height,
            u32::from(Fourcc::from(b"R8  ")),
            usage,
        )
    };
    if r8_bo.is_null() {
        anyhow::bail!(
            "Failed to allocate linear GBM buffer width={} height={} (NV12 and R8 fallback failed)",
            resolution.width,
            resolution.height
        );
    }

    tracing::warn!(
        width = resolution.width,
        height = resolution.height,
        fallback_height,
        "Native linear NV12 GBM BO allocation failed; using linear R8 storage with NV12 descriptor"
    );
    Ok(r8_bo)
}

impl Drop for LinearGbmVideoFrame {
    fn drop(&mut self) {
        // SAFETY: `bo` создан через `gbm_bo_create` и освобождается ровно один раз.
        unsafe { gbm_bo_destroy(self.bo) };
    }
}

// SAFETY: кадр используется decoder thread'ом; raw BO живёт внутри self и не копируется.
unsafe impl Send for LinearGbmVideoFrame {}
unsafe impl Sync for LinearGbmVideoFrame {}

/// VA-API external descriptor для `LinearGbmVideoFrame`.
pub struct LinearGbmExternalBufferDescriptor {
    /// Fourcc кадра.
    fourcc: Fourcc,
    /// DRM modifier.
    modifier: u64,
    /// Разрешение кадра.
    resolution: Resolution,
    /// Pitch плоскостей.
    pitches: Vec<usize>,
    /// Offset плоскостей.
    offsets: Vec<usize>,
    /// Exported DMA-BUF fd. Должен жить пока libva создаёт surface.
    export_file: File,
}

impl ExternalBufferDescriptor for LinearGbmExternalBufferDescriptor {
    const MEMORY_TYPE: MemoryType = MemoryType::DrmPrime2;
    type DescriptorAttribute = VADRMPRIMESurfaceDescriptor;

    fn va_surface_attribute(&mut self) -> Self::DescriptorAttribute {
        let objects = [
            VADRMPRIMESurfaceDescriptorObject {
                fd: self.export_file.as_raw_fd(),
                size: self.export_file.metadata().map(|m| m.len()).unwrap_or(0) as u32,
                drm_format_modifier: self.modifier,
            },
            Default::default(),
            Default::default(),
            Default::default(),
        ];

        let layers = [
            VADRMPRIMESurfaceDescriptorLayer {
                drm_format: u32::from(self.fourcc),
                num_planes: self.pitches.len() as u32,
                object_index: [0, 0, 0, 0],
                offset: self
                    .offsets
                    .iter()
                    .map(|offset| *offset as u32)
                    .chain(std::iter::repeat(0))
                    .take(4)
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap(),
                pitch: self
                    .pitches
                    .iter()
                    .map(|pitch| *pitch as u32)
                    .chain(std::iter::repeat(0))
                    .take(4)
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap(),
            },
            Default::default(),
            Default::default(),
            Default::default(),
        ];

        VADRMPRIMESurfaceDescriptor {
            fourcc: u32::from(self.fourcc),
            width: self.resolution.width,
            height: self.resolution.height,
            num_objects: 1,
            objects,
            num_layers: 1,
            layers,
        }
    }
}

impl VideoFrame for LinearGbmVideoFrame {
    type MemDescriptor = LinearGbmExternalBufferDescriptor;
    type NativeHandle = Surface<LinearGbmExternalBufferDescriptor>;

    fn fourcc(&self) -> Fourcc {
        self.fourcc
    }

    fn resolution(&self) -> Resolution {
        self.resolution
    }

    fn get_plane_size(&self) -> Vec<usize> {
        let vertical_subsampling = self.get_vertical_subsampling();
        zip(self.get_plane_pitch(), vertical_subsampling)
            .map(|(pitch, subsampling)| pitch * self.resolution.height as usize / subsampling)
            .collect()
    }

    fn get_plane_pitch(&self) -> Vec<usize> {
        if self.plane_count() >= 2 {
            return (0..self.num_planes())
                .map(|plane_index| {
                    // SAFETY: `bo` валиден, plane index ограничен `num_planes`.
                    unsafe {
                        gbm_bo_get_stride_for_plane(self.bo, plane_index as libc::c_int) as usize
                    }
                })
                .collect();
        }

        // SAFETY: для single-plane fallback stride plane 0 валиден.
        let y_stride = unsafe { gbm_bo_get_stride_for_plane(self.bo, 0) as usize };
        vec![y_stride, y_stride]
    }

    fn map<'a>(&'a self) -> Result<Box<dyn ReadMapping<'a> + 'a>, String> {
        Ok(Box::new(self.map_helper(false)?))
    }

    fn map_mut<'a>(&'a mut self) -> Result<Box<dyn WriteMapping<'a> + 'a>, String> {
        Ok(Box::new(self.map_helper(true)?))
    }

    fn to_native_handle(&self, display: &Rc<Display>) -> Result<Self::NativeHandle, String> {
        if self.decoded_format()? != DecodedFormat::NV12 {
            return Err("Only NV12 VA-API export is supported".to_string());
        }

        // SAFETY: `gbm_bo_get_fd` возвращает новый owned fd для валидного BO.
        let exported_fd = unsafe { gbm_bo_get_fd(self.bo) };
        if exported_fd < 0 {
            return Err("Failed to export GBM BO fd".to_string());
        }

        let descriptor = LinearGbmExternalBufferDescriptor {
            fourcc: self.fourcc,
            modifier: self.modifier(),
            resolution: self.resolution,
            pitches: self.get_plane_pitch(),
            offsets: self.plane_offsets(),
            // SAFETY: fd новый и owned, теперь им владеет `File`.
            export_file: unsafe { File::from_raw_fd(exported_fd) },
        };

        let mut surfaces = display
            .create_surfaces(
                cros_codecs::libva::VA_RT_FORMAT_YUV420,
                Some(u32::from(self.fourcc)),
                self.resolution.width,
                self.resolution.height,
                Some(UsageHint::USAGE_HINT_DECODER),
                vec![descriptor],
            )
            .map_err(|e| format!("Failed to import linear GBM frame to VA-API: {e:?}"))?;

        Ok(surfaces.pop().unwrap())
    }
}

/// Активный GBM mapping одного кадра.
pub struct LinearGbmMapping<'a> {
    /// BO, который нужно unmap при drop.
    bo: *mut gbm_bo,
    /// Opaque GBM map token.
    map_data: *mut libc::c_void,
    /// Указатели на начала плоскостей.
    plane_ptrs: Vec<*mut u8>,
    /// Размеры плоскостей.
    plane_sizes: Vec<usize>,
    /// Разрешает mutable доступ.
    is_writable: bool,
    /// Привязывает lifetime mapping к frame.
    _frame: &'a LinearGbmVideoFrame,
}

impl<'a> ReadMapping<'a> for LinearGbmMapping<'a> {
    fn get(&self) -> Vec<&[u8]> {
        // SAFETY: mapping жив до drop self; pointers и sizes рассчитаны из валидного BO layout.
        unsafe {
            zip(self.plane_ptrs.iter(), self.plane_sizes.iter())
                .map(|(ptr, len)| slice::from_raw_parts(*ptr as *const u8, *len))
                .collect()
        }
    }
}

impl<'a> WriteMapping<'a> for LinearGbmMapping<'a> {
    fn get(&self) -> Vec<RefCell<&'a mut [u8]>> {
        if !self.is_writable {
            panic!("Attempted to get writable slices from read-only GBM mapping");
        }

        // SAFETY: caller requested writable mapping; lifetime привязан к self/_frame.
        unsafe {
            zip(self.plane_ptrs.iter(), self.plane_sizes.iter())
                .map(|(ptr, len)| RefCell::new(slice::from_raw_parts_mut(*ptr, *len)))
                .collect()
        }
    }
}

impl Drop for LinearGbmMapping<'_> {
    fn drop(&mut self) {
        // SAFETY: `map_data` получен из `gbm_bo_map` для этого BO.
        unsafe { gbm_bo_unmap(self.bo, self.map_data) };
    }
}
