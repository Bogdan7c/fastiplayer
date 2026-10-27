//! Программный планарный кадр (8, 10 или 12 бит) в DMA-BUF и его паспорт (чистая логика, без GL).
//!
//! Откуда берётся такой кадр. Блок памяти декодера FFmpeg — запечатанный `memfd`, который ядро
//! оборачивает в dma-buf (`/dev/udmabuf`); три плоскости Y, U, V лежат в одном буфере (по
//! смещениям и шагам строк, которые выбрал декодер), и каждая становится своей текстурой
//! `DRM_FORMAT_R8` (8 бит) или `DRM_FORMAT_GR88` (10/12 бит: пара байтов LE на отсчёт, сессия
//! 12a) + `DRM_FORMAT_MOD_LINEAR`. Видеослой рисует их ТОЙ ЖЕ программой, что и кадр после
//! загрузки (`upload`); у 10/12 бит отличается только канал пары байтов (`.rg` вместо `.ra`),
//! поэтому картинка совпадает с копирующим путём.
//!
//! Паспорт [`DmaBufPlanarIdentity`] — ключ кэша импортов. В отличие от паспорта NV12
//! ([`crate::DmaBufBufferIdentity`]) смещения и шаги строк входят в паспорт: у программного
//! блока их выбирает не экспорт драйвера, а раскладка кадра FFmpeg, и два кадра одного блока с
//! разной раскладкой (смена формата посреди потока) обязаны получить разные записи кэша.
//!
//! Контракт синхронизации с производителем — тот же, что у NV12:
//! [`crate::DmaBufSharingContract`]. Для программного кадра к нему добавляется кэш CPU: на
//! GPU без общего с CPU кэша производитель обязан сбросить записанные строки
//! ([`crate::probe_cpu_write_visibility`] выясняет, нужен ли сброс).

use std::os::fd::BorrowedFd;

use thiserror::Error;

use gles2_egui::DrmModifier;

use crate::dma_buf_frame::DmaBufObjectIdentity;
use crate::planes::{PlanarLayout, PlaneExtent, PlaneRole, SampleDepth, VideoFrameKey};

/// Плоскости кадра в порядке Y, U, V (индексы массивов паспорта).
pub(crate) const PLANAR_ROLES: [PlaneRole; 3] =
    [PlaneRole::Luma, PlaneRole::ChromaU, PlaneRole::ChromaV];

/// Где лежит плоскость внутри объекта ядра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmaBufPlaneLocation {
    /// Смещение первой строки от начала объекта, байт.
    pub offset: u32,
    /// Шаг строк, байт.
    pub pitch: u32,
}

/// Паспорт планарного буфера: равенство означает «тот же объект ядра в той же раскладке».
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DmaBufPlanarIdentity {
    /// Номер ресурса у производителя (у программного декодера — номер блока пула). Производитель
    /// может пересоздать блок под тем же номером: тогда `object` отличается, а номер совпадает.
    pub resource_id: u64,
    /// Объект ядра, в котором лежат все три плоскости.
    pub object: DmaBufObjectIdentity,
    /// Прореживание цветности и разрядность (импортируется только `Bits8`).
    pub layout: PlanarLayout,
    /// Видимый размер яркости, отсчётов.
    pub luma_extent: PlaneExtent,
    /// Расположение плоскостей в порядке Y, U, V.
    pub planes: [DmaBufPlaneLocation; 3],
}

impl DmaBufPlanarIdentity {
    /// Видимый размер плоскости `role`: цветность выводится из раскладки.
    #[must_use]
    pub const fn plane_extent(&self, role: PlaneRole) -> PlaneExtent {
        match role {
            PlaneRole::Luma => self.luma_extent,
            PlaneRole::ChromaU | PlaneRole::ChromaV => {
                self.layout.chroma.chroma_extent(self.luma_extent)
            }
        }
    }

    /// Расположение плоскости `role` в объекте.
    #[must_use]
    pub const fn plane_location(&self, role: PlaneRole) -> DmaBufPlaneLocation {
        match role {
            PlaneRole::Luma => self.planes[0],
            PlaneRole::ChromaU => self.planes[1],
            PlaneRole::ChromaV => self.planes[2],
        }
    }
}

/// Планарный кадр в DMA-BUF противоречит сам себе или не годится для импорта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DmaBufPlanarFrameError {
    /// Нулевая ширина или высота яркости.
    #[error("planar DMA-BUF frame has an empty luma extent {extent:?}")]
    EmptyExtent {
        /// Размер яркости.
        extent: PlaneExtent,
    },
    /// Объект не линейный: только `DRM_FORMAT_MOD_LINEAR` (память `udmabuf` всегда линейная).
    #[error("planar DMA-BUF object has modifier {modifier:#x}, only LINEAR (0) is supported")]
    UnsupportedModifier {
        /// Модификатор объекта.
        modifier: u64,
    },
    /// Строка плоскости короче видимой части.
    #[error("{plane:?} plane pitch {pitch} is shorter than its {row_bytes}-byte visible row")]
    PitchShorterThanRow {
        /// Плоскость.
        plane: PlaneRole,
        /// Шаг строк.
        pitch: u32,
        /// Видимых байт в строке.
        row_bytes: u32,
    },
    /// Смещение или шаг не помещается в `EGLint` (знаковые 32 бита).
    #[error(
        "{plane:?} plane offset {offset} or pitch {pitch} does not fit into a signed 32-bit EGL attribute"
    )]
    LocationOutOfRange {
        /// Плоскость.
        plane: PlaneRole,
        /// Смещение.
        offset: u32,
        /// Шаг строк.
        pitch: u32,
    },
    /// Последняя видимая строка плоскости выходит за объект: импорт прочитал бы чужую память.
    #[error("{plane:?} plane ends at byte {end}, but the object holds {object_size} bytes")]
    PlaneOutsideObject {
        /// Плоскость.
        plane: PlaneRole,
        /// Конец плоскости (не включая), байт.
        end: u64,
        /// Размер объекта, байт.
        object_size: u64,
    },
}

