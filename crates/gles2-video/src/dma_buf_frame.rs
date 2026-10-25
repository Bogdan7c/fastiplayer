//! Аппаратный кадр NV12 в DMA-BUF и «паспорт буфера» (чистая логика, без GL).
//!
//! Словарь нарочно не зависит от `video-core` плеера (как и весь crate): паспорт буфера заполняет
//! адаптер из дескриптора декодера (у VA-API `resource_id` — номер поверхности `VASurfaceID`,
//! `device/inode/size/modifier` — из `fstat` и описания объекта экспорта).
//!
//! Зачем паспорт. Пул поверхностей декодера конечен и переиспользуется по кругу, поэтому один и
//! тот же kernel-буфер приходит снова и снова. Импорт (`EGLImage` + текстуры) стоит дорого, а
//! текстура ссылается на память буфера (не копирует её), поэтому один импорт живёт, пока жив
//! буфер, и показывает все кадры, которые декодер в него положил. Паспорт — ключ этого
//! переиспользования: равные паспорта означают «тот же буфер ядра в той же раскладке».
//!
//! Контракт синхронизации (кто, что и когда) вынесен в [`DmaBufSharingContract`].

use thiserror::Error;

use gles2_egui::DrmFourcc;

use crate::planes::{ChromaSubsampling, DmaBufPlane, PlaneExtent, VideoFrameKey};

/// Контракт совместного использования буфера производителем (декодером) и видеослоем.
///
/// Тип без данных: существует, чтобы контракт был явным именованным понятием, на которое
/// ссылаются документация и тесты (по образцу `ZeroCopyImportReuseContract` плеера).
///
/// 1. **Производитель завершает запись до выдачи кадра.** У VA-API это `vaSyncSurface` перед
///    экспортом дескриптора: видеослой не ждёт декодер и не проверяет готовность буфера.
/// 2. **GL читает память при рисовании, а не при импорте.** Текстура на `EGLImage` — ссылка на
///    буфер, а не копия (`EGL_EXT_image_dma_buf_import`; привязка к текстуре — `GL_OES_EGL_image`,
///    `glEGLImageTargetTexture2DOES`). Поэтому повторять импорт на каждом кадре не нужно, а
///    содержимое, записанное производителем в тот же буфер, видно при следующем рисовании.
///    Сами по себе `EGLImage` и привязка не делают синхронизации: её обеспечивает пункт 3.
/// 3. **Производитель не пишет в буфер, пока не сработал fence последнего кадра показа, читавшего
///    его.** Видеослой кадр декодеру НЕ возвращает: это делает вызывающий (в плеере — поток-
///    ожидатель по fence кадра показа, `retire_submitted_work` получает тот же fence).
/// 4. **Закрыть свой fd после импорта можно сразу**: EGL fd не забирает, а текстура держит
///    буфер сама. Но закрытый fd не освобождает кадр декодеру — это пункт 3.
#[derive(Debug, Clone, Copy)]
pub struct DmaBufSharingContract;

/// Какая плоскость NV12 имеется в виду в ошибках.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DmaBufPlaneRole {
    /// Яркость Y (`DRM_FORMAT_R8`).
    Luma,
    /// Цветность UV вперемешку (`DRM_FORMAT_GR88`).
    Chroma,
}

/// Объект ядра, из которого сделан буфер (по `fstat` его dma-buf fd и описанию экспорта).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmaBufObjectIdentity {
    /// Устройство inode (`st_dev`).
    pub device: u64,
    /// Номер inode (`st_ino`): у разных буферов ядра разный.
    pub inode: u64,
    /// Размер объекта, байт.
    pub size: u64,
    /// DRM-модификатор объекта.
    pub modifier: u64,
}

/// Как плоскости NV12 разложены по объектам ядра (понятие DRM PRIME).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DmaBufPlaneArrangement {
    /// Обе плоскости — один слой одного объекта (у VA-API «composed layers»).
    ComposedSingleObject,
    /// Плоскости — отдельные слои (у VA-API «separate layers»), обычно разные объекты.
    SeparatePlanes,
}

/// Сигнатура формата потока: только то, что известно из паспорта без дескрипторов.
///
/// Смена сигнатуры (другой размер, модификатор, раскладка) означает, что весь старый набор
/// буферов декодера выброшен: кэш импортов уничтожает все старые записи до импорта первой новой.
///
/// Почему здесь нет смещений и шагов строк. Паспорт строит адаптер из идентичности кадра
/// без дескрипторов (чтобы промах по кэшу был единственным моментом, когда нужен dup fd). Смещения
/// и шаги — свойство экспорта буфера: тот же `resource_id` и те же объекты (`device/inode/size/
/// modifier`) при той же раскладке и размере дают те же смещения и шаги. Поэтому в ключ кэша они
/// не входят.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmaBufFrameFormat {
    /// Видимый размер яркости, отсчётов (размер цветности — `ceil(яркость / 2)`).
    pub luma_extent: PlaneExtent,
    /// Раскладка плоскостей по объектам.
    pub arrangement: DmaBufPlaneArrangement,
    /// Общий DRM-модификатор объектов.
    pub modifier: u64,
}

