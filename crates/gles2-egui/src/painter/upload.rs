//! Сборка кадра egui в буферы GPU: примитивы → один вершинный и один индексный буфер + список
//! draw-call-ов ([`DrawCommand`]). Список и есть «удержанный UI-кадр»: повтор
//! (`ReusePrevious`) только проигрывает его, ничего не загружая.
//!
//! Индексы:
//! - есть `GL_OES_element_index_uint` → `u32`. Все меши лежат подряд в одном буфере, индексы
//!   сдвигаются на номер первой вершины меша (в GLES 2.0 нет `base vertex`), и атрибуты
//!   описываются один раз на кадр;
//! - нет расширения → меш режется `epaint::Mesh::split_to_u16` на куски ≤ 65 535 вершин, у
//!   каждого куска свои индексы с нуля, а атрибуты перепривязываются на начало его вершин.
//!
//! Безопасность: каждый меш проходит `Mesh::is_valid` (все индексы < числа вершин) до того, как
//! попадёт в буфер. Только поэтому [`IndexedDraw`] можно отдать в `glDrawElements` (см.
//! `draw_gl`). Невалидный меш пропускается со счётчиком.
//!
//! Без аллокаций в установившемся режиме: CPU-буферы (`staging`) и список команд переживают
//! кадры и только очищаются. Исключение — путь `u16` (`split_to_u16` выделяет куски).

use epaint::{ClippedPrimitive, Mesh, Primitive, TextureId, Vertex};

use crate::draw_gl::{GlScissor, IndexType, IndexedDraw, gl_int};

use super::scissor::{clip_rect_in_pixels, gl_scissor};

/// Размер вершины egui в буфере. `epaint::Vertex` — `#[repr(C)]`: `pos: Pos2` (2×f32),
/// `uv: Pos2` (2×f32), `color: Color32` (4×u8).
pub(super) const VERTEX_STRIDE_BYTES: usize = 20;
/// Смещение `uv` в вершине.
pub(super) const VERTEX_TEX_COORD_OFFSET: i32 = 8;
/// Смещение `color` в вершине.
pub(super) const VERTEX_COLOR_OFFSET: i32 = 16;

// Раскладка вершины — часть контракта с шейдером: если epaint её поменяет, сборка упадёт здесь.
const _: () = assert!(std::mem::size_of::<Vertex>() == VERTEX_STRIDE_BYTES);

/// Как привязаны атрибуты для команды.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VertexBinding {
    /// Общая привязка кадра с начала вершинного буфера (`u32`-индексы уже сдвинуты).
    FrameStart,
    /// Привязка на начало вершин куска (`u16`-индексы с нуля).
    AtByteOffset(i32),
}

/// Один draw-call удержанного кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DrawCommand {
    pub(super) texture_id: TextureId,
    pub(super) scissor: GlScissor,
    pub(super) vertex_binding: VertexBinding,
    pub(super) draw: IndexedDraw,
}

/// Что сборщик пропустил (для отчёта и счётчиков).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UiPrepareWork {
    /// Draw-call-ов в удержанном кадре.
    pub draw_commands: u32,
    /// Загружено байт вершин.
    pub vertex_bytes: usize,
    /// Загружено байт индексов.
    pub index_bytes: usize,
    /// `Primitive::Callback` пропущено: в GLES2-художнике колбэков рисования нет.
    pub skipped_paint_callbacks: u32,
    /// Невалидных мешей (индекс за пределами вершин) пропущено.
    pub skipped_invalid_meshes: u32,
}

/// CPU-сторона сборки: переиспользуемые буферы и список команд.
#[derive(Debug)]
pub(super) struct FrameStaging {
    index_type: IndexType,
    vertex_bytes: Vec<u8>,
    index_bytes: Vec<u8>,
    commands: Vec<DrawCommand>,
}

impl FrameStaging {
    pub(super) fn new(index_type: IndexType) -> Self {
        Self {
            index_type,
            vertex_bytes: Vec::new(),
            index_bytes: Vec::new(),
            commands: Vec::new(),
        }
    }

    pub(super) fn index_type(&self) -> IndexType {
        self.index_type
    }

    pub(super) fn vertex_bytes(&self) -> &[u8] {
        &self.vertex_bytes
    }

    pub(super) fn index_bytes(&self) -> &[u8] {
        &self.index_bytes
    }

