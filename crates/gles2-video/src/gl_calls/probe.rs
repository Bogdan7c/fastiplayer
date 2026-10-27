//! Вызовы GL самопроверки видимости записи CPU ([`crate::probe_cpu_write_visibility`]) — часть
//! модуля безопасности [`super`]: те же инварианты [`VideoGl`] (контекст текущий в этом потоке,
//! объекты созданы им).
//!
//! Самопроверка идёт ВНЕ кадра показа, посреди состояния, которое оставил прошлый кадр и которое
//! ждёт следующий. Поэтому всё, что проверка меняет, сохраняется [`VideoGl::save_probe_state`] и
//! возвращается [`VideoGl::restore_probe_state`].

use glow::HasContext;

use super::{VideoGl, VideoGlError, gl_int};

/// Возможности GL, которые проверка выключает на время рисования (и возвращает как были):
/// смешивание, отбраковка, дизеринг и scissor меняют записанный в цель пиксель.
const SWITCHED_OFF_CAPABILITIES: [u32; 4] = [
    glow::BLEND,
    glow::CULL_FACE,
    glow::DITHER,
    glow::SCISSOR_TEST,
];

/// Состояние GL, которое трогает самопроверка.
#[derive(Debug)]
pub(crate) struct ProbeSavedState {
    framebuffer: Option<glow::NativeFramebuffer>,
    viewport: [i32; 4],
    program: Option<glow::NativeProgram>,
    active_texture_unit: u32,
    /// Привязка `GL_TEXTURE_2D` блока 0: импорт плоскости (`attach_dma_buf_plane_to_texture`)
    /// оставляет блок без привязки, а проверка рисует через блок 0.
    texture_unit0: Option<glow::NativeTexture>,
    array_buffer: Option<glow::NativeBuffer>,
    capabilities: [(u32, bool); 4],
    color_mask: [bool; 4],
}

/// Цель рисования проверки: текстура RGBA8 в собственном FBO.
#[derive(Debug)]
pub(crate) struct ProbeRenderTarget {
    framebuffer: glow::NativeFramebuffer,
    texture: glow::NativeTexture,
    side: u32,
}

