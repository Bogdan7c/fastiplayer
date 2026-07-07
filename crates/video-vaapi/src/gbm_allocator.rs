use std::{
    error::Error,
    fmt,
    fs::{File, OpenOptions},
};

use cros_codecs::PlaneLayout;
use gbm::{BufferObjectFlags, Device, Format, Modifier};

/// Путь к DRI render node — доступен всем пользователям (обычно 666).
const RENDER_DRI_PATH: &str = "/dev/dri/renderD128";

/// Флаг GBM для hardware video decode.
///
/// Не экспортирован gbm crate, но присутствует в заголовках libgbm.
const GBM_BO_USE_HW_VIDEO_DECODER: u32 = 1 << 13;

#[derive(Debug)]
struct NonLinearGbmBufferError {
    modifier: Modifier,
}

impl fmt::Display for NonLinearGbmBufferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "GBM returned non-linear buffer {:?}; CPU texture upload requires linear DMA-BUF",
            self.modifier
        )
    }
}

impl Error for NonLinearGbmBufferError {}

/// Выделяет DMA-BUF через GBM (Generic Buffer Management).
///
/// GBM использует `/dev/dri/renderD128` — доступен без root на большинстве
/// Linux-систем (права 666 или группа video/render).
///
/// # Аргументы
/// * `width` — ширина кадра в пикселях (coded resolution).
/// * `height` — высота кадра в пикселях (coded resolution).
///
/// # Возвращаемое значение
/// Результат выделения GBM DMA-BUF под один NV12 кадр.
pub struct GbmAllocatedBuffer {
    /// DMA-BUF fd, который будет импортирован в VA-API.
    pub file: File,
    /// Общий размер DMA-BUF в байтах.
    pub total_size: usize,
    /// DRM modifier, фактически выбранный драйвером.
    pub modifier: Modifier,
    /// Layout плоскостей из GBM: offset и stride для Y/UV.
    pub plane_layouts: Vec<PlaneLayout>,
}
/// `GbmAllocatedBuffer` — DMA-BUF fd, размер, modifier и layout плоскостей.
///
/// # Ошибки
/// Возвращает ошибку если:
/// - `/dev/dri/renderD128` недоступен
/// - GBM device не создался
/// - NV12 buffer object не создался
/// - драйвер вернул non-linear modifier для CPU texture upload пути
pub fn allocate_gbm_buffer(width: u32, height: u32) -> anyhow::Result<GbmAllocatedBuffer> {
    // Открываем DRI render node на чтение и запись.
    // renderD128 не требует root, в отличие от dma-heap.
    let dri_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(RENDER_DRI_PATH)
        .map_err(|e| anyhow::anyhow!("Failed to open {}: {}", RENDER_DRI_PATH, e))?;

    // Создаём GBM device из DRI fd.
    let gbm_device =
        Device::new(dri_file).map_err(|e| anyhow::anyhow!("Failed to create GBM device: {}", e))?;

    // Флаги для VA-API/GBM allocation path.
    // HW_VIDEO_DECODER нужен VA-API decoder'у, LINEAR сохраняет предсказуемый
    // layout exported buffer-а без протаскивания renderer import API в этот crate.
    let usage_flags = BufferObjectFlags::from_bits_truncate(GBM_BO_USE_HW_VIDEO_DECODER)
        | BufferObjectFlags::LINEAR;

    // Пробуем создать BO с форматом NV12.
    let bo = match gbm_device.create_buffer_object::<()>(width, height, Format::Nv12, usage_flags) {
        Ok(bo) => bo,
        Err(e) => {
            // Fallback: если NV12 не поддерживается GBM, создаём R8 буфер
            // достаточного размера для размещения NV12 данных.
            // NV12 = Y (width * stride) + UV (width * stride / 2).
            let fake_width = width.max(1);
            let fake_height = height * 2; // достаточно для Y + UV
            gbm_device
                .create_buffer_object::<()>(fake_width, fake_height, Format::R8, usage_flags)
                .map_err(|e2| {
                    anyhow::anyhow!(
                        "Failed to create GBM buffer (NV12: {}, R8 fallback: {})",
                        e,
                        e2
                    )
                })?
        }
    };

    // Получаем stride и размер буфера.
    // Методы возвращают Result, unwrap безопасен т.к. BO только что создан.
    let stride = bo
        .stride()
        .map_err(|e| anyhow::anyhow!("GBM bo.stride() failed: {}", e))? as usize;
    let bo_width = bo
        .width()
        .map_err(|e| anyhow::anyhow!("GBM bo.width() failed: {}", e))? as usize;
    let bo_height =
        bo.height()
            .map_err(|e| anyhow::anyhow!("GBM bo.height() failed: {}", e))? as usize;
    let total_size = stride * bo_height;
    let modifier = bo
        .modifier()
        .map_err(|e| anyhow::anyhow!("GBM bo.modifier() failed: {}", e))?;
    if modifier != Modifier::Linear {
        return Err(NonLinearGbmBufferError { modifier }.into());
    }
    let plane_layouts = read_nv12_plane_layouts(&bo, stride, height as usize)?;

    // Получаем DMA-BUF fd из BO.
    // `bo.fd()` создаёт новый fd (dup) — BO можно дропнуть после этого.
    let owned_fd = bo
        .fd()
        .map_err(|e| anyhow::anyhow!("Failed to get DMA-BUF fd from GBM buffer: {}", e))?;

    tracing::info!(
        width = width,
        height = height,
        bo_width = bo_width,
        bo_height = bo_height,
        stride = stride,
        total_size = total_size,
        modifier = ?modifier,
        plane_layouts = ?plane_layouts,
        "GBM buffer allocated"
    );

    // Конвертируем OwnedFd в File.
    let file = File::from(owned_fd);

    tracing::debug!(
        width = width,
        height = height,
        bo_width = bo_width,
        bo_height = bo_height,
        stride = stride,
        total_size = total_size,
        "GBM buffer allocated"
    );

    Ok(GbmAllocatedBuffer {
        file,
        total_size,
        modifier,
        plane_layouts,
    })
}