    /// Собрать кадр. Команды прошлого кадра заменяются.
    pub(super) fn build(
        &mut self,
        primitives: Vec<ClippedPrimitive>,
        pixels_per_point: f32,
        target_size: [u32; 2],
    ) -> UiPrepareWork {
        self.vertex_bytes.clear();
        self.index_bytes.clear();
        self.commands.clear();
        let mut work = UiPrepareWork::default();
        for ClippedPrimitive {
            clip_rect,
            primitive,
        } in primitives
        {
            let mesh = match primitive {
                Primitive::Mesh(mesh) => mesh,
                Primitive::Callback(_) => {
                    work.skipped_paint_callbacks += 1;
                    continue;
                }
            };
            if mesh.indices.is_empty() {
                continue;
            }
            if !mesh.is_valid() {
                work.skipped_invalid_meshes += 1;
                continue;
            }
            // Пустая площадь отсечения — меш не нужен ни сейчас, ни при повторе.
            let Some(clip) = clip_rect_in_pixels(clip_rect, pixels_per_point, target_size) else {
                continue;
            };
            let scissor = gl_scissor(clip, target_size[1]);
            let pushed = match self.index_type {
                IndexType::U32 => self.push_u32_mesh(&mesh, scissor),
                IndexType::U16 => self.push_u16_batches(mesh, scissor),
            };
            if !pushed {
                work.skipped_invalid_meshes += 1;
            }
        }
        work.draw_commands = u32::try_from(self.commands.len()).unwrap_or(u32::MAX);
        work.vertex_bytes = self.vertex_bytes.len();
        work.index_bytes = self.index_bytes.len();
        work
    }

    /// Меш целиком с `u32`-индексами, сдвинутыми на номер его первой вершины. `false` — меш не
    /// поместился в адресацию GL (сдвиг или смещение за пределами `u32`/`GLint`).
    fn push_u32_mesh(&mut self, mesh: &Mesh, scissor: GlScissor) -> bool {
        let first_vertex = self.vertex_bytes.len() / VERTEX_STRIDE_BYTES;
        let Ok(first_vertex) = u32::try_from(first_vertex) else {
            return false;
        };
        let Ok(first_index_byte_offset) = gl_int(self.index_bytes.len(), "index byte offset")
        else {
            return false;
        };
        let Ok(index_count) = gl_int(mesh.indices.len(), "index count") else {
            return false;
        };
        // Проверить сдвиг до записи, чтобы не оставить полузаписанный меш.
        let Some(last_vertex) = u32::try_from(mesh.vertices.len())
            .ok()
            .and_then(|count| first_vertex.checked_add(count))
        else {
            return false;
        };
        if gl_int(last_vertex as usize, "vertex count").is_err() {
            return false;
        }
        self.vertex_bytes
            .extend_from_slice(bytemuck::cast_slice(&mesh.vertices));
        // `is_valid` гарантирует index < vertices.len(), поэтому сумма < last_vertex ≤ u32::MAX.
        for &index in &mesh.indices {
            self.index_bytes
                .extend_from_slice(&(first_vertex + index).to_ne_bytes());
        }
        self.commands.push(DrawCommand {
            texture_id: mesh.texture_id,
            scissor,
            vertex_binding: VertexBinding::FrameStart,
            draw: IndexedDraw {
                index_type: IndexType::U32,
                first_index_byte_offset,
                index_count,
            },
        });
        true
    }

    /// Меш кусками ≤ 65 535 вершин с собственными `u16`-индексами.
    fn push_u16_batches(&mut self, mesh: Mesh, scissor: GlScissor) -> bool {
        for batch in mesh.split_to_u16() {
            if !batch.is_valid() {
                return false;
            }
            let (Ok(vertex_byte_offset), Ok(first_index_byte_offset), Ok(index_count)) = (
                gl_int(self.vertex_bytes.len(), "vertex byte offset"),
                gl_int(self.index_bytes.len(), "index byte offset"),
                gl_int(batch.indices.len(), "index count"),
            ) else {
                return false;
            };
            self.vertex_bytes
                .extend_from_slice(bytemuck::cast_slice(&batch.vertices));
            for &index in &batch.indices {
                self.index_bytes.extend_from_slice(&index.to_ne_bytes());
            }
            self.commands.push(DrawCommand {
                texture_id: batch.texture_id,
                scissor,
                vertex_binding: VertexBinding::AtByteOffset(vertex_byte_offset),
                draw: IndexedDraw {
                    index_type: IndexType::U16,
                    first_index_byte_offset,
                    index_count,
                },
            });
        }
        true
    }

    /// Команды кадра (удержанные для повтора).
    pub(super) fn commands(&self) -> &[DrawCommand] {
        &self.commands
    }

    /// Забыть кадр (контекст потерян или текстуры пересозданы целиком).
    pub(super) fn clear_commands(&mut self) {
        self.commands.clear();
    }
}

#[cfg(test)]
mod tests {
    use epaint::{ClippedPrimitive, Color32, Mesh, Primitive, Rect, TextureId, Vertex, pos2};