/// Паспорт буфера: равенство означает «тот же буфер ядра в той же раскладке».
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DmaBufBufferIdentity {
    /// Номер ресурса у производителя (у VA-API — `VASurfaceID`). Производитель может пересоздать
    /// буфер под тем же номером: тогда `objects` отличаются, а номер совпадает.
    pub resource_id: u64,
    /// Объекты ядра, из которых состоит кадр (composed — один, separate — два).
    pub objects: Vec<DmaBufObjectIdentity>,
    /// Сигнатура формата.
    pub format: DmaBufFrameFormat,
}

/// Кадр NV12 в DMA-BUF для импорта: проверен до драйвера.
///
/// Владение fd: кадр только заимствует дескрипторы ([`DmaBufPlane::fd`]). Закрыть их можно сразу
/// после импорта (см. [`DmaBufSharingContract`]); освобождение кадра декодеру — дело вызывающего.
#[derive(Debug, Clone, Copy)]
pub struct DmaBufNv12Frame<'a> {
    key: VideoFrameKey,
    buffer: &'a DmaBufBufferIdentity,
    luma_extent: PlaneExtent,
    luma: DmaBufPlane<'a>,
    chroma: DmaBufPlane<'a>,
    /// Смещения плоскостей, уже проверенные на `u32` (так их принимает импорт).
    luma_offset: u32,
    chroma_offset: u32,
}

/// Кадр NV12 в DMA-BUF противоречит сам себе или формату NV12.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DmaBufFrameError {
    /// Нулевая ширина или высота яркости.
    #[error("DMA-BUF frame has an empty luma extent {extent:?}")]
    EmptyExtent {
        /// Размер яркости.
        extent: PlaneExtent,
    },
    /// Формат плоскости не тот, что у NV12.
    #[error("{plane:?} plane has DRM fourcc {actual:#010x}, NV12 needs {expected:#010x}")]
    WrongPlaneFormat {
        /// Плоскость.
        plane: DmaBufPlaneRole,
        /// Нужный fourcc.
        expected: u32,
        /// Полученный fourcc.
        actual: u32,
    },
    /// Строка плоскости короче видимой части.
    #[error("{plane:?} plane pitch {pitch} is shorter than its {row_bytes}-byte visible row")]
    PitchShorterThanRow {
        /// Плоскость.
        plane: DmaBufPlaneRole,
        /// Шаг строк.
        pitch: u32,
        /// Видимых байт в строке.
        row_bytes: u64,
    },
    /// Смещение плоскости не помещается в `u32` (так его принимает импорт).
    #[error("{plane:?} plane offset {offset} does not fit into 32 bits")]
    OffsetTooLarge {
        /// Плоскость.
        plane: DmaBufPlaneRole,
        /// Смещение.
        offset: u64,
    },
    /// Размер яркости в паспорте не равен размеру кадра: кэш по такому паспорту отдал бы чужую
    /// текстуру.
    #[error("buffer identity says luma extent {passport:?}, the frame has {frame:?}")]
    LumaExtentDiffersFromIdentity {
        /// Размер из паспорта.
        passport: PlaneExtent,
        /// Размер кадра.
        frame: PlaneExtent,
    },
    /// Модификатор плоскости не равен модификатору паспорта.
    #[error("{plane:?} plane modifier {actual:#x} differs from identity modifier {passport:#x}")]
    PlaneModifierDiffersFromIdentity {
        /// Плоскость.
        plane: DmaBufPlaneRole,
        /// Модификатор паспорта.
        passport: u64,
        /// Модификатор плоскости.
        actual: u64,
    },
}

