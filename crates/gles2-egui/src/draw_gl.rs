//! Модуль безопасности рисования: единственное место `unsafe` для художника egui
//! ([`crate::EguiPainter`]).
//!
//! Все функции `glow::HasContext` — `unsafe`: GL требует текущего в этом потоке контекста и
//! допустимых аргументов. [`DrawGl`] держит оба условия:
//! - создаётся только из начатого кадра ([`DrawGl::for_active_frame`]: `begin_frame` уже сделал
//!   контекст текущим) или через [`Gles2Context::gl`] (делает текущим сам). `Gles2Context` не
//!   `Send`, поэтому контекст не уходит в другой поток, пока жив `DrawGl`;
//! - всё, что обёртка может проверить сама, она проверяет: длина пикселей против размера,
//!   перевод размеров в `i32`. Прочитать за концом CPU-среза драйвер не может.
//!
//! Чего обёртка проверить не может — индексы внутри вершинного буфера при `glDrawElements`.
//! Это держит художник: меш проходит `epaint::Mesh::is_valid` до загрузки (см.
//! `painter::upload`), а [`IndexedDraw`] собирается только там же.
//!
//! Правила модуля: без `unwrap/expect`, отказы — типизированные ошибки, у каждого `unsafe`
//! блока `// SAFETY:`.

use glow::HasContext;
use thiserror::Error;

use crate::context::{GlAccessError, Gles2Context};
use crate::frame::ActiveFrame;

/// Таблица GL контекста, текущего в этом потоке.
#[derive(Clone, Copy)]
pub(crate) struct DrawGl<'a> {
    gl: &'a glow::Context,
}

/// Отказ GL-операции рисования, найденный до драйвера (или отказ драйвера создать объект).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DrawGlError {
    /// Драйвер не создал объект (`glGen*` вернул 0).
    #[error("GL object creation failed: {message}")]
    ObjectCreation {
        /// Сообщение glow.
        message: String,
    },
    /// Срез пикселей не совпадает с размером (иначе драйвер прочитал бы чужую память).
    #[error("pixel slice has {actual} bytes, expected {expected} for {width}x{height} RGBA8")]
    PixelLengthMismatch {
        /// Ширина в текселях.
        width: u32,
        /// Высота в текселях.
        height: u32,
        /// Ожидаемая длина в байтах.
        expected: usize,
        /// Фактическая длина.
        actual: usize,
    },
    /// Размер или смещение не помещается в `GLint`/`GLsizei`.
    #[error("{what} does not fit into a GL integer")]
    IntegerOverflow {
        /// Что именно.
        what: &'static str,
    },
}

/// Тип индексов в буфере индексов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum IndexType {
    /// `GL_UNSIGNED_SHORT` — ядро GLES 2.0.
    U16,
    /// `GL_UNSIGNED_INT` — только при `GL_OES_element_index_uint`.
    U32,
}

impl IndexType {
    const fn gl_enum(self) -> u32 {
        match self {
            Self::U16 => glow::UNSIGNED_SHORT,
            Self::U32 => glow::UNSIGNED_INT,
        }
    }
}

/// Один `glDrawElements(GL_TRIANGLES, ..)`. Собирается только загрузчиком художника после
/// проверки меша: каждый индекс меньше числа вершин, загруженных начиная с привязки атрибутов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IndexedDraw {
    /// Тип индексов.
    pub(crate) index_type: IndexType,
    /// Смещение первого индекса в буфере индексов, байты.
    pub(crate) first_index_byte_offset: i32,
    /// Число индексов (кратно 3).
    pub(crate) index_count: i32,
}

/// Параметры выборки текстуры (в GLES 2.0 они у объекта текстуры, sampler-объектов нет).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TextureSampling {
    /// `GL_TEXTURE_MIN_FILTER`.
    pub(crate) minification: u32,
    /// `GL_TEXTURE_MAG_FILTER`.
    pub(crate) magnification: u32,
    /// `GL_TEXTURE_WRAP_S` и `GL_TEXTURE_WRAP_T` (у egui одна настройка на обе оси).
    pub(crate) wrap: u32,
}

