use std::fs::File;
use std::iter::zip;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::rc::Rc;

use cros_codecs::libva::{
    Display, ExternalBufferDescriptor, MemoryType, Surface, UsageHint, VADRMPRIMESurfaceDescriptor,
    VADRMPRIMESurfaceDescriptorLayer, VADRMPRIMESurfaceDescriptorObject,
};
use cros_codecs::video_frame::{ReadMapping, VideoFrame, WriteMapping};
use cros_codecs::{DecodedFormat, Fourcc, Resolution};
use gbm_sys::{
    gbm_bo, gbm_bo_create, gbm_bo_destroy, gbm_bo_flags, gbm_bo_get_fd, gbm_bo_get_height,
    gbm_bo_get_modifier, gbm_bo_get_offset, gbm_bo_get_plane_count, gbm_bo_get_stride_for_plane,
    gbm_bo_get_width, gbm_create_device, gbm_device, gbm_device_destroy,
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
    pub fn open_default() -> anyhow::Result<Rc<Self>> {
        let device_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(Path::new(RENDER_DRI_PATH))
            .map_err(|e| anyhow::anyhow!("Failed to open {}: {}", RENDER_DRI_PATH, e))?;

        // SAFETY: `device_file` содержит открытый DRM render-node fd. `Self`
        // сохраняет `File` дольше raw `gbm_device*`, а null-result обрабатывается
        // до создания Rust owner-а.
        let device = unsafe { gbm_create_device(device_file.as_raw_fd()) };
        if device.is_null() {
            anyhow::bail!("Failed to create GBM device from {}", RENDER_DRI_PATH);
        }

        Ok(Rc::new(Self {
            device,
            _device_file: device_file,
        }))
    }

    /// Создаёт linear NV12 frame для VA-API decode output.
    pub fn new_nv12_frame(
        self: &Rc<Self>,
        resolution: Resolution,
    ) -> anyhow::Result<LinearGbmVideoFrame> {
        LinearGbmVideoFrame::new(self.clone(), resolution)
    }
}

impl Drop for LinearGbmDevice {
    fn drop(&mut self) {
        // SAFETY: `device` создан единственным успешным `gbm_create_device` и
        // освобождается только здесь. `Rc` запрещает межпоточную передачу и
        // гарантирует, что все `LinearGbmVideoFrame` уже уничтожены. Поле
        // `_device_file` закрывается автоматически только после этого вызова.
        unsafe { gbm_device_destroy(self.device) };
    }
}

/// Output frame, который VA-API импортирует как DRM PRIME surface.
///
/// Кадр предназначен только для owner-thread VA import. CPU mapping намеренно
/// не предоставляется: GBM не документирует размер многоплоскостного mapping,
/// достаточный для безопасного создания Rust slices.
#[derive(Debug)]
pub struct LinearGbmVideoFrame {
    /// Формат кадра. Для текущего decoder path всегда NV12.
    fourcc: Fourcc,
    /// Видимое/coded разрешение output кадра.
    resolution: Resolution,
    /// Raw GBM buffer object с linear layout.
    bo: *mut gbm_bo,
    /// GBM device должен жить дольше BO.
    _device: Rc<LinearGbmDevice>,
}

impl LinearGbmVideoFrame {
    /// Создаёт linear NV12 BO с флагами, совместимыми с Intel VA-API decode.
    fn new(device: Rc<LinearGbmDevice>, resolution: Resolution) -> anyhow::Result<Self> {
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
        // SAFETY: `bo` принадлежит `self`, тип закреплён за одним потоком, а
        // активная операция destroy невозможна до завершения `&self`.
        unsafe { gbm_bo_get_width(self.bo) }
    }

    /// Возвращает высоту BO, фактически выбранную GBM.
    fn bo_height(&self) -> u32 {
        // SAFETY: те же owner-thread и lifetime инварианты, что у `bo_width`.
        unsafe { gbm_bo_get_height(self.bo) }
    }

    /// Возвращает DRM modifier BO.
    fn modifier(&self) -> u64 {
        // SAFETY: те же owner-thread и lifetime инварианты, что у `bo_width`.
        unsafe { gbm_bo_get_modifier(self.bo) }
    }

    /// Возвращает количество плоскостей, которое сообщает GBM.
    fn plane_count(&self) -> usize {
        // SAFETY: те же owner-thread и lifetime инварианты, что у `bo_width`.
        let count = unsafe { gbm_bo_get_plane_count(self.bo) };
        count.max(1) as usize
    }

