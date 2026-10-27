//! Проигрывание удержанного кадра на GPU: состояние конвейера egui, раскладка вершин,
//! draw-call-ы, создание буферов. Вынесено из `painter/mod.rs`: это одна связная задача
//! «нарисовать список команд», отдельная от жизненного цикла кадра (текстуры, сборка, `free`).

use crate::draw_gl::{BlendFunction, DrawGl, DrawGlError, VertexAttribute};

use super::scissor::{PaintScissorLimit, intersect_scissors};
use super::upload::{
    DrawCommand, VERTEX_COLOR_OFFSET, VERTEX_STRIDE_BYTES, VERTEX_TEX_COORD_OFFSET, VertexBinding,
};
use super::{EguiPainter, UiPaintWork, UiScreen};

impl EguiPainter {
    /// Состояние конвейера художника: программа, смешивание egui, вершины.
    pub(super) fn begin_paint_state(
        &self,
        gl: DrawGl<'_>,
        screen: UiScreen,
        width: i32,
        height: i32,
    ) {
        gl.set_plain_2d_state();
        gl.viewport(width, height);
        gl.enable_blend(BlendFunction {
            source_rgb: glow::ONE,
            destination_rgb: glow::ONE_MINUS_SRC_ALPHA,
            source_alpha: glow::ONE_MINUS_DST_ALPHA,
            destination_alpha: glow::ONE,
        });
        gl.use_program(Some(self.program.program));
        gl.set_uniform_vec2(&self.program.screen_size_points, screen.size_in_points());
        gl.set_uniform_int(&self.program.texture_sampler, 0);
        if let Some(target_height) = &self.program.target_height_pixels {
            gl.set_uniform_float(target_height, height as f32);
        }
        match self.vertex_array {
            Some(vertex_array) => gl.bind_vertex_array(Some(vertex_array)),
            None => {
                gl.bind_array_buffer(Some(self.vertex_buffer));
                gl.bind_element_buffer(Some(self.index_buffer));
                self.bind_frame_vertex_layout(gl, 0);
            }
        }
    }

    /// Проиграть команды удержанного кадра.
    ///
    /// `limit` — область перерисовки кадра: scissor каждой команды пересекается с ней, команды без
    /// пересечения пропускаются (`skipped_outside_repaint`). Так художник не меняет пиксели вне
    /// области частичной перерисовки.
    pub(super) fn draw_commands(&self, gl: DrawGl<'_>, limit: &PaintScissorLimit) -> UiPaintWork {
        let mut work = UiPaintWork::default();
        let mut bound_texture: Option<glow::NativeTexture> = None;
        let mut bound_vertex_offset = 0;
        for command in self.staging.commands() {
            let scissor = match limit {
                PaintScissorLimit::Unrestricted => command.scissor,
                PaintScissorLimit::Region(region) => {
                    match intersect_scissors(command.scissor, *region) {
                        Some(scissor) => scissor,
                        None => {
                            work.skipped_outside_repaint += 1;
                            continue;
                        }
                    }
                }
                PaintScissorLimit::Nothing => {
                    work.skipped_outside_repaint += 1;
                    continue;
                }
            };
            let Some(texture) = self.textures.gl_texture(command.texture_id) else {
                work.skipped_missing_texture += 1;
                continue;
            };
            self.bind_command_vertices(gl, command, &mut bound_vertex_offset);
            if bound_texture != Some(texture) {
                gl.bind_texture_unit0(Some(texture));
                bound_texture = Some(texture);
                work.texture_binds += 1;
            }
            gl.enable_scissor(scissor);
            gl.draw_indexed_triangles(command.draw);
            work.draw_calls += 1;
        }
        if work.skipped_missing_texture > 0 {
            log::warn!(
                "egui painter: {} mesh(es) skipped: texture missing",
                work.skipped_missing_texture
            );
        }
        work
    }

