//! Вызовы GL слоя bitmap-оверлеев (субтитры и титры поверх видео) — часть модуля безопасности
//! [`super`]: те же инварианты [`VideoGl`] (контекст текущий в этом потоке, объекты созданы им),
//! та же проверка «сколько байт прочитает драйвер» до вызова.

use std::ops::Range;

use glow::HasContext;

use super::{TextureDefinition, VideoGl, VideoGlError, gl_int};
use crate::geometry::GlWindowRect;
use crate::upload_plan::TextureExtent;

/// Как хранится атлас оверлея.
///
/// Выборка всегда `NEAREST`: один пиксель холста — ровно один тексель атласа (как `textureLoad`
/// у wgpu), а соседние маски лежат в атласе вплотную — билинейная выборка подмешала бы соседа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AtlasTextureFormat {
    /// Маски покрытия в `GL_LUMINANCE` ядра GLES 2.0 (выборка `(L, L, L, 1)`, нужен `.r`).
    CoverageLuminance,
    /// Маски покрытия в `GL_RED_EXT` (`GL_EXT_texture_rg`, выборка `(R, 0, 0, 1)`).
    CoverageRedExt,
    /// Готовые картинки RGBA8 с премультиплицированной альфой (не sRGB — как `Rgba8Unorm` wgpu).
    PremultipliedRgba,
}

impl AtlasTextureFormat {
    /// `internalformat`/`format` для `glTexImage2D` (в GLES 2.0 они обязаны совпадать).
    const fn gl_format(self) -> u32 {
        match self {
            Self::CoverageLuminance => glow::LUMINANCE,
            // `GL_RED_EXT` = 0x1903 = `GL_RED` (спецификация `EXT_texture_rg`).
            Self::CoverageRedExt => glow::RED,
            Self::PremultipliedRgba => glow::RGBA,
        }
    }

    /// Байт на тексель.
    pub(crate) const fn bytes_per_texel(self) -> usize {
        match self {
            Self::CoverageLuminance | Self::CoverageRedExt => 1,
            Self::PremultipliedRgba => 4,
        }
    }
}

/// Один вызов `glTexSubImage2D` в атлас: прямоугольник `width × rows` текселей с левым верхним
/// углом (`x`, `y`) атласа читается из `source_bytes` среза картинки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AtlasUploadCall {
    /// Столбец атласа, куда ложится первый тексель строки.
    pub(crate) x: u32,
    /// Строка атласа, куда ложится первая строка вызова.
    pub(crate) y: u32,
    /// Текселей в строке.
    pub(crate) width: u32,
    /// Строк в вызове.
    pub(crate) rows: u32,
    /// `GL_UNPACK_ROW_LENGTH` в текселях (только при `GL_EXT_unpack_subimage`); `None` — строки
    /// идут вплотную (шаг = `width`).
    pub(crate) row_length: Option<u32>,
    /// Какие байты среза читает вызов.
    pub(crate) source_bytes: Range<usize>,
}

/// Входы вершинной программы оверлея (номера атрибутов слинкованной программы).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OverlayVertexAttributes {
    /// `a_canvas_position`: угол картинки в пикселях холста, 2 × f32.
    pub(crate) canvas_position: u32,
    /// `a_atlas_uv`: тот же угол в нормированных координатах атласа, 2 × f32.
    pub(crate) atlas_uv: u32,
    /// `a_tint`: цвет картинки, 4 × u8 (нормируются в 0..1).
    pub(crate) tint: u32,
    /// `a_group`: группа размещения 0/1, 1 × f32.
    pub(crate) group: u32,
}

/// Где в вершине лежит каждый атрибут (байты от начала вершины) и шаг вершины.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OverlayVertexLayout {
    /// Шаг вершины в байтах.
    pub(crate) stride: usize,
    /// Смещение `a_canvas_position`.
    pub(crate) canvas_position_offset: usize,
    /// Смещение `a_atlas_uv`.
    pub(crate) atlas_uv_offset: usize,
    /// Смещение `a_tint`.
    pub(crate) tint_offset: usize,
    /// Смещение `a_group`.
    pub(crate) group_offset: usize,
}

