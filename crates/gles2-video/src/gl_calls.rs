//! Модуль безопасности видеослоя: единственное место `unsafe` в crate-е.
//!
//! Все функции `glow::HasContext` — `unsafe`: GL требует текущего в этом потоке контекста и
//! допустимых аргументов. [`VideoGl`] держит оба условия:
//! - создаётся только из начатого кадра ([`VideoGl::for_active_frame`]: `begin_frame` уже сделал
//!   контекст текущим) или через [`Gles2Context::gl`] (делает текущим сам). `Gles2Context` не
//!   `Send`, поэтому контекст не уходит в другой поток, пока жив `VideoGl`;
//! - всё, что обёртка может проверить сама, она проверяет: сколько байт прочитает драйвер при
//!   загрузке (по ширине, строкам, `GL_UNPACK_ROW_LENGTH` и выравниванию 1) против длины среза,
//!   перевод размеров в `GLint`. Прочитать за концом CPU-среза драйвер не может.
//!
//! Правила модуля (по образцу `gles2-egui/src/draw_gl.rs`): без `unwrap/expect`, отказы —
//! типизированные ошибки, у каждого `unsafe` блока `// SAFETY:`.

use glow::HasContext;
use thiserror::Error;

use gles2_egui::{ActiveFrame, GlAccessError, Gles2Context};

use crate::geometry::GlWindowRect;
use crate::program::ChromaFilter;
use crate::upload_plan::{TextureExtent, UploadCall};

/// Как хранится плоскость в текстуре.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaneTextureFormat {
    /// `GL_LUMINANCE` ядра GLES 2.0: выборка даёт `(L, L, L, 1)`.
    Luminance,
    /// `GL_RED_EXT` из `GL_EXT_texture_rg`: настоящая одноканальная текстура, `(R, 0, 0, 1)`.
    RedExt,
    /// `GL_LUMINANCE_ALPHA` ядра GLES 2.0 для 10/12 бит: два байта на тексель, первый (младший
    /// байт LE-слова) — `L`, второй (старший) — `A`; выборка `(L, L, L, A)`. Шейдер собирает код
    /// `L·255 + A·255·256`. Фильтрация только `NEAREST`: смешивание соседних текселей смешало бы
    /// байты по отдельности и дало мусорный код (у wgpu 16-битный путь тоже без фильтрации —
    /// `textureLoad`).
    LuminanceAlpha,
}

impl PlaneTextureFormat {
    /// `internalformat`/`format` для `glTexImage2D` (в GLES 2.0 они обязаны совпадать).
    const fn gl_format(self) -> u32 {
        match self {
            Self::Luminance => glow::LUMINANCE,
            // `GL_RED_EXT` = 0x1903 = `GL_RED` (спецификация `EXT_texture_rg`).
            Self::RedExt => glow::RED,
            Self::LuminanceAlpha => glow::LUMINANCE_ALPHA,
        }
    }

    /// Байт на тексель.
    pub(crate) const fn bytes_per_texel(self) -> usize {
        match self {
            Self::Luminance | Self::RedExt => 1,
            Self::LuminanceAlpha => 2,
        }
    }

    /// Фильтр выборки: 8 бит — билинейный (паритет с wgpu), пары байтов — только nearest.
    pub(crate) const fn filter(self) -> u32 {
        match self {
            Self::Luminance | Self::RedExt => glow::LINEAR,
            Self::LuminanceAlpha => glow::NEAREST,
        }
    }
}