/// Описание одного атрибута вершины для `glVertexAttribPointer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VertexAttribute {
    /// Индекс атрибута в программе.
    pub(crate) location: u32,
    /// Число компонент (1..=4).
    pub(crate) components: i32,
    /// Тип компоненты (`GL_FLOAT`, `GL_UNSIGNED_BYTE`).
    pub(crate) component_type: u32,
    /// Нормализовать целые в 0..=1 (цвет вершины egui: байт / 255).
    pub(crate) normalized: bool,
    /// Шаг между вершинами, байты.
    pub(crate) stride: i32,
    /// Смещение атрибута от начала буфера, байты.
    pub(crate) byte_offset: i32,
}

/// Смешивание `glBlendFuncSeparate(src_rgb, dst_rgb, src_alpha, dst_alpha)` с `FUNC_ADD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlendFunction {
    pub(crate) source_rgb: u32,
    pub(crate) destination_rgb: u32,
    pub(crate) source_alpha: u32,
    pub(crate) destination_alpha: u32,
}

/// Прямоугольник scissor в координатах GL: начало — НИЖНИЙ левый угол цели.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GlScissor {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

/// `usize`/`u32` → `GLint` с понятной ошибкой.
pub(crate) fn gl_int(value: usize, what: &'static str) -> Result<i32, DrawGlError> {
    i32::try_from(value).map_err(|_| DrawGlError::IntegerOverflow { what })
}

impl<'a> DrawGl<'a> {
    /// GL начатого кадра: контекст уже текущий, цель привязана.
    pub(crate) fn for_active_frame(frame: &'a ActiveFrame<'_>) -> Self {
        Self { gl: frame.gl() }
    }

    /// GL вне кадра (загрузка текстур до `begin_frame`). Делает контекст текущим.
    pub(crate) fn for_context(context: &'a Gles2Context) -> Result<Self, GlAccessError> {
        context.gl().map(|gl| Self { gl })
    }

    // --- Объекты -------------------------------------------------------------------------

    pub(crate) fn create_buffer(self) -> Result<glow::NativeBuffer, DrawGlError> {
        // SAFETY: контекст текущий (инвариант `DrawGl`); `glGenBuffers` без аргументов-указателей.
        unsafe { self.gl.create_buffer() }
            .map_err(|message| DrawGlError::ObjectCreation { message })
    }

    pub(crate) fn delete_buffer(self, buffer: glow::NativeBuffer) {
        // SAFETY: контекст текущий; буфер этого контекста, владелец удаляет его один раз.
        unsafe { self.gl.delete_buffer(buffer) }
    }

    pub(crate) fn create_texture(self) -> Result<glow::NativeTexture, DrawGlError> {
        // SAFETY: контекст текущий; `glGenTextures` без аргументов-указателей.
        unsafe { self.gl.create_texture() }
            .map_err(|message| DrawGlError::ObjectCreation { message })
    }

    pub(crate) fn delete_texture(self, texture: glow::NativeTexture) {
        // SAFETY: контекст текущий; текстура этого контекста, владелец удаляет её один раз.
        unsafe { self.gl.delete_texture(texture) }
    }

    /// VAO через `GL_OES_vertex_array_object`. Вызывать только если расширение есть
    /// (`Conveniences::vertex_array_object`): glow берёт `glGenVertexArraysOES`.
    pub(crate) fn create_vertex_array(self) -> Result<glow::NativeVertexArray, DrawGlError> {
        // SAFETY: контекст текущий; наличие расширения проверил вызывающий по строке расширений.
        unsafe { self.gl.create_vertex_array() }
            .map_err(|message| DrawGlError::ObjectCreation { message })
    }

    pub(crate) fn delete_vertex_array(self, vertex_array: glow::NativeVertexArray) {
        // SAFETY: контекст текущий; VAO этого контекста, владелец удаляет его один раз.
        unsafe { self.gl.delete_vertex_array(vertex_array) }
    }

    pub(crate) fn bind_vertex_array(self, vertex_array: Option<glow::NativeVertexArray>) {
        // SAFETY: контекст текущий; VAO этого контекста или 0 (VAO по умолчанию).
        unsafe { self.gl.bind_vertex_array(vertex_array) }
    }

    // --- Программа и uniform -------------------------------------------------------------

    pub(crate) fn use_program(self, program: Option<glow::NativeProgram>) {
        // SAFETY: контекст текущий; программа слинкована в этом контексте или 0.
        unsafe { self.gl.use_program(program) }
    }

    pub(crate) fn attribute_location(
        self,
        program: glow::NativeProgram,
        name: &str,
    ) -> Option<u32> {
        // SAFETY: контекст текущий; программа слинкована; имя — обычная строка (glow добавляет NUL).
        unsafe { self.gl.get_attrib_location(program, name) }
    }