impl VideoGl<'_> {
    /// Запомнить состояние, которое изменит самопроверка.
    pub(crate) fn save_probe_state(self) -> ProbeSavedState {
        // SAFETY: контекст текущий; запросы состояния `glGet*`/`glIsEnabled` ядра GLES 2.0 пишут
        // только в локальные значения (`viewport` — массив ровно из четырёх `GLint`, как требует
        // `GL_VIEWPORT`).
        unsafe {
            let mut viewport = [0_i32; 4];
            self.gl
                .get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            let active_texture_unit = self
                .gl
                .get_parameter_i32(glow::ACTIVE_TEXTURE)
                .cast_unsigned();
            // Привязку читаем у блока 0, затем возвращаем активным прежний блок.
            self.gl.active_texture(glow::TEXTURE0);
            let texture_unit0 = self.gl.get_parameter_texture(glow::TEXTURE_BINDING_2D);
            self.gl.active_texture(active_texture_unit);
            ProbeSavedState {
                framebuffer: self.gl.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING),
                viewport,
                program: self.gl.get_parameter_program(glow::CURRENT_PROGRAM),
                active_texture_unit,
                texture_unit0,
                array_buffer: self.gl.get_parameter_buffer(glow::ARRAY_BUFFER_BINDING),
                capabilities: SWITCHED_OFF_CAPABILITIES
                    .map(|capability| (capability, self.gl.is_enabled(capability))),
                color_mask: self.gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK),
            }
        }
    }

    /// Вернуть состояние, сохранённое [`Self::save_probe_state`]. Атрибут вершин проверки
    /// выключается её же вызовом `unbind_vertex_positions`; указатель атрибута не возвращается:
    /// его задаёт каждый проход рисования сам (контракт слоя кадра).
    pub(crate) fn restore_probe_state(self, saved: &ProbeSavedState) {
        // SAFETY: контекст текущий; объекты в `saved` созданы этим же контекстом и не удалены
        // самопроверкой; остальное — константы ядра GLES 2.0.
        unsafe {
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, saved.framebuffer);
            let [x, y, width, height] = saved.viewport;
            self.gl.viewport(x, y, width, height);
            self.gl.use_program(saved.program);
            self.gl.bind_buffer(glow::ARRAY_BUFFER, saved.array_buffer);
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, saved.texture_unit0);
            self.gl.active_texture(saved.active_texture_unit);
            for (capability, was_enabled) in saved.capabilities {
                if was_enabled {
                    self.gl.enable(capability);
                } else {
                    self.gl.disable(capability);
                }
            }
            let [red, green, blue, alpha] = saved.color_mask;
            self.gl.color_mask(red, green, blue, alpha);
        }
    }

    /// Цель рисования `side × side`: текстура RGBA8 (цветовой формат, обязательный для
    /// рендера в текстуру в GLES 2.0, §4.4.5) в своём FBO. Отказ удаляет созданное.
    pub(crate) fn create_probe_render_target(
        self,
        side: u32,
    ) -> Result<ProbeRenderTarget, VideoGlError> {
        let size = gl_int(side, "probe target side")?;
        let texture = self.create_texture()?;
        // SAFETY: контекст текущий; `glGenFramebuffers` без указателей.
        let created = unsafe { self.gl.create_framebuffer() };
        let framebuffer = match created {
            Ok(framebuffer) => framebuffer,
            Err(message) => {
                self.delete_texture(texture);
                return Err(VideoGlError::ObjectCreation { message });
            }
        };
        let target = ProbeRenderTarget {
            framebuffer,
            texture,
            side,
        };
        // SAFETY: контекст текущий; объекты только что созданы им; данных нет (`None`) — драйвер
        // лишь выделяет память `size × size × 4`; фильтр NEAREST и CLAMP делают текстуру полной
        // без мипмапов (§3.7.10).
        let status = unsafe {
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            for (parameter, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
                (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                self.gl
                    .tex_parameter_i32(glow::TEXTURE_2D, parameter, value.cast_signed());
            }
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA.cast_signed(),
                size,
                size,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            self.gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            self.gl.check_framebuffer_status(glow::FRAMEBUFFER)
        };
        if status != glow::FRAMEBUFFER_COMPLETE {
            self.delete_probe_render_target(target);
            return Err(VideoGlError::FramebufferIncomplete { status });
        }
        Ok(target)
    }

    pub(crate) fn delete_probe_render_target(self, target: ProbeRenderTarget) {
        // SAFETY: контекст текущий; объекты созданы этим контекстом и больше не используются
        // (удаление привязанного FBO/текстуры безопасно: приложение C.1 спецификации GLES 2.0).
        unsafe {
            self.gl.delete_framebuffer(target.framebuffer);
            self.gl.delete_texture(target.texture);
        }
    }

    /// Нарисовать полноэкранный треугольник в цель проверки, читая `source` (текстура блока 0).
    /// Программа проверки уже слинкована, её сэмплер указывает на блок 0.
    pub(crate) fn draw_probe_sample(
        self,
        target: &ProbeRenderTarget,
        program: glow::NativeProgram,
        position_attribute: u32,
        vertices: glow::NativeBuffer,
        source: glow::NativeTexture,
    ) {
        let side = i32::try_from(target.side).unwrap_or(i32::MAX);
        // SAFETY: контекст текущий; все объекты созданы им; FBO полон (проверено при создании);
        // буфер вершин держит три вершины (`upload_static_vertices`).
        unsafe {
            for capability in SWITCHED_OFF_CAPABILITIES {
                self.gl.disable(capability);
            }
            self.gl.color_mask(true, true, true, true);
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, Some(target.framebuffer));
            self.gl.viewport(0, 0, side, side);
            self.gl.use_program(Some(program));
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(source));
        }
        self.bind_vertex_positions(vertices, position_attribute);
        self.draw_fullscreen_triangle();
        self.unbind_vertex_positions(position_attribute);
    }

    /// Красный канал пикселя (0, 0) цели проверки. `glReadPixels` дожидается завершения
    /// рисования, поэтому значение уже отражает то, что GPU прочитал из памяти буфера.
    pub(crate) fn read_probe_red(self, target: &ProbeRenderTarget) -> u8 {
        let mut pixel = [0_u8; 4];
        // SAFETY: контекст текущий; FBO цели привязан и полон; `pixel` — ровно 4 байта для одного
        // пикселя RGBA/UNSIGNED_BYTE при `GL_PACK_ALIGNMENT` ≤ 4 (строка из одного пикселя).
        unsafe {
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, Some(target.framebuffer));
            self.gl.read_pixels(
                0,
                0,
                1,
                1,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixel)),
            );
        }
        pixel[0]
    }
}