/// Отказ GL-операции видеослоя.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VideoGlError {
    /// Драйвер не создал объект (`glGen*` вернул 0).
    #[error("GL object creation failed: {message}")]
    ObjectCreation {
        /// Сообщение glow.
        message: String,
    },
    /// Драйвер не выделил память под текстуру плоскости.
    #[error("GL could not allocate a {width}x{height} plane texture (GL error 0x{gl_error:04x})")]
    TextureAllocation {
        /// Ширина.
        width: u32,
        /// Высота.
        height: u32,
        /// Код `glGetError`.
        gl_error: u32,
    },
    /// Вызов загрузки прочитал бы не те байты, что ему выделены (ошибка плана — защита до
    /// драйвера).
    #[error("upload call would read {gl_reads} bytes, but {available} bytes were given")]
    UploadSizeMismatch {
        /// Сколько прочитал бы драйвер.
        gl_reads: usize,
        /// Сколько выделено вызову.
        available: usize,
    },
    /// Размер или смещение не помещается в `GLint`/`GLsizei`.
    #[error("{what} does not fit into a GL integer")]
    IntegerOverflow {
        /// Что именно.
        what: &'static str,
    },
    /// Цель рисования самопроверки не стала полной (`glCheckFramebufferStatus` не
    /// `GL_FRAMEBUFFER_COMPLETE`).
    #[error("probe render target is incomplete (framebuffer status 0x{status:04x})")]
    FramebufferIncomplete {
        /// Ответ `glCheckFramebufferStatus`.
        status: u32,
    },
    /// Draw-вызов прочитал бы вершины за концом буфера (ошибка плана рисования — защита до
    /// драйвера).
    #[error("draw call would read vertices {first}..{end}, but the buffer holds {available}")]
    DrawOutOfBuffer {
        /// Первая вершина вызова.
        first: usize,
        /// Конец диапазона (не включая).
        end: usize,
        /// Вершин в буфере.
        available: usize,
    },
}

/// Текстурный блок таблицы HDR → SDR (после Y, U, V в блоках 0–2).
pub(crate) const HDR_LUT_TEXTURE_UNIT: i32 = 3;

/// Что задаёт `glTexImage2D` текстуры видеослоя.
struct TextureDefinition<'a> {
    /// `internalformat`/`format` (в GLES 2.0 совпадают).
    format: u32,
    /// `MIN`/`MAG` фильтр.
    filter: u32,
    extent: TextureExtent,
    /// Данные уровня 0 или `None` — только выделить.
    pixels: Option<&'a [u8]>,
}

/// Таблица GL контекста, текущего в этом потоке.
#[derive(Clone, Copy)]
pub(crate) struct VideoGl<'a> {
    gl: &'a glow::Context,
}

fn gl_int(value: u32, what: &'static str) -> Result<i32, VideoGlError> {
    i32::try_from(value).map_err(|_| VideoGlError::IntegerOverflow { what })
}

impl<'a> VideoGl<'a> {
    /// GL начатого кадра: контекст уже текущий, цель привязана.
    pub(crate) fn for_active_frame(frame: &'a ActiveFrame<'_>) -> Self {
        Self { gl: frame.gl() }
    }

    /// GL вне кадра (загрузка плоскостей до `begin_frame`). Делает контекст текущим.
    pub(crate) fn for_context(context: &'a Gles2Context) -> Result<Self, GlAccessError> {
        context.gl().map(|gl| Self { gl })
    }

    // --- Объекты -------------------------------------------------------------------------

    pub(crate) fn create_texture(self) -> Result<glow::NativeTexture, VideoGlError> {
        // SAFETY: контекст текущий (инвариант `VideoGl`); `glGenTextures` без указателей.
        unsafe { self.gl.create_texture() }
            .map_err(|message| VideoGlError::ObjectCreation { message })
    }

    pub(crate) fn delete_texture(self, texture: glow::NativeTexture) {
        // SAFETY: контекст текущий; объект создан этим контекстом и больше не используется.
        unsafe { self.gl.delete_texture(texture) }
    }

    pub(crate) fn create_buffer(self) -> Result<glow::NativeBuffer, VideoGlError> {
        // SAFETY: контекст текущий; `glGenBuffers` без указателей.
        unsafe { self.gl.create_buffer() }
            .map_err(|message| VideoGlError::ObjectCreation { message })
    }

    pub(crate) fn delete_buffer(self, buffer: glow::NativeBuffer) {
        // SAFETY: контекст текущий; объект создан этим контекстом и больше не используется.
        unsafe { self.gl.delete_buffer(buffer) }
    }

    pub(crate) fn use_program(self, program: Option<glow::NativeProgram>) {
        // SAFETY: контекст текущий; программа слинкована этим контекстом или `None` (0).
        unsafe { self.gl.use_program(program) }
    }