/// Читает layout плоскостей NV12 из GBM.
///
/// Если драйвер отдаёт настоящий multi-plane BO, используем `offset(plane)`
/// и `stride_for_plane(plane)`. Если GBM вернул fallback single-plane BO,
/// строим NV12 layout вручную поверх одного contiguous буфера.
fn read_nv12_plane_layouts<T>(
    bo: &gbm::BufferObject<T>,
    fallback_stride: usize,
    frame_height: usize,
) -> anyhow::Result<Vec<PlaneLayout>> {
    let plane_count = bo
        .plane_count()
        .map_err(|e| anyhow::anyhow!("GBM bo.plane_count() failed: {}", e))?;

    if plane_count >= 2 {
        let y_offset = bo
            .offset(0)
            .map_err(|e| anyhow::anyhow!("GBM bo.offset(0) failed: {}", e))?
            as usize;
        let y_stride = bo
            .stride_for_plane(0)
            .map_err(|e| anyhow::anyhow!("GBM bo.stride_for_plane(0) failed: {}", e))?
            as usize;
        let uv_offset = bo
            .offset(1)
            .map_err(|e| anyhow::anyhow!("GBM bo.offset(1) failed: {}", e))?
            as usize;
        let uv_stride = bo
            .stride_for_plane(1)
            .map_err(|e| anyhow::anyhow!("GBM bo.stride_for_plane(1) failed: {}", e))?
            as usize;

        return Ok(vec![
            PlaneLayout {
                buffer_index: 0,
                offset: y_offset,
                stride: y_stride,
            },
            PlaneLayout {
                buffer_index: 0,
                offset: uv_offset,
                stride: uv_stride,
            },
        ]);
    }

    Ok(vec![
        PlaneLayout {
            buffer_index: 0,
            offset: 0,
            stride: fallback_stride,
        },
        PlaneLayout {
            buffer_index: 0,
            offset: fallback_stride * frame_height,
            stride: fallback_stride,
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn gbm_available() -> bool {
        Path::new(RENDER_DRI_PATH).exists()
    }

    #[test]
    fn test_allocate_gbm_buffer() {
        if !gbm_available() {
            eprintln!("Skipping test: {} not available", RENDER_DRI_PATH);
            return;
        }
        let allocated_buffer = match allocate_gbm_buffer(1920, 1080) {
            Ok(allocated_buffer) => allocated_buffer,
            Err(error) if error.downcast_ref::<NonLinearGbmBufferError>().is_some() => {
                eprintln!("Skipping test: {error:#}");
                return;
            }
            Err(error) => panic!("GBM alloc failed: {error:#}"),
        };
        let metadata = allocated_buffer.file.metadata().expect("metadata failed");
        assert!(
            metadata.len() as usize >= allocated_buffer.total_size,
            "buffer too small"
        );
        assert!(
            allocated_buffer.plane_layouts[0].stride >= 1920,
            "Y stride too small"
        );
        assert_eq!(allocated_buffer.plane_layouts.len(), 2);
    }
}
