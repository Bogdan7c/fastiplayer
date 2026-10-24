//! Свой словарь кадра видеослоя: раскладка плоскостей, их размеры и откуда брать байты.
//!
//! Словарь нарочно не зависит от `video-core` плеера: crate подключается к любому приложению.
//! Перевод дескрипторов декодера плеера в эти типы — работа адаптера (`render-gles2-shell`).
//!
//! Главный инвариант: всё, что можно проверить до драйвера, проверяется здесь — размеры
//! плоскостей против раскладки, длина байтового среза против stride и числа строк. Драйвер не
//! получает аргументов, с которыми он прочитал бы за концом чужого буфера.

use std::os::fd::BorrowedFd;

use thiserror::Error;

/// Как цветность (U и V) прорежена относительно яркости.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromaSubsampling {
    /// U/V вдвое уже и вдвое ниже яркости.
    Yuv420,
    /// U/V вдвое уже яркости, высота та же.
    Yuv422,
    /// U/V того же размера, что яркость.
    Yuv444,
}

impl ChromaSubsampling {
    /// Размер плоскости цветности для плоскости яркости `luma`.
    ///
    /// Нечётная сторона округляется вверх (`ceil(n / 2)`): так раскладывают кадр FFmpeg и
    /// проверка `video-core` плеера, так же считает шейдер wgpu.
    #[must_use]
    pub const fn chroma_extent(self, luma: PlaneExtent) -> PlaneExtent {
        match self {
            Self::Yuv420 => PlaneExtent {
                width: luma.width.div_ceil(2),
                height: luma.height.div_ceil(2),
            },
            Self::Yuv422 => PlaneExtent {
                width: luma.width.div_ceil(2),
                height: luma.height,
            },
            Self::Yuv444 => luma,
        }
    }
}

/// Разрядность отсчёта плоскости.
///
/// В сессии 05 — только 8 бит. 10/12 бит (пары байтов, сборка в шейдере) добавляет сессия 06
/// новым вариантом, не меняя остальной словарь.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleDepth {
    /// Один байт на отсчёт, значение `0..=255`.
    Bits8,
}

impl SampleDepth {
    /// Байт на отсчёт.
    #[must_use]
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::Bits8 => 1,
        }
    }
}

/// Раскладка планарного кадра: три плоскости Y, U, V.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlanarLayout {
    /// Прореживание цветности.
    pub chroma: ChromaSubsampling,
    /// Разрядность отсчётов (одинаковая у всех плоскостей).
    pub depth: SampleDepth,
}

/// Размер плоскости в отсчётах (не в байтах).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlaneExtent {
    /// Отсчётов в строке.
    pub width: u32,
    /// Строк.
    pub height: u32,
}

impl PlaneExtent {
    /// Есть ли что рисовать.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Роль плоскости в кадре.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaneRole {
    /// Яркость Y.
    Luma,
    /// Цветность U (Cb).
    ChromaU,
    /// Цветность V (Cr).
    ChromaV,
}

/// Строки плоскости в памяти процесса: драйвер копирует их в текстуру (одна копия).
///
/// Срез берётся только на время вызова загрузки: по спецификации OpenGL ES 2.0 (§2.1, «data
/// binding occurs on call») драйвер забирает пиксели клиентской памяти до возврата из
/// `glTexSubImage2D`. Когда вызывающий вернёт кадр декодеру — решает он (в плеере: только после
/// fence кадра, решение владельца сессии 05).
#[derive(Debug, Clone, Copy)]
pub struct HostRows<'a> {
    bytes: &'a [u8],
    stride: usize,
    visible_row_bytes: usize,
    rows: u32,
}

