//! Безопасные обёртки над вызовами GL, которые нужны фундаменту (сессия 03).
//!
//! Все функции `glow::HasContext` — `unsafe`: GL требует текущего контекста в потоке и
//! допустимых аргументов. Оба условия держит [`CurrentGl`]:
//! - его создаёт только `Gles2Context` сразу после успешного `eglMakeCurrent`, а сам
//!   `Gles2Context` не `Send` — контекст не уходит в другой поток;
//! - аргументы — только константы спецификации GLES 2.0 и размеры из [`SurfaceSize`].
//!
//! Художник egui (сессия 04) и видео (05+) держат свои вызовы в своих модулях безопасности.

use glow::HasContext;

use crate::context::driver::SurfaceSize;

/// Таблица функций GL контекста, текущего в этом потоке.
#[derive(Clone, Copy)]
pub(crate) struct CurrentGl<'a> {
    gl: &'a glow::Context,
}

/// Объекты offscreen-цели: FBO и текстура RGBA8 как цветовое вложение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OffscreenGlObjects {
    pub(crate) framebuffer: glow::NativeFramebuffer,
    pub(crate) color_texture: glow::NativeTexture,
}

/// Стадия шейдера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShaderStageKind {
    Vertex,
    Fragment,
}

impl ShaderStageKind {
    const fn gl_enum(self) -> u32 {
        match self {
            Self::Vertex => glow::VERTEX_SHADER,
            Self::Fragment => glow::FRAGMENT_SHADER,
        }
    }
}

/// Итог компиляции одной стадии.
pub(crate) enum CompiledShader {
    Compiled(glow::NativeShader),
    Failed { log: String },
}

/// Итог линковки.
pub(crate) enum LinkedProgram {
    Linked(glow::NativeProgram),
    Failed { log: String },
}

impl<'a> CurrentGl<'a> {
    /// Обёртка над таблицей функций. Вызывать ТОЛЬКО сразу после успешного `eglMakeCurrent`
    /// этого контекста в текущем потоке (см. документацию модуля).
    pub(crate) fn after_make_current(gl: &'a glow::Context) -> Self {
        Self { gl }
    }

    /// `glFlush`.
    pub(crate) fn flush(self) {
        // SAFETY: контекст текущий (инвариант `CurrentGl`), функция без аргументов.
        unsafe { self.gl.flush() }
    }

    /// `glFinish`.
    pub(crate) fn finish(self) {
        // SAFETY: контекст текущий (инвариант `CurrentGl`), функция без аргументов.
        unsafe { self.gl.finish() }
    }

    /// `glGetString` для `GL_VENDOR`/`GL_RENDERER`/`GL_SHADING_LANGUAGE_VERSION`.
    pub(crate) fn string(self, name: u32) -> String {
        // SAFETY: контекст текущий; вызывающий передаёт только имена строк из GLES 2.0, 6.1.5
        // (glow сам обрабатывает их как статические строки драйвера).
        unsafe { self.gl.get_parameter_string(name) }
    }

    /// `glGetIntegerv` для скалярного параметра (`GL_MAX_TEXTURE_SIZE`).
    pub(crate) fn integer(self, name: u32) -> i32 {
        // SAFETY: контекст текущий; параметр скалярный (одно значение), буфер glow на 1 элемент.
        unsafe { self.gl.get_parameter_i32(name) }
    }

    /// Бит точности `highp float` во фрагментном шейдере (0 — highp нет; GLES 2.0, 6.1.8).
    pub(crate) fn fragment_high_float_precision_bits(self) -> u32 {
        // SAFETY: контекст текущий; оба аргумента — константы спецификации.
        let format = unsafe {
            self.gl
                .get_shader_precision_format(glow::FRAGMENT_SHADER, glow::HIGH_FLOAT)
        };
        format.map_or(0, |format| u32::try_from(format.precision).unwrap_or(0))
    }

    /// `glViewport` на весь размер цели.
    pub(crate) fn viewport(self, size: SurfaceSize) {
        // SAFETY: контекст текущий; размеры положительные и ≤ i32::MAX (`SurfaceSize`).
        unsafe { self.gl.viewport(0, 0, size.width_i32(), size.height_i32()) }
    }