    pub(crate) fn uniform_location(
        self,
        program: glow::NativeProgram,
        name: &str,
    ) -> Option<glow::NativeUniformLocation> {
        // SAFETY: контекст текущий; программа слинкована; имя — обычная строка.
        unsafe { self.gl.get_uniform_location(program, name) }
    }

    /// Uniform `vec2` текущей программы.
    pub(crate) fn set_uniform_vec2(self, location: &glow::NativeUniformLocation, value: [f32; 2]) {
        // SAFETY: контекст текущий; расположение получено у текущей программы.
        unsafe { self.gl.uniform_2_f32(Some(location), value[0], value[1]) }
    }

    /// Uniform `float` текущей программы.
    pub(crate) fn set_uniform_float(self, location: &glow::NativeUniformLocation, value: f32) {
        // SAFETY: контекст текущий; расположение получено у текущей программы.
        unsafe { self.gl.uniform_1_f32(Some(location), value) }
    }

    /// Uniform `int`/`sampler2D` текущей программы.
    pub(crate) fn set_uniform_int(self, location: &glow::NativeUniformLocation, value: i32) {
        // SAFETY: контекст текущий; расположение получено у текущей программы.
        unsafe { self.gl.uniform_1_i32(Some(location), value) }
    }

    // --- Текстуры ------------------------------------------------------------------------