impl<'a> DmaBufNv12Frame<'a> {
    /// Собрать кадр, проверив всё, что можно проверить до драйвера.
    ///
    /// Расклад `arrangement` берётся из паспорта: по двум `BorrowedFd` надёжно не отличить
    /// composed от separate, и что для `ComposedSingleObject` обе плоскости смотрят в один объект,
    /// сверяет адаптер (без `fstat` здесь это не проверить).
    ///
    /// # Errors
    /// [`DmaBufFrameError`]: пустой размер, не тот формат плоскости (яркость — `R8`, цветность —
    /// `GR88`), шаг короче строки, смещение шире `u32`, размер или модификатор плоскостей не
    /// совпадают с паспортом.
    pub fn new(
        key: VideoFrameKey,
        buffer: &'a DmaBufBufferIdentity,
        luma_extent: PlaneExtent,
        luma: DmaBufPlane<'a>,
        chroma: DmaBufPlane<'a>,
    ) -> Result<Self, DmaBufFrameError> {
        if luma_extent.is_empty() {
            return Err(DmaBufFrameError::EmptyExtent {
                extent: luma_extent,
            });
        }
        let chroma_extent = ChromaSubsampling::Yuv420.chroma_extent(luma_extent);
        let luma_offset = validate_plane(
            DmaBufPlaneRole::Luma,
            &luma,
            luma_extent,
            LUMA_BYTES_PER_TEXEL,
        )?;
        let chroma_offset = validate_plane(
            DmaBufPlaneRole::Chroma,
            &chroma,
            chroma_extent,
            CHROMA_BYTES_PER_TEXEL,
        )?;
        if buffer.format.luma_extent != luma_extent {
            return Err(DmaBufFrameError::LumaExtentDiffersFromIdentity {
                passport: buffer.format.luma_extent,
                frame: luma_extent,
            });
        }
        for (role, plane) in [
            (DmaBufPlaneRole::Luma, &luma),
            (DmaBufPlaneRole::Chroma, &chroma),
        ] {
            if plane.modifier != buffer.format.modifier {
                return Err(DmaBufFrameError::PlaneModifierDiffersFromIdentity {
                    plane: role,
                    passport: buffer.format.modifier,
                    actual: plane.modifier,
                });
            }
        }
        Ok(Self {
            key,
            buffer,
            luma_extent,
            luma,
            chroma,
            luma_offset,
            chroma_offset,
        })
    }

    /// Ключ кадра у вызывающего.
    #[must_use]
    pub const fn key(&self) -> VideoFrameKey {
        self.key
    }

    /// Паспорт буфера.
    #[must_use]
    pub const fn buffer(&self) -> &'a DmaBufBufferIdentity {
        self.buffer
    }

    /// Видимый размер яркости.
    #[must_use]
    pub const fn luma_extent(&self) -> PlaneExtent {
        self.luma_extent
    }

    /// Размер плоскости цветности: `ceil(яркость / 2)` по обеим осям (4:2:0).
    #[must_use]
    pub const fn chroma_extent(&self) -> PlaneExtent {
        ChromaSubsampling::Yuv420.chroma_extent(self.luma_extent)
    }

    /// Плоскость яркости.
    #[must_use]
    pub const fn luma(&self) -> &DmaBufPlane<'a> {
        &self.luma
    }

    /// Смещение плоскости яркости в буфере, байт.
    #[must_use]
    pub const fn luma_offset(&self) -> u32 {
        self.luma_offset
    }

    /// Смещение плоскости цветности в буфере, байт.
    #[must_use]
    pub const fn chroma_offset(&self) -> u32 {
        self.chroma_offset
    }

    /// Плоскость цветности.
    #[must_use]
    pub const fn chroma(&self) -> &DmaBufPlane<'a> {
        &self.chroma
    }
}

/// Байт на тексель `DRM_FORMAT_R8`.
const LUMA_BYTES_PER_TEXEL: u64 = 1;
/// Байт на тексель `DRM_FORMAT_GR88`.
const CHROMA_BYTES_PER_TEXEL: u64 = 2;

/// Проверить одну плоскость: формат, ширину строки, смещение.
fn validate_plane(
    role: DmaBufPlaneRole,
    plane: &DmaBufPlane<'_>,
    extent: PlaneExtent,
    bytes_per_texel: u64,
) -> Result<u32, DmaBufFrameError> {
    let expected = match role {
        DmaBufPlaneRole::Luma => DrmFourcc::R8.0,
        DmaBufPlaneRole::Chroma => DrmFourcc::GR88.0,
    };
    if plane.drm_format != expected {
        return Err(DmaBufFrameError::WrongPlaneFormat {
            plane: role,
            expected,
            actual: plane.drm_format,
        });
    }
    let row_bytes = u64::from(extent.width) * bytes_per_texel;
    if u64::from(plane.pitch) < row_bytes {
        return Err(DmaBufFrameError::PitchShorterThanRow {
            plane: role,
            pitch: plane.pitch,
            row_bytes,
        });
    }
    u32::try_from(plane.offset).map_err(|_| DmaBufFrameError::OffsetTooLarge {
        plane: role,
        offset: plane.offset,
    })
}

#[cfg(test)]
mod tests;
