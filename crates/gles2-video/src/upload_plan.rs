//! Как загрузить строки плоскости в текстуру за одну копию (чистая логика, без GL).
//!
//! Инвариант README плана: программный путь — не больше одной копии, без промежуточной
//! CPU-переупаковки и без построчных вызовов.
//!
//! Два способа:
//! - **с `GL_EXT_unpack_subimage`**: `GL_UNPACK_ROW_LENGTH = stride / байт на отсчёт`, один
//!   `glTexSubImage2D` на плоскость, текстура шириной в видимую часть;
//! - **без расширения**: текстура шириной в stride (а не в видимую ширину), хвост строки
//!   обрезает шейдер. Блок плоскости кадра заканчивается на видимой части последней строки (так
//!   отдаёт `visible_plane_block` FFmpeg), а вызов шириной в stride прочитал бы `stride × rows`
//!   байт — за концом буфера. Поэтому вызовов два: все строки, кроме последней, шириной в
//!   stride, и последняя строка шириной в видимую часть. Каждый байт по-прежнему копируется
//!   один раз.
//!
//! `GL_UNPACK_ALIGNMENT` всегда 1: шаг строк задаёт либо `GL_UNPACK_ROW_LENGTH`, либо ширина
//! вызова, равная stride, и выравнивание не должно его округлять (OpenGL ES 2.0 §3.7.1, формула
//! шага строки `k`).

use crate::planes::{HostRows, PlaneExtent, required_host_bytes};

/// Чем драйвер умеет задавать шаг строк источника.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowLengthSupport {
    /// `GL_EXT_unpack_subimage`: `GL_UNPACK_ROW_LENGTH` доступен.
    UnpackRowLength,
    /// Только ядро GLES 2.0: шаг строк = ширина вызова.
    StrideWideTexture,
}

/// Размер текстуры плоскости в текселях.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TextureExtent {
    /// Ширина в текселях (может быть больше видимой ширины — stride без расширения).
    pub(crate) width: u32,
    /// Высота в текселях (= видимых строк).
    pub(crate) height: u32,
}

/// Один вызов `glTexSubImage2D`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UploadCall {
    /// Первая строка текстуры, куда пишет вызов.
    pub(crate) first_row: u32,
    /// Ширина вызова в текселях.
    pub(crate) width: u32,
    /// Строк в вызове.
    pub(crate) rows: u32,
    /// Значение `GL_UNPACK_ROW_LENGTH` в текселях; `None` — шаг строк равен ширине вызова.
    pub(crate) row_length: Option<u32>,
    /// Байты источника, которые читает вызов.
    pub(crate) source_bytes: std::ops::Range<usize>,
}

/// План загрузки одной плоскости.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaneUploadPlan {
    /// Размер текстуры, нужный плоскости.
    pub(crate) texture: TextureExtent,
    /// Видимый размер плоскости (его видит шейдер).
    pub(crate) visible: PlaneExtent,
    /// Первый вызов (есть всегда).
    pub(crate) first_call: UploadCall,
    /// Второй вызов — только последняя строка, если текстура шириной в stride.
    pub(crate) last_row_call: Option<UploadCall>,
    /// Байт на тексель (= на отсчёт плоскости).
    pub(crate) bytes_per_texel: usize,
}

impl PlaneUploadPlan {
    /// Вызовы плана по порядку.
    pub(crate) fn calls(&self) -> impl Iterator<Item = &UploadCall> {
        std::iter::once(&self.first_call).chain(self.last_row_call.as_ref())
    }

    /// Сколько байт драйвер записывает в текстуру: ширина × строки каждого вызова. С
    /// `GL_UNPACK_ROW_LENGTH` это ровно видимые байты (хвосты строк пропускаются), у текстуры
    /// шириной в stride — ещё и хвосты всех строк, кроме последней. Каждый байт — один раз.
    pub(crate) fn copied_bytes(&self) -> usize {
        self.calls()
            .map(|call| call.width as usize * call.rows as usize * self.bytes_per_texel)
            .sum()
    }
}