    /// Заливка цели цветом (`glClearColor` + `glClear(GL_COLOR_BUFFER_BIT)`).
    pub(crate) fn clear_color(self, rgba: [f32; 4]) {
        let [red, green, blue, alpha] = rgba;
        // SAFETY: контекст текущий; компоненты GL сам прижимает к [0, 1].
        unsafe {
            self.gl.clear_color(red, green, blue, alpha);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }
    }

    /// Привязывает FBO (`None` — буфер окна/surface по умолчанию).
    pub(crate) fn bind_framebuffer(self, framebuffer: Option<glow::NativeFramebuffer>) {
        // SAFETY: контекст текущий; FBO создан этим же контекстом и ещё не удалён (владелец —
        // `OffscreenTarget`).
        unsafe { self.gl.bind_framebuffer(glow::FRAMEBUFFER, framebuffer) }
    }

    /// Создаёт FBO с текстурой RGBA8 (`GL_RGBA`/`GL_UNSIGNED_BYTE` — ядро GLES 2.0, в отличие от
    /// renderbuffer RGBA8, которому нужен `GL_OES_rgb8_rgba8`).
    pub(crate) fn create_offscreen_objects(
        self,
        size: SurfaceSize,
    ) -> Result<OffscreenGlObjects, String> {
        // SAFETY: контекст текущий; создание объектов без аргументов.
        let color_texture = unsafe { self.gl.create_texture() }?;
        // SAFETY: контекст текущий; созданная выше текстура.
        let framebuffer = match unsafe { self.gl.create_framebuffer() } {
            Ok(framebuffer) => framebuffer,
            Err(error) => {
                // SAFETY: текстура создана выше этим контекстом и больше нигде не используется.
                unsafe { self.gl.delete_texture(color_texture) };
                return Err(error);
            }
        };
        let objects = OffscreenGlObjects {
            framebuffer,
            color_texture,
        };
        self.allocate_color_texture(objects.color_texture, size);
        // SAFETY: контекст текущий; FBO и текстура созданы выше; вложение уровня 0 текстуры
        // `GL_TEXTURE_2D` в `GL_COLOR_ATTACHMENT0` (GLES 2.0, 4.4.3).
        let status = unsafe {
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, Some(objects.framebuffer));
            self.gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(objects.color_texture),
                0,
            );
            self.gl.check_framebuffer_status(glow::FRAMEBUFFER)
        };
        if status == glow::FRAMEBUFFER_COMPLETE {
            Ok(objects)
        } else {
            self.delete_offscreen_objects(objects);
            Err(format!("framebuffer status {status:#06x}"))
        }
    }

    /// Новый размер текстуры offscreen-цели (содержимое не сохраняется).
    pub(crate) fn allocate_color_texture(self, texture: glow::NativeTexture, size: SurfaceSize) {
        // SAFETY: контекст текущий; текстура этого контекста; формат `GL_RGBA`/`GL_UNSIGNED_BYTE`
        // и фильтры — константы ядра GLES 2.0; данные `None` — только выделение памяти.
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            for (parameter, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
                (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                self.gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    parameter,
                    i32::try_from(value).unwrap_or_default(),
                );
            }
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                i32::try_from(glow::RGBA).unwrap_or_default(),
                size.width_i32(),
                size.height_i32(),
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            self.gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    /// Удаляет объекты offscreen-цели.
    pub(crate) fn delete_offscreen_objects(self, objects: OffscreenGlObjects) {
        // SAFETY: контекст текущий; объекты созданы этим контекстом, владелец удаляет их ровно
        // один раз. Сначала отвязываем FBO, чтобы не удалять привязанный.
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            self.gl.delete_framebuffer(objects.framebuffer);
            self.gl.delete_texture(objects.color_texture);
        }
    }

    /// `glReadPixels` всей цели в RGBA8, строки снизу вверх (как отдаёт GL).
    pub(crate) fn read_rgba_bottom_up(self, size: SurfaceSize) -> Vec<u8> {
        let byte_count = usize::try_from(size.width())
            .unwrap_or_default()
            .saturating_mul(usize::try_from(size.height()).unwrap_or_default())
            .saturating_mul(4);
        let mut pixels = vec![0_u8; byte_count];
        // SAFETY: контекст текущий; `GL_RGBA`/`GL_UNSIGNED_BYTE` — обязательная пара чтения ES 2.0
        // (4.3.1); буфер ровно ширина × высота × 4 байта, выравнивание `GL_PACK_ALIGNMENT` 4 по
        // умолчанию совпадает с шагом строки RGBA8.
        unsafe {
            self.gl.read_pixels(
                0,
                0,
                size.width_i32(),
                size.height_i32(),
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixels)),
            );
        }
        pixels
    }

    /// Компилирует одну стадию шейдера.
    pub(crate) fn compile_shader(
        self,
        stage: ShaderStageKind,
        source: &str,
    ) -> Result<CompiledShader, String> {
        // SAFETY: контекст текущий; тип стадии — константа спецификации.
        let shader = unsafe { self.gl.create_shader(stage.gl_enum()) }?;
        // SAFETY: контекст текущий; шейдер создан выше; glow передаёт длину строки явно.
        let compiled = unsafe {
            self.gl.shader_source(shader, source);
            self.gl.compile_shader(shader);
            self.gl.get_shader_compile_status(shader)
        };
        if compiled {
            Ok(CompiledShader::Compiled(shader))
        } else {
            // SAFETY: контекст текущий; шейдер этого контекста, удаляется ровно один раз здесь.
            let log = unsafe {
                let log = self.gl.get_shader_info_log(shader);
                self.gl.delete_shader(shader);
                log
            };
            Ok(CompiledShader::Failed { log })
        }
    }

    /// Линкует программу из двух скомпилированных стадий и удаляет стадии.
    pub(crate) fn link_program(
        self,
        vertex: glow::NativeShader,
        fragment: glow::NativeShader,
    ) -> Result<LinkedProgram, String> {
        // SAFETY: контекст текущий; создание программы без аргументов.
        let program = match unsafe { self.gl.create_program() } {
            Ok(program) => program,
            Err(error) => {
                self.delete_shader(vertex);
                self.delete_shader(fragment);
                return Err(error);
            }
        };
        // SAFETY: контекст текущий; программа и шейдеры этого контекста. После линковки стадии
        // отсоединяются и удаляются (программа хранит свой бинарный код).
        let linked = unsafe {
            self.gl.attach_shader(program, vertex);
            self.gl.attach_shader(program, fragment);
            self.gl.link_program(program);
            let linked = self.gl.get_program_link_status(program);
            self.gl.detach_shader(program, vertex);
            self.gl.detach_shader(program, fragment);
            self.gl.delete_shader(vertex);
            self.gl.delete_shader(fragment);
            linked
        };
        if linked {
            Ok(LinkedProgram::Linked(program))
        } else {
            // SAFETY: контекст текущий; программа этого контекста, удаляется ровно один раз.
            let log = unsafe {
                let log = self.gl.get_program_info_log(program);
                self.gl.delete_program(program);
                log
            };
            Ok(LinkedProgram::Failed { log })
        }
    }

    /// Удаляет шейдер.
    pub(crate) fn delete_shader(self, shader: glow::NativeShader) {
        // SAFETY: контекст текущий; шейдер этого контекста, вызывающий удаляет его один раз.
        unsafe { self.gl.delete_shader(shader) }
    }

    /// Удаляет программу.
    pub(crate) fn delete_program(self, program: glow::NativeProgram) {
        // SAFETY: контекст текущий; программа этого контекста, владелец удаляет её один раз.
        unsafe { self.gl.delete_program(program) }
    }
}

impl crate::fence::GpuCommandSubmission for CurrentGl<'_> {
    fn flush(&self) {
        CurrentGl::flush(*self);
    }

    fn finish(&self) {
        CurrentGl::finish(*self);
    }
}