    use super::{FrameStaging, VERTEX_STRIDE_BYTES, VertexBinding};
    use crate::draw_gl::IndexType;

    fn quad(texture_id: TextureId, min: f32, max: f32) -> ClippedPrimitive {
        let mut mesh = Mesh::with_texture(texture_id);
        mesh.add_rect_with_uv(
            Rect::from_min_max(pos2(min, min), pos2(max, max)),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        ClippedPrimitive {
            clip_rect: Rect::EVERYTHING,
            primitive: Primitive::Mesh(mesh),
        }
    }

    fn indices_u32(bytes: &[u8]) -> Vec<u32> {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| u32::from_ne_bytes(*chunk))
            .collect()
    }

    #[test]
    fn u32_meshes_share_one_buffer_with_rebased_indices() {
        let mut staging = FrameStaging::new(IndexType::U32);
        let work = staging.build(
            vec![
                quad(TextureId::Managed(0), 0.0, 4.0),
                quad(TextureId::Managed(1), 4.0, 8.0),
            ],
            1.0,
            [16, 16],
        );
        assert_eq!(work.draw_commands, 2);
        assert_eq!(work.vertex_bytes, 8 * VERTEX_STRIDE_BYTES);
        // Второй прямоугольник начинается с вершины 4: его индексы сдвинуты на 4.
        assert_eq!(
            indices_u32(staging.index_bytes()),
            vec![0, 1, 2, 2, 1, 3, 4, 5, 6, 6, 5, 7]
        );
        let commands = staging.commands();
        assert_eq!(commands[1].draw.first_index_byte_offset, 6 * 4);
        assert_eq!(commands[1].vertex_binding, VertexBinding::FrameStart);
        assert_eq!(commands[1].texture_id, TextureId::Managed(1));
    }

    #[test]
    fn mesh_above_u16_range_is_split_into_batches_with_own_vertex_binding() {
        // 70 000 вершин в одной полосе треугольников — больше, чем адресует u16.
        let mut mesh = Mesh::with_texture(TextureId::Managed(0));
        let vertex_count = 70_000_u32;
        for index in 0..vertex_count {
            mesh.vertices.push(Vertex {
                pos: pos2(index as f32 % 16.0, (index / 16) as f32 % 16.0),
                uv: pos2(0.0, 0.0),
                color: Color32::WHITE,
            });
        }
        for start in (0..vertex_count - 2).step_by(3) {
            mesh.add_triangle(start, start + 1, start + 2);
        }
        let mut staging = FrameStaging::new(IndexType::U16);
        let work = staging.build(
            vec![ClippedPrimitive {
                clip_rect: Rect::EVERYTHING,
                primitive: Primitive::Mesh(mesh),
            }],
            1.0,
            [16, 16],
        );
        assert!(work.draw_commands >= 2, "{work:?}");
        let commands = staging.commands();
        assert_eq!(commands[0].vertex_binding, VertexBinding::AtByteOffset(0));
        let VertexBinding::AtByteOffset(second_offset) = commands[1].vertex_binding else {
            panic!("u16 batch must bind its own vertices");
        };
        assert!(second_offset > 0);
        assert!(
            commands
                .iter()
                .all(|command| command.draw.index_type == IndexType::U16)
        );
        let total_indices: i32 = commands
            .iter()
            .map(|command| command.draw.index_count)
            .sum();
        assert_eq!(total_indices as u32, (vertex_count - 2).div_ceil(3) * 3);
    }

    #[test]
    fn invalid_meshes_and_callbacks_are_skipped_with_counters() {
        let mut broken = Mesh::with_texture(TextureId::Managed(0));
        broken.indices = vec![0, 1, 7];
        broken.vertices = vec![Vertex::default(); 3];
        let callback = ClippedPrimitive {
            clip_rect: Rect::EVERYTHING,
            primitive: Primitive::Callback(epaint::PaintCallback {
                rect: Rect::EVERYTHING,
                callback: std::sync::Arc::new(()),
            }),
        };
        let mut staging = FrameStaging::new(IndexType::U32);
        let work = staging.build(
            vec![
                ClippedPrimitive {
                    clip_rect: Rect::EVERYTHING,
                    primitive: Primitive::Mesh(broken),
                },
                callback,
                quad(TextureId::Managed(0), 0.0, 2.0),
            ],
            1.0,
            [8, 8],
        );
        assert_eq!(work.skipped_invalid_meshes, 1);
        assert_eq!(work.skipped_paint_callbacks, 1);
        assert_eq!(work.draw_commands, 1);
        assert_eq!(work.vertex_bytes, 4 * VERTEX_STRIDE_BYTES);
    }
}