    /// Привязать 2D-текстуру к блоку 0 (художник использует один блок).
    pub(crate) fn bind_texture_unit0(self, texture: Option<glow::NativeTexture>) {
        // SAFETY: контекст текущий; `GL_TEXTURE0` есть всегда; текстура этого контекста или 0.
        unsafe {
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, texture);
        }
    }

    /// Параметры выборки привязанной к блоку 0 текстуры.
    pub(crate) fn set_bound_texture_sampling(self, sampling: TextureSampling) {
        let parameters = [
            (glow::TEXTURE_MIN_FILTER, sampling.minification),
            (glow::TEXTURE_MAG_FILTER, sampling.magnification),
            (glow::TEXTURE_WRAP_S, sampling.wrap),
            (glow::TEXTURE_WRAP_T, sampling.wrap),
        ];
        for (parameter, value) in parameters {
            // Значения — константы GLES 2.0 (`GL_NEAREST` 0x2600 … `GL_MIRRORED_REPEAT` 0x8370),
            // все меньше 2^31, поэтому приведение к `GLint` без потерь.
            let value = value as i32;
            // SAFETY: контекст текущий; текстура привязана к `GL_TEXTURE_2D`; параметр и значение
            // — допустимые константы спецификации GLES 2.0 (3.7.4).
            unsafe {
                self.gl
                    .tex_parameter_i32(glow::TEXTURE_2D, parameter, value)
            }
        }
    }

    /// Раскладка загрузки RGBA8: строки плотные. `row_length_reset` — сбросить
    /// `GL_UNPACK_ROW_LENGTH` в 0 (есть только с `GL_EXT_unpack_subimage`; видео-слой может
    /// оставить там шаг своей плоскости).
    pub(crate) fn set_tight_rgba_unpack(self, row_length_reset: UnpackRowLength) {
        // SAFETY: контекст текущий; 4 — допустимое выравнивание, строка RGBA8 всегда кратна 4.
        unsafe { self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4) }
        if row_length_reset == UnpackRowLength::ResetSupported {
            // SAFETY: контекст текущий; `GL_UNPACK_ROW_LENGTH` (0x0CF2) существует при
            // `GL_EXT_unpack_subimage`, это проверил вызывающий; 0 = «по ширине загрузки».
            unsafe { self.gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0) }
        }
    }

    /// Выделить хранилище привязанной текстуры и залить его (`glTexImage2D`, RGBA8).
    pub(crate) fn upload_bound_texture_rgba8(
        self,
        size: [u32; 2],
        pixels: &[u8],
    ) -> Result<(), DrawGlError> {
        let [width, height] = checked_rgba_extent(size, pixels)?;
        // SAFETY: контекст текущий; текстура привязана к блоку 0; длина среза проверена ровно
        // под width × height × 4 при выравнивании 4 (`set_tight_rgba_unpack`), поэтому драйвер
        // не читает за концом; формат и тип — `GL_RGBA`/`GL_UNSIGNED_BYTE` ядра GLES 2.0.
        unsafe {
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                width,
                height,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(pixels)),
            );
        }
        Ok(())
    }

    /// Переписать часть привязанной текстуры (`glTexSubImage2D`, RGBA8). Границы относительно
    /// текстуры проверяет вызывающий (`painter::textures`); драйвер на выход за границы отвечает
    /// `GL_INVALID_VALUE`, а не порчей памяти.
    pub(crate) fn upload_bound_texture_patch_rgba8(
        self,
        offset: [u32; 2],
        size: [u32; 2],
        pixels: &[u8],
    ) -> Result<(), DrawGlError> {
        let [width, height] = checked_rgba_extent(size, pixels)?;
        let x = gl_int(offset[0] as usize, "texture patch x offset")?;
        let y = gl_int(offset[1] as usize, "texture patch y offset")?;
        // SAFETY: как у `upload_bound_texture_rgba8`: длина среза проверена под размер правки.
        unsafe {
            self.gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                x,
                y,
                width,
                height,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(pixels)),
            );
        }
        Ok(())
    }

    // --- Буферы и атрибуты ---------------------------------------------------------------

    pub(crate) fn bind_array_buffer(self, buffer: Option<glow::NativeBuffer>) {
        // SAFETY: контекст текущий; буфер этого контекста или 0.
        unsafe { self.gl.bind_buffer(glow::ARRAY_BUFFER, buffer) }
    }

    pub(crate) fn bind_element_buffer(self, buffer: Option<glow::NativeBuffer>) {
        // SAFETY: контекст текущий; буфер этого контекста или 0.
        unsafe { self.gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, buffer) }
    }

    /// Стриминг в привязанный буфер: сначала «осиротить» старое хранилище
    /// (`glBufferData(NULL)`), потом записать. Драйвер отдаёт новую память, если GPU ещё читает
    /// старую, вместо остановки конвейера (на Ironlake запись в занятый объект — +8 мс к кадру,
    /// разведка 00).
    pub(crate) fn stream_into_bound_buffer(
        self,
        target: BufferTarget,
        bytes: &[u8],
    ) -> Result<(), DrawGlError> {
        let size = gl_int(bytes.len(), "stream buffer size")?;
        let target = target.gl_enum();
        // SAFETY: контекст текущий; буфер привязан к `target`; размер проверен в `GLsizeiptr`;
        // запись ровно `bytes.len()` байт со смещения 0 в только что выделенное хранилище этого
        // же размера; glow берёт указатель и длину из среза.
        unsafe {
            self.gl.buffer_data_size(target, size, glow::STREAM_DRAW);
            self.gl.buffer_sub_data_u8_slice(target, 0, bytes);
        }
        Ok(())
    }

    /// Описать атрибут вершины относительно привязанного `GL_ARRAY_BUFFER` и включить его.
    pub(crate) fn enable_vertex_attribute(self, attribute: VertexAttribute) {
        // SAFETY: контекст текущий; к `GL_ARRAY_BUFFER` привязан буфер (смещение — внутри
        // буфера, не указатель на CPU-память); индекс получен у слинкованной программы.
        unsafe {
            self.gl.vertex_attrib_pointer_f32(
                attribute.location,
                attribute.components,
                attribute.component_type,
                attribute.normalized,
                attribute.stride,
                attribute.byte_offset,
            );
            self.gl.enable_vertex_attrib_array(attribute.location);
        }
    }

    pub(crate) fn disable_vertex_attribute(self, location: u32) {
        // SAFETY: контекст текущий; индекс получен у слинкованной программы.
        unsafe { self.gl.disable_vertex_attrib_array(location) }
    }

    // --- Состояние конвейера -------------------------------------------------------------

    /// Состояние, которое художник задаёт явно, не полагаясь на чужие проходы:
    /// без отбраковки граней (egui не держит единый порядок обхода), глубины, трафарета и
    /// аппаратного дизеринга GL (`GL_DITHER` включён по умолчанию, а его результат зависит от
    /// реализации — паритет требует предсказуемых пикселей), запись всех каналов.
    pub(crate) fn set_plain_2d_state(self) {
        // SAFETY: контекст текущий; только константы `glEnable/glDisable` ядра GLES 2.0.
        unsafe {
            self.gl.disable(glow::CULL_FACE);
            self.gl.disable(glow::DEPTH_TEST);
            self.gl.disable(glow::STENCIL_TEST);
            self.gl.disable(glow::DITHER);
            self.gl.color_mask(true, true, true, true);
        }
    }

    pub(crate) fn enable_blend(self, function: BlendFunction) {
        // SAFETY: контекст текущий; множители — константы `glBlendFuncSeparate` GLES 2.0.
        unsafe {
            self.gl.enable(glow::BLEND);
            self.gl.blend_equation(glow::FUNC_ADD);
            self.gl.blend_func_separate(
                function.source_rgb,
                function.destination_rgb,
                function.source_alpha,
                function.destination_alpha,
            );
        }
    }

    pub(crate) fn viewport(self, width: i32, height: i32) {
        // SAFETY: контекст текущий; размеры неотрицательны (из `SurfaceSize`).
        unsafe { self.gl.viewport(0, 0, width, height) }
    }

    pub(crate) fn enable_scissor(self, scissor: GlScissor) {
        // SAFETY: контекст текущий; ширина и высота неотрицательны (собирает `painter::scissor`).
        unsafe {
            self.gl.enable(glow::SCISSOR_TEST);
            self.gl
                .scissor(scissor.x, scissor.y, scissor.width, scissor.height);
        }
    }

    pub(crate) fn disable_scissor(self) {
        // SAFETY: контекст текущий; константа ядра GLES 2.0.
        unsafe { self.gl.disable(glow::SCISSOR_TEST) }
    }

    // --- Рисование -----------------------------------------------------------------------

    /// `glDrawElements(GL_TRIANGLES, ..)` из привязанного буфера индексов.
    pub(crate) fn draw_indexed_triangles(self, draw: IndexedDraw) {
        // SAFETY: контекст текущий; к `GL_ELEMENT_ARRAY_BUFFER` привязан буфер индексов художника
        // (смещение — внутри него); каждый индекс меньше числа вершин от привязки атрибутов —
        // `IndexedDraw` собирается только после `Mesh::is_valid` (см. документацию модуля).
        unsafe {
            self.gl.draw_elements(
                glow::TRIANGLES,
                draw.index_count,
                draw.index_type.gl_enum(),
                draw.first_index_byte_offset,
            );
        }
    }
}