/// Строки плоскости не укладываются в целые тексели.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UnalignedPlaneRows;

/// Строит план загрузки плоскости. Строки уже проверены против раскладки
/// (`planes::validate_host_plane`): видимая строка = ширина × байт на отсчёт, stride кратен
/// отсчёту.
pub(crate) fn plan_plane_upload(
    rows: &HostRows<'_>,
    visible: PlaneExtent,
    bytes_per_sample: usize,
    support: RowLengthSupport,
) -> Result<PlaneUploadPlan, UnalignedPlaneRows> {
    let stride_texels = texels(rows.stride(), bytes_per_sample)?;
    let visible_bytes = required_host_bytes(rows.stride(), rows.visible_row_bytes(), rows.rows())
        .ok_or(UnalignedPlaneRows)?;
    let has_row_tail = stride_texels != visible.width;

    let plan = match (support, has_row_tail) {
        // Нет хвоста строки: один вызов, шаг строк = ширина, при любой поддержке.
        (_, false) => PlaneUploadPlan {
            texture: TextureExtent {
                width: visible.width,
                height: visible.height,
            },
            visible,
            first_call: UploadCall {
                first_row: 0,
                width: visible.width,
                rows: visible.height,
                row_length: None,
                source_bytes: 0..visible_bytes,
            },
            last_row_call: None,
            bytes_per_texel: bytes_per_sample,
        },
        // Хвост есть, но шаг строк задаётся отдельно: один вызов, текстура в видимую ширину.
        (RowLengthSupport::UnpackRowLength, true) => PlaneUploadPlan {
            texture: TextureExtent {
                width: visible.width,
                height: visible.height,
            },
            visible,
            first_call: UploadCall {
                first_row: 0,
                width: visible.width,
                rows: visible.height,
                row_length: Some(stride_texels),
                source_bytes: 0..visible_bytes,
            },
            last_row_call: None,
            bytes_per_texel: bytes_per_sample,
        },
        // Хвост есть, расширения нет: текстура шириной в stride.
        (RowLengthSupport::StrideWideTexture, true) => stride_wide_plan(
            rows,
            visible,
            stride_texels,
            visible_bytes,
            bytes_per_sample,
        ),
    };
    Ok(plan)
}

/// План «текстура шириной в stride»: все строки, кроме последней, одним вызовом, последняя —
/// только видимой частью (иначе вызов прочитал бы хвост, которого нет в источнике).
fn stride_wide_plan(
    rows: &HostRows<'_>,
    visible: PlaneExtent,
    stride_texels: u32,
    visible_bytes: usize,
    bytes_per_texel: usize,
) -> PlaneUploadPlan {
    let texture = TextureExtent {
        width: stride_texels,
        height: visible.height,
    };
    let last_row = visible.height - 1;
    let last_row_start = visible_bytes - rows.visible_row_bytes();
    let last_row_call = UploadCall {
        first_row: last_row,
        width: visible.width,
        rows: 1,
        row_length: None,
        source_bytes: last_row_start..visible_bytes,
    };
    if last_row == 0 {
        // Одна строка: только видимая часть.
        return PlaneUploadPlan {
            texture,
            visible,
            first_call: last_row_call,
            last_row_call: None,
            bytes_per_texel,
        };
    }
    PlaneUploadPlan {
        texture,
        visible,
        first_call: UploadCall {
            first_row: 0,
            width: stride_texels,
            rows: last_row,
            row_length: None,
            source_bytes: 0..last_row_start,
        },
        last_row_call: Some(last_row_call),
        bytes_per_texel,
    }
}

/// Байты → тексели; остаток означает строку, начинающуюся посреди отсчёта.
fn texels(bytes: usize, bytes_per_sample: usize) -> Result<u32, UnalignedPlaneRows> {
    if bytes_per_sample == 0 || !bytes.is_multiple_of(bytes_per_sample) {
        return Err(UnalignedPlaneRows);
    }
    u32::try_from(bytes / bytes_per_sample).map_err(|_| UnalignedPlaneRows)
}

#[cfg(test)]
mod tests;