/// Планарный кадр 8 бит в DMA-BUF для импорта: проверен до драйвера.
///
/// Один fd на весь буфер (все плоскости — смещения в нём). Кадр только заимствует дескриптор;
/// закрыть его можно сразу после импорта, а освобождение блока декодеру — дело вызывающего
/// ([`crate::DmaBufSharingContract`]).
#[derive(Debug, Clone, Copy)]
pub struct DmaBufPlanarFrame<'fd> {
    key: VideoFrameKey,
    identity: &'fd DmaBufPlanarIdentity,
    dma_buf: BorrowedFd<'fd>,
}

impl<'fd> DmaBufPlanarFrame<'fd> {
    /// Собрать кадр, проверив всё, что можно проверить до драйвера.
    ///
    /// Сходство паспорта с настоящим объектом (`fstat` по fd) проверяет адаптер: здесь, как и у
    /// NV12, по `BorrowedFd` этого не узнать.
    ///
    /// # Errors
    /// [`DmaBufPlanarFrameError`]: пустой размер, нелинейный модификатор, шаг короче строки,
    /// смещение или шаг шире `EGLint`, плоскость за пределами объекта.
    pub fn new(
        key: VideoFrameKey,
        identity: &'fd DmaBufPlanarIdentity,
        dma_buf: BorrowedFd<'fd>,
    ) -> Result<Self, DmaBufPlanarFrameError> {
        if identity.luma_extent.is_empty() {
            return Err(DmaBufPlanarFrameError::EmptyExtent {
                extent: identity.luma_extent,
            });
        }
        if identity.object.modifier != DrmModifier::LINEAR.0 {
            return Err(DmaBufPlanarFrameError::UnsupportedModifier {
                modifier: identity.object.modifier,
            });
        }
        for role in PLANAR_ROLES {
            validate_plane(identity, role)?;
        }
        Ok(Self {
            key,
            identity,
            dma_buf,
        })
    }

    /// Ключ кадра у вызывающего.
    #[must_use]
    pub const fn key(&self) -> VideoFrameKey {
        self.key
    }

    /// Паспорт буфера.
    #[must_use]
    pub const fn identity(&self) -> &'fd DmaBufPlanarIdentity {
        self.identity
    }

    /// Дескриптор dma-buf (заимствован).
    #[must_use]
    pub const fn dma_buf(&self) -> BorrowedFd<'fd> {
        self.dma_buf
    }
}

/// Проверить одну плоскость паспорта: ширину строки, диапазон атрибутов EGL, границы объекта.
fn validate_plane(
    identity: &DmaBufPlanarIdentity,
    role: PlaneRole,
) -> Result<(), DmaBufPlanarFrameError> {
    let extent = identity.plane_extent(role);
    let location = identity.plane_location(role);
    // `R8` — один байт на отсчёт, `GR88` (10/12 бит) — два: видимая строка = ширина × байты.
    // Ширина плоскости — тысячи текселей, умножение на 2 в `u32` не переполняется.
    let row_bytes = extent
        .width
        .saturating_mul(bytes_per_sample(identity.layout.depth));
    if location.pitch < row_bytes {
        return Err(DmaBufPlanarFrameError::PitchShorterThanRow {
            plane: role,
            pitch: location.pitch,
            row_bytes,
        });
    }
    if i32::try_from(location.offset).is_err() || i32::try_from(location.pitch).is_err() {
        return Err(DmaBufPlanarFrameError::LocationOutOfRange {
            plane: role,
            offset: location.offset,
            pitch: location.pitch,
        });
    }
    // Последняя строка занимает только видимые байты, хвост шага за ней не читается. Сумма
    // `u32 + u32 × u32 + u32` в `u64` не переполняется.
    let end = u64::from(location.offset)
        + u64::from(location.pitch) * u64::from(extent.height.saturating_sub(1))
        + u64::from(row_bytes);
    if end > identity.object.size {
        return Err(DmaBufPlanarFrameError::PlaneOutsideObject {
            plane: role,
            end,
            object_size: identity.object.size,
        });
    }
    Ok(())
}

/// Байт на отсчёт плоскости в DMA-BUF.
const fn bytes_per_sample(depth: SampleDepth) -> u32 {
    match depth {
        SampleDepth::Bits8 => 1,
        SampleDepth::Bits10 | SampleDepth::Bits12 => 2,
    }
}

#[cfg(test)]
mod tests;