    /// Возвращает offset плоскостей. Если GBM сообщает один plane, строим NV12 layout вручную.
    fn plane_offsets(&self) -> Vec<usize> {
        if self.plane_count() >= 2 {
            return (0..self.num_planes())
                .map(|plane_index| {
                    // SAFETY: owner-thread/lifetime инварианты сохраняют `bo`.
                    // Эта ветка выполняется только когда GBM сообщил минимум
                    // две плоскости; NV12 `num_planes()` возвращает ровно две.
                    unsafe { gbm_bo_get_offset(self.bo, plane_index as libc::c_int) as usize }
                })
                .collect();
        }

        let y_stride = self.get_plane_pitch()[0];
        vec![0, y_stride * self.resolution.height as usize]
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
    // SAFETY: `device.device` принадлежит текущему owner thread и удерживается
    // `Rc` дольше результата. Остальные аргументы — значения; null-result
    // обрабатывается до создания Rust owner-а.
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

    // SAFETY: тот же owner-thread/lifetime contract, что у первой попытки.
    // R8 dimensions описывают полный contiguous storage; null обрабатывается.
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
        // SAFETY: `bo` создан единственным успешным `gbm_bo_create` и не
        // копируется. Тип не `Send`/`Sync`, активные mappings не выдаются, а
        // `_device` удерживает GBM device до завершения этого Drop.
        unsafe { gbm_bo_destroy(self.bo) };
    }
}

/// VA-API external descriptor для `LinearGbmVideoFrame`.
pub struct LinearGbmExternalBufferDescriptor {
    /// Fourcc кадра.
    fourcc: Fourcc,
    /// DRM modifier.
    modifier: u64,
    /// Разрешение кадра.
    resolution: Resolution,
    /// Число значимых плоскостей в массивах descriptor-а.
    num_planes: u32,
    /// Проверенные pitch плоскостей; неиспользуемые элементы равны нулю.
    pitches: [u32; 4],
    /// Проверенные offset плоскостей; неиспользуемые элементы равны нулю.
    offsets: [u32; 4],
    /// Размер DMA-BUF object, проверенный до вызова libva.
    object_size: u32,
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
                size: self.object_size,
                drm_format_modifier: self.modifier,
            },
            Default::default(),
            Default::default(),
            Default::default(),
        ];

        let layers = [
            VADRMPRIMESurfaceDescriptorLayer {
                drm_format: u32::from(self.fourcc),
                num_planes: self.num_planes,
                object_index: [0, 0, 0, 0],
                offset: self.offsets,
                pitch: self.pitches,
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
                    // SAFETY: owner-thread/lifetime инварианты сохраняют `bo`;
                    // индекс ограничен двумя плоскостями, подтверждёнными GBM.
                    unsafe {
                        gbm_bo_get_stride_for_plane(self.bo, plane_index as libc::c_int) as usize
                    }
                })
                .collect();
        }

        // SAFETY: owner-thread/lifetime инварианты сохраняют `bo`; plane 0
        // существует у каждого успешно созданного BO, включая R8 fallback.
        let y_stride = unsafe { gbm_bo_get_stride_for_plane(self.bo, 0) as usize };
        vec![y_stride, y_stride]
    }

    fn map<'a>(&'a self) -> Result<Box<dyn ReadMapping<'a> + 'a>, String> {
        Err(
            "Linear GBM VA output is owner-thread zero-copy only; CPU mapping is not exposed"
                .to_string(),
        )
    }

    fn map_mut<'a>(&'a mut self) -> Result<Box<dyn WriteMapping<'a> + 'a>, String> {
        Err("Linear GBM VA output is not CPU-writable through VideoFrame::map_mut()".to_string())
    }

    fn to_native_handle(&self, display: &Rc<Display>) -> Result<Self::NativeHandle, String> {
        if self.decoded_format()? != DecodedFormat::NV12 {
            return Err("Only NV12 VA-API export is supported".to_string());
        }

        let plane_pitches = self.get_plane_pitch();
        let plane_offsets = self.plane_offsets();
        if plane_pitches.len() != plane_offsets.len() {
            return Err(format!(
                "Linear GBM layout mismatch: {} pitches for {} offsets",
                plane_pitches.len(),
                plane_offsets.len()
            ));
        }
        let num_planes = u32::try_from(plane_pitches.len())
            .map_err(|_| "Linear GBM plane count does not fit VA descriptor".to_string())?;
        let descriptor_pitches = checked_descriptor_plane_values(&plane_pitches, "pitch")?;
        let descriptor_offsets = checked_descriptor_plane_values(&plane_offsets, "offset")?;

        // SAFETY: `bo` остаётся валиден благодаря `&self`. Mesa документирует,
        // что каждый `gbm_bo_get_fd` возвращает новый fd, который обязан закрыть
        // caller; отрицательный результат обрабатывается до принятия ownership.
        let exported_fd = unsafe { gbm_bo_get_fd(self.bo) };
        if exported_fd < 0 {
            return Err("Failed to export GBM BO fd".to_string());
        }

        // SAFETY: это новый положительный owned fd из `gbm_bo_get_fd`.
        // До этой строки его никто не закрыл; после неё exactly-once close
        // обеспечивает `File`, включая все дальнейшие error paths.
        let export_file = unsafe { File::from_raw_fd(exported_fd) };
        let object_size = checked_dma_buf_object_size(&export_file)?;

        let descriptor = LinearGbmExternalBufferDescriptor {
            fourcc: self.fourcc,
            modifier: self.modifier(),
            resolution: self.resolution,
            num_planes,
            pitches: descriptor_pitches,
            offsets: descriptor_offsets,
            object_size,
            export_file,
        };

        // `Surface` сохраняет `Rc<Display>` и descriptor внутри себя. Поэтому
        // `vaDestroySurfaces` выполняется до возможного `vaTerminate`, а owned
        // export fd остаётся жив как минимум весь VA surface lifecycle.
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

        surfaces
            .pop()
            .ok_or_else(|| "VA driver returned no imported linear GBM surface".to_string())
    }
}