    pub(crate) fn attribute_location(
        self,
        program: glow::NativeProgram,
        name: &str,
    ) -> Option<u32> {
        // SAFETY: контекст текущий; программа слинкована; имя — обычная строка Rust (glow сам
        // добавляет завершающий ноль).
        unsafe { self.gl.get_attrib_location(program, name) }
    }

    pub(crate) fn uniform_location(
        self,
        program: glow::NativeProgram,
        name: &str,
    ) -> Option<glow::NativeUniformLocation> {
        // SAFETY: контекст текущий; программа слинкована; имя — обычная строка Rust.
        unsafe { self.gl.get_uniform_location(program, name) }
    }

    // --- Uniform-ы (программа уже `use_program`) -----------------------------------------

    pub(crate) fn set_uniform_int(self, location: &glow::NativeUniformLocation, value: i32) {
        // SAFETY: контекст текущий; location получен у текущей программы; тип `int`/`sampler2D`.
        unsafe { self.gl.uniform_1_i32(Some(location), value) }
    }

    pub(crate) fn set_uniform_vec2(self, location: &glow::NativeUniformLocation, value: [f32; 2]) {
        // SAFETY: контекст текущий; location получен у текущей программы; тип `vec2`.
        unsafe { self.gl.uniform_2_f32(Some(location), value[0], value[1]) }
    }

    pub(crate) fn set_uniform_vec3(self, location: &glow::NativeUniformLocation, value: [f32; 3]) {
        // SAFETY: контекст текущий; location получен у текущей программы; тип `vec3`.
        unsafe {
            self.gl
                .uniform_3_f32(Some(location), value[0], value[1], value[2])
        }
    }

    pub(crate) fn set_uniform_vec4(self, location: &glow::NativeUniformLocation, value: [f32; 4]) {
        // SAFETY: контекст текущий; location получен у текущей программы; тип `vec4`.
        unsafe {
            self.gl
                .uniform_4_f32(Some(location), value[0], value[1], value[2], value[3]);
        }
    }

    // --- Текстуры плоскостей -------------------------------------------------------------

    /// Выделить текстуру плоскости без данных: фильтр формата ([`PlaneTextureFormat`]),
    /// `CLAMP_TO_EDGE`, без mip (единственный вариант для текстур не степени двойки в ядре
    /// GLES 2.0, §3.8.2). Привязка — к блоку 0.
    pub(crate) fn allocate_plane_texture(
        self,
        texture: glow::NativeTexture,
        format: PlaneTextureFormat,
        extent: TextureExtent,
    ) -> Result<(), VideoGlError> {
        self.define_texture(
            texture,
            TextureDefinition {
                format: format.gl_format(),
                filter: format.filter(),
                extent,
                pixels: None,
            },
        )
    }

    /// Создать текстуру атласа таблицы HDR → SDR (RGBA8, билинейная) сразу с данными.
    /// `rgba` — ровно `ширина × высота × 4` байт, иначе отказ до драйвера.
    pub(crate) fn allocate_hdr_lut_texture(
        self,
        texture: glow::NativeTexture,
        extent: TextureExtent,
        rgba: &[u8],
    ) -> Result<(), VideoGlError> {
        let to_usize = |value: u32| usize::try_from(value).ok();
        let expected = to_usize(extent.width)
            .zip(to_usize(extent.height))
            .and_then(|(width, height)| width.checked_mul(height)?.checked_mul(4))
            .ok_or(VideoGlError::IntegerOverflow {
                what: "HDR table byte size",
            })?;
        if expected != rgba.len() {
            return Err(VideoGlError::UploadSizeMismatch {
                gl_reads: expected,
                available: rgba.len(),
            });
        }
        self.define_texture(
            texture,
            TextureDefinition {
                format: glow::RGBA,
                filter: glow::LINEAR,
                extent,
                pixels: Some(rgba),
            },
        )
    }