    /// Батч `u16` смотрит на свои вершины: атрибуты перепривязываются, только если смещение
    /// сменилось.
    pub(super) fn bind_command_vertices(
        &self,
        gl: DrawGl<'_>,
        command: &DrawCommand,
        bound_offset: &mut i32,
    ) {
        let offset = match command.vertex_binding {
            VertexBinding::FrameStart => 0,
            VertexBinding::AtByteOffset(offset) => offset,
        };
        if offset != *bound_offset {
            self.bind_frame_vertex_layout(gl, offset);
            *bound_offset = offset;
        }
    }

    /// Вернуть состояние, которое не должно протечь в чужие проходы.
    pub(super) fn end_paint_state(&self, gl: DrawGl<'_>) {
        gl.disable_scissor();
        gl.bind_texture_unit0(None);
        match self.vertex_array {
            Some(vertex_array) => {
                // VAO у «u16»-пути могли перепривязать на смещение: вернуть к началу, чтобы
                // следующий кадр начинался с известного состояния.
                gl.bind_vertex_array(Some(vertex_array));
                self.bind_frame_vertex_layout(gl, 0);
                gl.bind_vertex_array(None);
            }
            None => {
                let attributes = self.program.attributes;
                for location in [attributes.position, attributes.tex_coord, attributes.color] {
                    gl.disable_vertex_attribute(location);
                }
            }
        }
        gl.bind_array_buffer(None);
        gl.use_program(None);
    }

    /// Описать атрибуты вершины egui относительно своего вершинного буфера со сдвигом
    /// `vertex_byte_offset` (0 у `u32`-пути). Привязывает свои буферы.
    pub(super) fn bind_frame_vertex_layout(&self, gl: DrawGl<'_>, vertex_byte_offset: i32) {
        gl.bind_array_buffer(Some(self.vertex_buffer));
        gl.bind_element_buffer(Some(self.index_buffer));
        let attributes = self.program.attributes;
        // 20 байт — константа раскладки `epaint::Vertex`, в `GLsizei` помещается.
        let stride = VERTEX_STRIDE_BYTES as i32;
        let layout = [
            (attributes.position, 2, glow::FLOAT, false, 0),
            (
                attributes.tex_coord,
                2,
                glow::FLOAT,
                false,
                VERTEX_TEX_COORD_OFFSET,
            ),
            // Цвет — 4 байта, нормализуются в 0..=1 (как `unpack_color` у egui-wgpu).
            (
                attributes.color,
                4,
                glow::UNSIGNED_BYTE,
                true,
                VERTEX_COLOR_OFFSET,
            ),
        ];
        for (location, components, component_type, normalized, attribute_offset) in layout {
            gl.enable_vertex_attribute(VertexAttribute {
                location,
                components,
                component_type,
                normalized,
                stride,
                byte_offset: vertex_byte_offset + attribute_offset,
            });
        }
    }
}

/// VBO, IBO и (если есть расширение) VAO. При отказе уже созданное удаляется.
pub(super) fn create_buffer_objects(
    gl: DrawGl<'_>,
    vertex_array_supported: bool,
) -> Result<
    (
        glow::NativeBuffer,
        glow::NativeBuffer,
        Option<glow::NativeVertexArray>,
    ),
    DrawGlError,
> {
    let vertex_buffer = gl.create_buffer()?;
    let index_buffer = match gl.create_buffer() {
        Ok(buffer) => buffer,
        Err(error) => {
            gl.delete_buffer(vertex_buffer);
            return Err(error);
        }
    };
    let vertex_array = if vertex_array_supported {
        match gl.create_vertex_array() {
            Ok(vertex_array) => Some(vertex_array),
            Err(error) => {
                gl.delete_buffer(vertex_buffer);
                gl.delete_buffer(index_buffer);
                return Err(error);
            }
        }
    } else {
        None
    };
    Ok((vertex_buffer, index_buffer, vertex_array))
}