/// Переводит backend layout в фиксированный VA descriptor без truncation.
fn checked_descriptor_plane_values(
    values: &[usize],
    field_name: &'static str,
) -> Result<[u32; 4], String> {
    if values.len() > 4 {
        return Err(format!(
            "Linear GBM {field_name} count {} exceeds VA descriptor capacity 4",
            values.len()
        ));
    }

    let mut descriptor_values = [0; 4];
    for (index, value) in values.iter().copied().enumerate() {
        descriptor_values[index] = u32::try_from(value).map_err(|_| {
            format!("Linear GBM {field_name}[{index}] does not fit VA descriptor: {value}")
        })?;
    }
    Ok(descriptor_values)
}

/// Читает размер owned DMA-BUF fd и запрещает молчаливое `u64 -> u32` truncation.
fn checked_dma_buf_object_size(export_file: &File) -> Result<u32, String> {
    let object_size = export_file
        .metadata()
        .map_err(|error| format!("Failed to query exported GBM DMA-BUF size: {error}"))?
        .len();

    u32::try_from(object_size)
        .map_err(|_| format!("Exported GBM DMA-BUF size does not fit VA descriptor: {object_size}"))
}

#[cfg(test)]
mod safety_tests {
    use super::*;
    use static_assertions::{assert_impl_all, assert_not_impl_any};

    // Raw GBM owners закреплены за одним decoder/VA owner thread через `Rc`.
    // Ни lifetime refcount, ни immutable Rust reference не сериализуют GBM calls.
    assert_not_impl_any!(LinearGbmDevice: Send, Sync);
    assert_not_impl_any!(LinearGbmVideoFrame: Send, Sync);

    // VA display/surface wrappers тоже остаются на owner thread: `Surface`
    // удерживает `Rc<Display>`, поэтому Drop surface не может пересечься с
    // `vaTerminate` на другом потоке.
    assert_not_impl_any!(Display: Send, Sync);
    assert_not_impl_any!(Surface<LinearGbmExternalBufferDescriptor>: Send, Sync);

    // Descriptor содержит только owned `File` и value metadata. Он может быть
    // передан как значение; это не делает исходный `gbm_bo*` разделяемым.
    assert_impl_all!(LinearGbmExternalBufferDescriptor: Send, Sync);

    #[test]
    fn descriptor_plane_values_reject_more_than_four_planes() {
        let error = checked_descriptor_plane_values(&[0, 1, 2, 3, 4], "offset").unwrap_err();

        assert!(error.contains("exceeds VA descriptor capacity 4"));
    }

    #[test]
    fn descriptor_plane_values_reject_u32_truncation() {
        let overflowing_value = u32::MAX as usize + 1;

        let error = checked_descriptor_plane_values(&[overflowing_value], "pitch").unwrap_err();

        assert!(error.contains("does not fit VA descriptor"));
    }

    #[test]
    fn frame_keeps_owner_device_alive_and_cpu_mapping_fails_closed() {
        let Ok(device) = LinearGbmDevice::open_default() else {
            eprintln!("Skipping test: GBM render node is unavailable");
            return;
        };
        let weak_device = Rc::downgrade(&device);
        let Ok(mut frame) = device.new_nv12_frame(Resolution {
            width: 64,
            height: 64,
        }) else {
            eprintln!("Skipping test: linear GBM allocation is unsupported");
            return;
        };

        let read_error = frame
            .map()
            .err()
            .expect("CPU read mapping must fail closed");
        assert!(read_error.contains("zero-copy only"));
        let write_error = frame
            .map_mut()
            .err()
            .expect("CPU write mapping must fail closed");
        assert!(write_error.contains("not CPU-writable"));

        drop(device);
        assert!(weak_device.upgrade().is_some());

        drop(frame);
        assert!(weak_device.upgrade().is_none());
    }
}