/// Сбрасывать ли `GL_UNPACK_ROW_LENGTH` перед загрузкой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnpackRowLength {
    /// `GL_EXT_unpack_subimage` есть: сбросить в 0.
    ResetSupported,
    /// Расширения нет: параметра не существует, трогать нельзя.
    NotSupported,
}

/// Куда привязан буфер.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BufferTarget {
    /// `GL_ARRAY_BUFFER`.
    Vertices,
    /// `GL_ELEMENT_ARRAY_BUFFER`.
    Indices,
}

impl BufferTarget {
    const fn gl_enum(self) -> u32 {
        match self {
            Self::Vertices => glow::ARRAY_BUFFER,
            Self::Indices => glow::ELEMENT_ARRAY_BUFFER,
        }
    }
}

/// Проверка «срез RGBA8 ровно под размер» и перевод размера в `GLsizei`.
fn checked_rgba_extent(size: [u32; 2], pixels: &[u8]) -> Result<[i32; 2], DrawGlError> {
    let [width, height] = size;
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|texels| texels.checked_mul(4))
        .ok_or(DrawGlError::IntegerOverflow {
            what: "texture byte size",
        })?;
    if pixels.len() != expected {
        return Err(DrawGlError::PixelLengthMismatch {
            width,
            height,
            expected,
            actual: pixels.len(),
        });
    }
    Ok([
        gl_int(width as usize, "texture width")?,
        gl_int(height as usize, "texture height")?,
    ])
}

#[cfg(test)]
mod tests {
    use super::{DrawGlError, checked_rgba_extent};

    #[test]
    fn rgba_extent_requires_exact_pixel_length() {
        assert_eq!(checked_rgba_extent([2, 3], &[0; 24]), Ok([2, 3]));
        assert_eq!(
            checked_rgba_extent([2, 3], &[0; 23]),
            Err(DrawGlError::PixelLengthMismatch {
                width: 2,
                height: 3,
                expected: 24,
                actual: 23,
            })
        );
        assert!(matches!(
            checked_rgba_extent([u32::MAX, 2], &[]),
            Err(DrawGlError::IntegerOverflow { .. }) | Err(DrawGlError::PixelLengthMismatch { .. })
        ));
    }
}