    /// `glTexImage2D` уровня 0 с параметрами выборки; ошибка выделения — по `glGetError`.
    fn define_texture(
        self,
        texture: glow::NativeTexture,
        definition: TextureDefinition<'_>,
    ) -> Result<(), VideoGlError> {
        let TextureDefinition {
            format,
            filter,
            extent,
            pixels,
        } = definition;
        let width = gl_int(extent.width, "texture width")?;
        let height = gl_int(extent.height, "texture height")?;
        self.report_stale_gl_errors();
        // SAFETY: контекст текущий; текстура создана этим контекстом; параметры — константы
        // GLES 2.0. Данные: `None` (NULL) — драйвер только выделяет память; `Some` — срез, длину
        // которого вызывающий сверил с `ширина × высота × байт на тексель` при `UNPACK_ALIGNMENT`
        // 1 (ставится здесь же), то есть драйвер не читает за его концом.
        let allocation_error = unsafe {
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            for (parameter, value) in [
                (glow::TEXTURE_MIN_FILTER, filter),
                (glow::TEXTURE_MAG_FILTER, filter),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                // Константы GL — небольшие u32, в i32 помещаются без потерь.
                self.gl
                    .tex_parameter_i32(glow::TEXTURE_2D, parameter, value.cast_signed());
            }
            self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                format.cast_signed(),
                width,
                height,
                0,
                format,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(pixels),
            );
            self.gl.get_error()
        };
        if allocation_error == glow::NO_ERROR {
            return Ok(());
        }
        Err(VideoGlError::TextureAllocation {
            width: extent.width,
            height: extent.height,
            gl_error: allocation_error,
        })
    }

    /// Чужие флаги ошибок GL до нашего выделения: сообщаем в лог, чтобы не приписать их себе.
    fn report_stale_gl_errors(self) {
        // `glGetError` отдаёт по одному флагу за вызов; флагов не больше, чем видов ошибок.
        const MAX_PENDING_ERROR_FLAGS: usize = 8;
        for _ in 0..MAX_PENDING_ERROR_FLAGS {
            // SAFETY: контекст текущий; `glGetError` без аргументов.
            let stale = unsafe { self.gl.get_error() };
            if stale == glow::NO_ERROR {
                return;
            }
            log::warn!("gles2-video: GL error 0x{stale:04x} left by earlier GL work");
        }
    }

    /// Один вызов `glTexSubImage2D` плана загрузки. `source` — весь срез плоскости, вызов
    /// читает из него `call.source_bytes`.
    pub(crate) fn upload_plane_rows(
        self,
        texture: glow::NativeTexture,
        format: PlaneTextureFormat,
        call: &UploadCall,
        source: &[u8],
    ) -> Result<(), VideoGlError> {
        let call_bytes =
            source
                .get(call.source_bytes.clone())
                .ok_or(VideoGlError::UploadSizeMismatch {
                    gl_reads: call.source_bytes.end,
                    available: source.len(),
                })?;
        let gl_reads = gl_read_size(format, call)?;
        if gl_reads != call_bytes.len() {
            return Err(VideoGlError::UploadSizeMismatch {
                gl_reads,
                available: call_bytes.len(),
            });
        }
        let first_row = gl_int(call.first_row, "upload first row")?;
        let width = gl_int(call.width, "upload width")?;
        let rows = gl_int(call.rows, "upload rows")?;
        let row_length = gl_int(call.row_length.unwrap_or(0), "unpack row length")?;
        let gl_format = format.gl_format();
        // SAFETY: контекст текущий; текстура создана этим контекстом и выделена размером не
        // меньше вызова (`TextureSet::ensure_allocated` по тому же плану). Выравнивание 1 и
        // `GL_UNPACK_ROW_LENGTH` (только при `EXT_unpack_subimage`, иначе план даёт `None` → 0)
        // задают шаг строк, по которому `gl_read_size` посчитал ровно `call_bytes.len()` байт —
        // драйвер не читает за концом среза.
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
                0,
                first_row,
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

    /// Вершины полноэкранного треугольника в буфер (один раз при создании).
    pub(crate) fn upload_static_vertices(self, buffer: glow::NativeBuffer, vertices: &[f32]) {
        let bytes: Vec<u8> = vertices
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect();
        // SAFETY: контекст текущий; буфер создан этим контекстом; срез байт живёт до возврата, а
        // `glBufferData` копирует его до возврата (OpenGL ES 2.0 §2.9).
        unsafe {
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer));
            self.gl
                .buffer_data_u8_slice(glow::ARRAY_BUFFER, &bytes, glow::STATIC_DRAW);
            self.gl.bind_buffer(glow::ARRAY_BUFFER, None);
        }
    }

    // --- Рисование -----------------------------------------------------------------------

    /// Состояние, которое видеослой задаёт явно: без смешивания (видео непрозрачно), отбраковки,
    /// глубины, трафарета и `GL_DITHER` (его результат зависит от реализации — паритет требует
    /// предсказуемых пикселей), запись всех каналов, scissor выключен.
    pub(crate) fn set_opaque_video_state(self) {
        // SAFETY: контекст текущий; только константы `glEnable/glDisable` ядра GLES 2.0.
        unsafe {
            self.gl.disable(glow::BLEND);
            self.gl.disable(glow::CULL_FACE);
            self.gl.disable(glow::DEPTH_TEST);
            self.gl.disable(glow::STENCIL_TEST);
            self.gl.disable(glow::DITHER);
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.color_mask(true, true, true, true);
        }
    }

    /// Залить непрозрачным чёрным (как `ClearBlack` видеопрохода wgpu) — всю цель или, при
    /// включённом scissor, только его прямоугольник.
    pub(crate) fn clear_opaque_black(self) {
        // SAFETY: контекст текущий; заливка ограничена текущим scissor (или всей целью без него).
        unsafe {
            self.gl.clear_color(0.0, 0.0, 0.0, 1.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }
    }

    pub(crate) fn viewport(self, rect: GlWindowRect) {
        // SAFETY: контекст текущий; размеры неотрицательны (из `u32`), GL ограничит их сам.
        unsafe { self.gl.viewport(rect.x, rect.y, rect.width, rect.height) }
    }

    pub(crate) fn enable_scissor(self) {
        // SAFETY: контекст текущий; константа ядра GLES 2.0.
        unsafe { self.gl.enable(glow::SCISSOR_TEST) }
    }

    pub(crate) fn disable_scissor(self) {
        // SAFETY: контекст текущий; константа ядра GLES 2.0.
        unsafe { self.gl.disable(glow::SCISSOR_TEST) }
    }

    pub(crate) fn scissor(self, rect: GlWindowRect) {
        // SAFETY: контекст текущий; ширина и высота неотрицательны (из `u32`).
        unsafe { self.gl.scissor(rect.x, rect.y, rect.width, rect.height) }
    }

    /// Привязать текстуры Y, U, V к блокам 0, 1, 2 и таблицу HDR (если есть) к блоку
    /// [`HDR_LUT_TEXTURE_UNIT`]; активным остаётся блок 0 (его ждёт художник).
    ///
    /// Фильтр задаётся здесь, при привязке, а не при выделении: импортированные и загруженные
    /// текстуры рисуют одни и те же программы, а фильтр цветности — свойство программы
    /// (`ChromaFilter`: co-sited 4:2:0 берёт сама nearest-выборка, сессия 12a). Повтор того же
    /// значения драйвер пропускает.
    pub(crate) fn bind_plane_textures(
        self,
        textures: [glow::NativeTexture; 3],
        format: PlaneTextureFormat,
        chroma_filter: ChromaFilter,
        hdr_lut: Option<glow::NativeTexture>,
    ) {
        let filters = [
            format.filter(),
            chroma_filter.gl_filter(),
            chroma_filter.gl_filter(),
        ];
        // SAFETY: контекст текущий; текстуры созданы этим контекстом; блоки 0–3 есть в любой
        // реализации GLES 2.0 (`MAX_TEXTURE_IMAGE_UNITS` ≥ 8, таблица 6.20); значения фильтров —
        // константы GLES 2.0, в i32 помещаются без потерь.
        unsafe {
            for ((unit, texture), filter) in [glow::TEXTURE0, glow::TEXTURE1, glow::TEXTURE2]
                .into_iter()
                .zip(textures)
                .zip(filters)
            {
                self.gl.active_texture(unit);
                self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                for parameter in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
                    self.gl
                        .tex_parameter_i32(glow::TEXTURE_2D, parameter, filter.cast_signed());
                }
            }
            if let Some(table) = hdr_lut {
                self.gl
                    .active_texture(glow::TEXTURE0 + HDR_LUT_TEXTURE_UNIT.cast_unsigned());
                self.gl.bind_texture(glow::TEXTURE_2D, Some(table));
            }
            self.gl.active_texture(glow::TEXTURE0);
        }
    }

    /// Привязать текстуры яркости и цветности NV12 (импорт DMA-BUF) к блокам 0 и 1; активным
    /// остаётся блок 0.
    pub(crate) fn bind_nv12_textures(self, luma: glow::NativeTexture, chroma: glow::NativeTexture) {
        // SAFETY: контекст текущий; текстуры созданы этим контекстом; блоки 0 и 1 есть в любой
        // реализации GLES 2.0 (`MAX_TEXTURE_IMAGE_UNITS` ≥ 8, таблица 6.20).
        unsafe {
            self.gl.active_texture(glow::TEXTURE1);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(chroma));
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(luma));
        }
    }

    /// Параметры выборки текстуры, чьё хранилище задано `EGLImage`: билинейный фильтр и
    /// `CLAMP_TO_EDGE`, как у `Sampler` NV12-пути wgpu. Ставятся ПОСЛЕ привязки образа: без
    /// них текстура остаётся с умолчанием `NEAREST_MIPMAP_LINEAR`, а у текстуры без мипмапов
    /// это «неполная» текстура, и выборка вернула бы чёрный (OpenGL ES 2.0.25, §3.7.10
    /// «Texture Completeness»). Привязка блока 0 после вызова сброшена.
    pub(crate) fn set_imported_texture_sampling(self, texture: glow::NativeTexture) {
        // SAFETY: контекст текущий; текстура создана этим контекстом; параметры — константы
        // GLES 2.0 (значения фильтра и wrap — небольшие u32, в i32 помещаются без потерь).
        unsafe {
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            for (parameter, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                self.gl
                    .tex_parameter_i32(glow::TEXTURE_2D, parameter, value.cast_signed());
            }
            self.gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    /// Атрибут позиции из буфера вершин: 2 × f32 подряд.
    pub(crate) fn bind_vertex_positions(self, buffer: glow::NativeBuffer, location: u32) {
        // SAFETY: контекст текущий; буфер создан этим контекстом и держит 3 вершины по 2 f32;
        // location получен у слинкованной программы; VAO не привязан (контракт слоя кадра).
        unsafe {
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer));
            self.gl
                .vertex_attrib_pointer_f32(location, 2, glow::FLOAT, false, 0, 0);
            self.gl.enable_vertex_attrib_array(location);
        }
    }

    /// Вернуть атрибут и буфер вершин: следующий проход (художник egui) задаёт свои.
    pub(crate) fn unbind_vertex_positions(self, location: u32) {
        // SAFETY: контекст текущий; location получен у слинкованной программы.
        unsafe {
            self.gl.disable_vertex_attrib_array(location);
            self.gl.bind_buffer(glow::ARRAY_BUFFER, None);
        }
    }

    /// Полноэкранный треугольник: 3 вершины из привязанного буфера.
    pub(crate) fn draw_fullscreen_triangle(self) {
        // SAFETY: контекст текущий; программа и атрибут позиции привязаны
        // (`bind_vertex_positions`), буфер держит ровно 3 вершины.
        unsafe { self.gl.draw_arrays(glow::TRIANGLES, 0, 3) }
    }
}

/// Сколько байт прочитает `glTexSubImage2D` для вызова при `GL_UNPACK_ALIGNMENT = 1`:
/// шаг строки — `GL_UNPACK_ROW_LENGTH` (если задан) или ширина вызова, последняя строка —
/// только ширина вызова (OpenGL ES 2.0 §3.7.1; `EXT_unpack_subimage`).
fn gl_read_size(format: PlaneTextureFormat, call: &UploadCall) -> Result<usize, VideoGlError> {
    let overflow = VideoGlError::IntegerOverflow {
        what: "upload byte size",
    };
    let texels_per_row = call.row_length.unwrap_or(call.width);
    let to_usize = |value: u32| usize::try_from(value).map_err(|_| overflow.clone());
    let row_pitch = to_usize(texels_per_row)?
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

pub(crate) mod overlay;
pub(crate) mod probe;
#[cfg(test)]
mod tests;