impl<'a> HostRows<'a> {
    /// Строки плоскости: `rows` строк по `visible_row_bytes` байт, начало соседних строк — через
    /// `stride` байт.
    ///
    /// Срез обязан покрывать все видимые байты: `(rows − 1) × stride + visible_row_bytes`.
    /// Хвост последней строки (байты после видимой части) не требуется — так отдаёт плоскость
    /// дескриптор кадра FFmpeg (`visible_plane_block`).
    ///
    /// # Errors
    /// [`PlaneSourceError`], если stride короче видимой строки, строк нет или срез короче
    /// видимых байтов.
    pub fn new(
        bytes: &'a [u8],
        stride: usize,
        visible_row_bytes: usize,
        rows: u32,
    ) -> Result<Self, PlaneSourceError> {
        if visible_row_bytes == 0 || rows == 0 {
            return Err(PlaneSourceError::EmptyPlane);
        }
        if stride < visible_row_bytes {
            return Err(PlaneSourceError::StrideShorterThanRow {
                stride,
                visible_row_bytes,
            });
        }
        let required = required_host_bytes(stride, visible_row_bytes, rows)
            .ok_or(PlaneSourceError::SizeOverflow)?;
        if bytes.len() < required {
            return Err(PlaneSourceError::TruncatedRows {
                required,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            bytes,
            stride,
            visible_row_bytes,
            rows,
        })
    }

    /// Все байты плоскости (может быть длиннее видимой части).
    #[must_use]
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Шаг между началами строк в байтах.
    #[must_use]
    pub const fn stride(&self) -> usize {
        self.stride
    }

    /// Видимых байт в строке.
    #[must_use]
    pub const fn visible_row_bytes(&self) -> usize {
        self.visible_row_bytes
    }

    /// Число строк.
    #[must_use]
    pub const fn rows(&self) -> u32 {
        self.rows
    }
}

/// Байт, которые драйвер прочитает для `rows` строк: все строки, кроме последней, целиком
/// (`stride`), последняя — только видимая часть.
pub(crate) fn required_host_bytes(
    stride: usize,
    visible_row_bytes: usize,
    rows: u32,
) -> Option<usize> {
    let full_rows = usize::try_from(rows.checked_sub(1)?).ok()?;
    full_rows
        .checked_mul(stride)?
        .checked_add(visible_row_bytes)
}

/// Плоскость в DMA-BUF: GPU читает её без копии (импорт `EGLImage`).
///
/// Вариант заложен в словарь сразу (решение владельца 6: источник плоскостей с самого начала
/// допускает 0 копий), а реализуется в сессиях 07 (VA-API) и 12 (программный декод через
/// `udmabuf`). Сейчас загрузка такой плоскости — типизированный отказ
/// [`crate::VideoUploadError::PlaneSourceUnsupported`].
///
/// В отличие от [`HostRows`], буфер DMA-BUF нужен GPU при каждом рисовании: вызывающий держит
/// кадр, пока не сработает fence последнего кадра, который из него рисовал.
#[derive(Debug, Clone, Copy)]
pub struct DmaBufPlane<'a> {
    /// Дескриптор буфера (владеет вызывающий).
    pub fd: BorrowedFd<'a>,
    /// Смещение плоскости в буфере, байт.
    pub offset: u64,
    /// Шаг строк, байт.
    pub pitch: u32,
    /// DRM-модификатор раскладки (`DRM_FORMAT_MOD_*`).
    pub modifier: u64,
    /// DRM fourcc формата плоскости (например, `R8`, `GR88`).
    pub drm_format: u32,
}

/// Откуда брать байты плоскости.
#[derive(Debug, Clone, Copy)]
pub enum PlaneSource<'a> {
    /// Строки в памяти процесса, одна копия драйвером.
    HostRows(HostRows<'a>),
    /// DMA-BUF без копии (сессии 07 и 12).
    DmaBuf(DmaBufPlane<'a>),
}

impl PlaneSource<'_> {
    /// Короткое имя вида источника для диагностики.
    #[must_use]
    pub const fn kind(&self) -> PlaneSourceKind {
        match self {
            Self::HostRows(_) => PlaneSourceKind::HostRows,
            Self::DmaBuf(_) => PlaneSourceKind::DmaBuf,
        }
    }
}

/// Вид источника плоскости без данных.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaneSourceKind {
    /// [`PlaneSource::HostRows`].
    HostRows,
    /// [`PlaneSource::DmaBuf`].
    DmaBuf,
}

/// Непрозрачный ключ кадра вызывающего.
///
/// Видеослой сравнивает ключи только на равенство: тот же ключ подряд — повтор кадра без
/// загрузки. Если ключи могут повториться для другого содержимого (новый декодер начал счёт
/// заново), вызывающий сначала зовёт [`crate::VideoLayerRenderer::forget_uploaded_frames`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VideoFrameKey(pub u64);

/// Кадр для загрузки: раскладка, размер яркости и три источника плоскостей.
#[derive(Debug, Clone, Copy)]
pub struct PlanarFrameUpload<'a> {
    /// Ключ кадра (повтор того же ключа — без загрузки).
    pub key: VideoFrameKey,
    /// Раскладка плоскостей.
    pub layout: PlanarLayout,
    /// Видимый размер яркости в отсчётах. Размер цветности выводится из раскладки.
    pub luma_extent: PlaneExtent,
    /// Плоскость Y.
    pub luma: PlaneSource<'a>,
    /// Плоскость U.
    pub chroma_u: PlaneSource<'a>,
    /// Плоскость V.
    pub chroma_v: PlaneSource<'a>,
}