impl VideoGl<'_> {
    /// Выделить квадратный атлас стороной `side` без данных: `NEAREST`, `CLAMP_TO_EDGE`, без mip.
    /// Ошибка выделения — по `glGetError`. Прежнее содержимое текстуры (при пересоздании) теряется.
    pub(crate) fn allocate_atlas_texture(
        self,
        texture: glow::NativeTexture,
        format: AtlasTextureFormat,
        side: u32,
    ) -> Result<(), VideoGlError> {
        self.define_texture(
            texture,
            TextureDefinition {
                format: format.gl_format(),
                filter: glow::NEAREST,
                extent: TextureExtent {
                    width: side,
                    height: side,
                },
                pixels: None,
            },
        )
    }

    /// Один вызов загрузки картинки в атлас. `source` — весь срез картинки, вызов читает из него
    /// `call.source_bytes`; длина сверяется с тем, что прочитает драйвер, до вызова.
    pub(crate) fn upload_atlas_rows(
        self,
        texture: glow::NativeTexture,
        format: AtlasTextureFormat,
        call: &AtlasUploadCall,
        source: &[u8],
    ) -> Result<(), VideoGlError> {
        let call_bytes =
            source
                .get(call.source_bytes.clone())
                .ok_or(VideoGlError::UploadSizeMismatch {
                    gl_reads: call.source_bytes.end,
                    available: source.len(),
                })?;
        let gl_reads = atlas_read_size(format, call)?;
        if gl_reads != call_bytes.len() {
            return Err(VideoGlError::UploadSizeMismatch {
                gl_reads,
                available: call_bytes.len(),
            });
        }
        let x = gl_int(call.x, "atlas upload column")?;
        let y = gl_int(call.y, "atlas upload row")?;
        let width = gl_int(call.width, "atlas upload width")?;
        let rows = gl_int(call.rows, "atlas upload rows")?;
        let row_length = gl_int(call.row_length.unwrap_or(0), "unpack row length")?;
        let gl_format = format.gl_format();
        // SAFETY: контекст текущий; текстура создана этим контекстом и выделена стороной, в которую
        // упаковщик атласа уложил прямоугольник вызова. Выравнивание 1 и `GL_UNPACK_ROW_LENGTH`
        // (только при `EXT_unpack_subimage`, иначе `None` → 0) задают шаг строк, по которому
        // `atlas_read_size` посчитал ровно `call_bytes.len()` байт — драйвер не читает за концом
        // среза.
        unsafe {
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            if call.row_length.is_some() {
                self.gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, row_length);
            }
            self.gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                x,
                y,
                width,
                rows,
                gl_format,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(call_bytes)),
            );
            if call.row_length.is_some() {
                self.gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
            }
        }
        Ok(())
    }

    /// Вершины оверлея в буфер (на каждую смену содержимого). `glBufferData` с новыми данными
    /// заменяет хранилище целиком (orphaning): драйвер не ждёт прошлый кадр, который ещё читает
    /// старые вершины.
    pub(crate) fn upload_dynamic_vertices(self, buffer: glow::NativeBuffer, bytes: &[u8]) {
        // SAFETY: контекст текущий; буфер создан этим контекстом; `glBufferData` копирует срез до
        // возврата (OpenGL ES 2.0 §2.9).
        unsafe {
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer));
            self.gl
                .buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::DYNAMIC_DRAW);
            self.gl.bind_buffer(glow::ARRAY_BUFFER, None);
        }
    }

    /// Состояние прохода оверлея: смешивание премультиплицированной альфы
    /// `ONE, ONE_MINUS_SRC_ALPHA` для цвета и альфы (`PREMULTIPLIED_ALPHA_BLENDING` wgpu), без
    /// отбраковки, глубины, трафарета и `GL_DITHER`, запись всех каналов, scissor включён.
    ///
    /// На непрозрачной цели альфа под субтитрами остаётся 1: `a + 1·(1 − a) = 1`.
    pub(crate) fn set_premultiplied_overlay_state(self, scissor: GlWindowRect) {
        // SAFETY: контекст текущий; только константы ядра GLES 2.0 и неотрицательный прямоугольник
        // scissor (размеры из `u32`).
        unsafe {
            self.gl.disable(glow::CULL_FACE);
            self.gl.disable(glow::DEPTH_TEST);
            self.gl.disable(glow::STENCIL_TEST);
            self.gl.disable(glow::DITHER);
            self.gl.color_mask(true, true, true, true);
            self.gl.enable(glow::BLEND);
            self.gl.blend_equation(glow::FUNC_ADD);
            self.gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            self.gl.enable(glow::SCISSOR_TEST);
            self.gl
                .scissor(scissor.x, scissor.y, scissor.width, scissor.height);
        }
    }

    /// Вернуть состояние после прохода: смешивание и scissor выключены (следующие проходы
    /// задают своё явно, но ни один не должен унаследовать обрезку видимой областью видео).
    pub(crate) fn reset_overlay_state(self) {
        // SAFETY: контекст текущий; константы ядра GLES 2.0.
        unsafe {
            self.gl.disable(glow::BLEND);
            self.gl.disable(glow::SCISSOR_TEST);
        }
    }

    /// Привязать атлас к блоку 0 (сэмплер программы оверлея смотрит туда).
    pub(crate) fn bind_atlas_texture(self, texture: glow::NativeTexture) {
        // SAFETY: контекст текущий; текстура создана этим контекстом.
        unsafe {
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        }
    }

    /// Атрибуты вершин оверлея из буфера по раскладке `layout`.
    pub(crate) fn bind_overlay_vertices(
        self,
        buffer: glow::NativeBuffer,
        attributes: OverlayVertexAttributes,
        layout: OverlayVertexLayout,
    ) -> Result<(), VideoGlError> {
        let stride = i32::try_from(layout.stride).map_err(|_| VideoGlError::IntegerOverflow {
            what: "overlay vertex stride",
        })?;
        let offset = |bytes: usize| {
            i32::try_from(bytes).map_err(|_| VideoGlError::IntegerOverflow {
                what: "overlay vertex attribute offset",
            })
        };
        let (position, uv, tint, group) = (
            offset(layout.canvas_position_offset)?,
            offset(layout.atlas_uv_offset)?,
            offset(layout.tint_offset)?,
            offset(layout.group_offset)?,
        );
        // SAFETY: контекст текущий; буфер создан этим контекстом и держит вершины ровно этой
        // раскладки (`overlay::vertices`); номера атрибутов получены у слинкованной программы; VAO
        // не привязан (контракт слоя кадра) — указатели атрибутов — смещения в привязанном буфере.
        unsafe {
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer));
            self.gl.vertex_attrib_pointer_f32(
                attributes.canvas_position,
                2,
                glow::FLOAT,
                false,
                stride,
                position,
            );
            self.gl.vertex_attrib_pointer_f32(
                attributes.atlas_uv,
                2,
                glow::FLOAT,
                false,
                stride,
                uv,
            );
            // Цвет — 4 байта, нормированные в 0..1 (как `Unorm8x4` инстанса wgpu).
            self.gl.vertex_attrib_pointer_f32(
                attributes.tint,
                4,
                glow::UNSIGNED_BYTE,
                true,
                stride,
                tint,
            );
            self.gl.vertex_attrib_pointer_f32(
                attributes.group,
                1,
                glow::FLOAT,
                false,
                stride,
                group,
            );
            for location in [
                attributes.canvas_position,
                attributes.atlas_uv,
                attributes.tint,
                attributes.group,
            ] {
                self.gl.enable_vertex_attrib_array(location);
            }
        }
        Ok(())
    }

    /// Выключить атрибуты оверлея и отвязать буфер: следующий проход (художник egui) включает
    /// свои, и лишний включённый массив без буфера читал бы мимо.
    pub(crate) fn unbind_overlay_vertices(self, attributes: OverlayVertexAttributes) {
        // SAFETY: контекст текущий; номера атрибутов получены у слинкованной программы.
        unsafe {
            for location in [
                attributes.canvas_position,
                attributes.atlas_uv,
                attributes.tint,
                attributes.group,
            ] {
                self.gl.disable_vertex_attrib_array(location);
            }
            self.gl.bind_buffer(glow::ARRAY_BUFFER, None);
        }
    }

    /// Нарисовать треугольники вершин `vertices` из привязанного буфера, в котором лежит
    /// `buffer_vertices` вершин. Диапазон за концом буфера — отказ до драйвера.
    pub(crate) fn draw_overlay_triangles(
        self,
        vertices: Range<usize>,
        buffer_vertices: usize,
    ) -> Result<(), VideoGlError> {
        if vertices.start > vertices.end || vertices.end > buffer_vertices {
            return Err(VideoGlError::DrawOutOfBuffer {
                first: vertices.start,
                end: vertices.end,
                available: buffer_vertices,
            });
        }
        let overflow = VideoGlError::IntegerOverflow {
            what: "overlay vertex range",
        };
        let first = i32::try_from(vertices.start).map_err(|_| overflow.clone())?;
        let count = i32::try_from(vertices.len()).map_err(|_| overflow)?;
        // SAFETY: контекст текущий; программа и атрибуты привязаны (`bind_overlay_vertices`);
        // диапазон проверен выше и лежит внутри буфера.
        unsafe { self.gl.draw_arrays(glow::TRIANGLES, first, count) }
        Ok(())
    }
}

/// Сколько байт прочитает `glTexSubImage2D` атласа при `GL_UNPACK_ALIGNMENT = 1`: все строки,
/// кроме последней, — шагом `GL_UNPACK_ROW_LENGTH` (или ширины вызова), последняя — только
/// шириной вызова (OpenGL ES 2.0 §3.7.1; `EXT_unpack_subimage`).
pub(crate) fn atlas_read_size(
    format: AtlasTextureFormat,
    call: &AtlasUploadCall,
) -> Result<usize, VideoGlError> {
    let overflow = VideoGlError::IntegerOverflow {
        what: "atlas upload byte size",
    };
    let to_usize = |value: u32| usize::try_from(value).map_err(|_| overflow.clone());
    let row_pitch = to_usize(call.row_length.unwrap_or(call.width))?
        .checked_mul(format.bytes_per_texel())
        .ok_or(overflow.clone())?;
    let last_row = to_usize(call.width)?
        .checked_mul(format.bytes_per_texel())
        .ok_or(overflow.clone())?;
    let full_rows = to_usize(call.rows.saturating_sub(1))?;
    full_rows
        .checked_mul(row_pitch)
        .and_then(|bytes| bytes.checked_add(last_row))
        .ok_or(overflow)
}