impl<'a> PlanarFrameUpload<'a> {
    /// Плоскости в порядке Y, U, V вместе с ролью.
    #[must_use]
    pub const fn planes(&self) -> [(PlaneRole, PlaneSource<'a>); 3] {
        [
            (PlaneRole::Luma, self.luma),
            (PlaneRole::ChromaU, self.chroma_u),
            (PlaneRole::ChromaV, self.chroma_v),
        ]
    }

    /// Ожидаемый видимый размер плоскости `role`.
    #[must_use]
    pub const fn expected_extent(&self, role: PlaneRole) -> PlaneExtent {
        match role {
            PlaneRole::Luma => self.luma_extent,
            PlaneRole::ChromaU | PlaneRole::ChromaV => {
                self.layout.chroma.chroma_extent(self.luma_extent)
            }
        }
    }
}

/// Строки плоскости противоречат сами себе.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PlaneSourceError {
    /// Нет ни одной строки или строка пустая.
    #[error("plane has no visible rows or no visible bytes per row")]
    EmptyPlane,
    /// Stride меньше видимой строки: строки налезли бы друг на друга.
    #[error("plane stride {stride} is shorter than its visible row ({visible_row_bytes} bytes)")]
    StrideShorterThanRow {
        /// Шаг строк.
        stride: usize,
        /// Видимых байт в строке.
        visible_row_bytes: usize,
    },
    /// Срез короче видимых байтов.
    #[error("plane bytes are truncated: {actual} bytes, {required} required")]
    TruncatedRows {
        /// Нужно байт.
        required: usize,
        /// Есть байт.
        actual: usize,
    },
    /// Размер плоскости не помещается в `usize`.
    #[error("plane byte size overflows usize")]
    SizeOverflow,
}

/// Плоскость не подходит к заявленной раскладке кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FrameLayoutError {
    /// Размер яркости нулевой.
    #[error("frame luma extent is empty")]
    EmptyFrame,
    /// Видимая строка плоскости не равна ширине × байт на отсчёт.
    #[error("{role:?} plane row has {actual} bytes, layout expects {expected}")]
    RowBytesMismatch {
        /// Плоскость.
        role: PlaneRole,
        /// По раскладке.
        expected: usize,
        /// В источнике.
        actual: usize,
    },
    /// Строк в плоскости не столько, сколько по раскладке.
    #[error("{role:?} plane has {actual} rows, layout expects {expected}")]
    RowCountMismatch {
        /// Плоскость.
        role: PlaneRole,
        /// По раскладке.
        expected: u32,
        /// В источнике.
        actual: u32,
    },
    /// Stride не кратен размеру отсчёта: строка начиналась бы посреди отсчёта.
    #[error(
        "{role:?} plane stride {stride} is not a multiple of {bytes_per_sample} bytes per sample"
    )]
    StrideNotSampleAligned {
        /// Плоскость.
        role: PlaneRole,
        /// Шаг строк.
        stride: usize,
        /// Байт на отсчёт.
        bytes_per_sample: usize,
    },
    /// Размер в байтах не помещается в `usize`.
    #[error("{role:?} plane byte size overflows usize")]
    SizeOverflow {
        /// Плоскость.
        role: PlaneRole,
    },
}

/// Сверяет строки плоскости `role` с раскладкой кадра.
pub(crate) fn validate_host_plane(
    frame: &PlanarFrameUpload<'_>,
    role: PlaneRole,
    rows: &HostRows<'_>,
) -> Result<(), FrameLayoutError> {
    let extent = frame.expected_extent(role);
    let bytes_per_sample = frame.layout.depth.bytes_per_sample();
    let expected_row_bytes = usize::try_from(extent.width)
        .ok()
        .and_then(|width| width.checked_mul(bytes_per_sample))
        .ok_or(FrameLayoutError::SizeOverflow { role })?;
    if rows.visible_row_bytes() != expected_row_bytes {
        return Err(FrameLayoutError::RowBytesMismatch {
            role,
            expected: expected_row_bytes,
            actual: rows.visible_row_bytes(),
        });
    }
    if rows.rows() != extent.height {
        return Err(FrameLayoutError::RowCountMismatch {
            role,
            expected: extent.height,
            actual: rows.rows(),
        });
    }
    if !rows.stride().is_multiple_of(bytes_per_sample) {
        return Err(FrameLayoutError::StrideNotSampleAligned {
            role,
            stride: rows.stride(),
            bytes_per_sample,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
