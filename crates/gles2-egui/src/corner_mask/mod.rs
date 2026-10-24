//! Маска скруглённых углов окна ([`CornerMaskRenderer`]) — паритет `window_corner_mask.wgsl`.
//!
//! Последний проход кадра (после видео, оверлеев и egui): умножает уже нарисованное на
//! аналитическое покрытие скруглённого прямоугольника (SDF, сглаживание 1 px). Радиус — точки ×
//! `pixels_per_point`, не больше половины меньшей стороны. Квадратные углы (0, NaN,
//! отрицательный радиус) — прохода нет вовсе.
//!
//! Смешивание с источником `(0, 0, 0, покрытие)` — как у wgpu:
//! - premultiplied-поверхность: `rgb·покрытие`, `a·покрытие` (цвет — `ZERO, SRC_ALPHA`);
//! - postmultiplied: `rgb` как был, `a·покрытие` (цвет — `ZERO, ONE`).
//!
//! Оптимизация для старого GPU: рисуются только 4 угловых квадрата, а не весь экран (почему
//! пиксели совпадают — `geometry`). Геометрия загружается только при смене размера или радиуса.

mod geometry;

use thiserror::Error;

use crate::context::{ContextId, GlAccessError, Gles2Context};
use crate::draw_gl::{BlendFunction, BufferTarget, DrawGl, DrawGlError, VertexAttribute};
use crate::frame::ActiveFrame;
use crate::shader::{Es100ProgramSources, ShaderError};

use self::geometry::{CORNER_MASK_VERTEX_COUNT, CornerVertex, corner_vertices, physical_radius};

const VERTEX_SHADER: &str = "#version 100
precision highp float;

attribute vec2 a_ndc;
attribute vec2 a_edge_distance;

varying vec2 v_edge_distance;

void main() {
    v_edge_distance = a_edge_distance;
    gl_Position = vec4(a_ndc, 0.0, 1.0);
}
";

/// Тело фрагментного шейдера: `q = r − расстояние до краёв` — то же, что
/// `|p − half| − (half − r)` у wgpu для пикселя в своём угловом квадрате.
///
/// Точность — `highp`, если она есть у фрагментного шейдера (T410 и 780M — да). radeonsi
/// считает `mediump` в fp16 по-настоящему: `length()` от вектора около (0,5; 11,5) ошибается там
/// на ~0,003 — почти 1/255, и пиксель края расходится с wgpu на единицу (найдено пиксельным
/// тестом). Без `highp` — `mediump` с этой оговоркой.
const FRAGMENT_SHADER_BODY: &str = "
uniform float u_radius;

varying vec2 v_edge_distance;

void main() {
    vec2 q = vec2(u_radius) - v_edge_distance;
    float distance = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - u_radius;
    float coverage = clamp(0.5 - distance / 1.0, 0.0, 1.0);
    gl_FragColor = vec4(0.0, 0.0, 0.0, coverage);
}
";

/// Исходник фрагментного шейдера с точностью по возможностям драйвера.
fn fragment_shader_source(high_precision_available: bool) -> String {
    let precision_line = if high_precision_available {
        "precision highp float;"
    } else {
        "precision mediump float;"
    };
    format!("#version 100\n{precision_line}\n{FRAGMENT_SHADER_BODY}")
}

/// Радиус скругления в точках egui. Неконечный или отрицательный — квадратные углы (как
/// `render_shell_api::WindowCornerMask::rounded_in_points`), поэтому NaN не доходит до GPU.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CornerRadius {
    points: f32,
}

impl CornerRadius {
    /// Квадратные углы.
    pub const fn square() -> Self {
        Self { points: 0.0 }
    }

    /// Скругление в точках.
    pub fn from_points(points: f32) -> Self {
        Self {
            points: if points.is_finite() {
                points.max(0.0)
            } else {
                0.0
            },
        }
    }

    /// Радиус в точках.
    pub fn points(self) -> f32 {
        self.points
    }
}

/// Как композитор понимает альфу цели.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CornerAlphaEncoding {
    /// Цвет уже умножен на альфу (Wayland, ARGB-visual X11 — так работает EGL-окно).
    Premultiplied,
    /// Цвет не умножен на альфу (поверхности, которые композитор умножает сам).
    Postmultiplied,
}

/// Итог маски.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CornerMaskOutcome {
    /// Нарисованы 4 угловых квадрата.
    Drawn {
        /// Геометрия загружалась в этом кадре (сменился размер или радиус).
        geometry_uploaded: bool,
    },
    /// Углы квадратные: прохода нет.
    SkippedSquare,
}

/// Ошибки маски.
#[derive(Debug, Error)]
pub enum CornerMaskError {
    /// Маска создана в другом контексте.
    #[error("corner mask belongs to another GLES2 context")]
    ForeignContext,
    /// Контекст недоступен.
    #[error("GLES2 context unavailable: {0}")]
    GlAccess(#[source] GlAccessError),
    /// Программа не собралась.
    #[error("corner mask shader: {0}")]
    Shader(#[source] ShaderError),
    /// В программе нет нужного входа.
    #[error("corner mask shader has no input `{name}`")]
    MissingShaderInput {
        /// Имя.
        name: &'static str,
    },
    /// Отказ GL-операции.
    #[error("corner mask GL operation: {0}")]
    Gl(#[source] DrawGlError),
}

/// Счётчики маски.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CornerMaskStats {
    /// Нарисованных масок (по одному draw-call-у).
    pub draws: u64,
    /// Загрузок геометрии.
    pub geometry_uploads: u64,
}

/// Для чего геометрия в буфере сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GeometryKey {
    target_size: [u32; 2],
    radius_bits: u32,
}

/// Маска скруглённых углов. Владеет программой и маленьким вершинным буфером в одном контексте.
#[derive(Debug)]
pub struct CornerMaskRenderer {
    owner: ContextId,
    program: glow::NativeProgram,
    ndc_attribute: u32,
    edge_distance_attribute: u32,
    radius_uniform: glow::NativeUniformLocation,
    vertex_buffer: glow::NativeBuffer,
    geometry: Option<GeometryKey>,
    stats: CornerMaskStats,
    destroyed: bool,
}

impl CornerMaskRenderer {
    /// Собирает программу и буфер.
    pub fn new(context: &Gles2Context) -> Result<Self, CornerMaskError> {
        let high_precision_available =
            context.capabilities().fragment_high_float_precision_bits() > 0;
        let fragment_source = fragment_shader_source(high_precision_available);
        let program = context
            .compile_es100_program(Es100ProgramSources {
                vertex: VERTEX_SHADER,
                fragment: &fragment_source,
            })
            .map_err(CornerMaskError::Shader)?;
        match Self::with_program(context, program) {
            Ok(renderer) => Ok(renderer),
            Err(error) => {
                if let Err(delete_error) = context.delete_program(program) {
                    log::warn!("corner mask: failed to delete rejected program: {delete_error}");
                }
                Err(error)
            }
        }
    }

    fn with_program(
        context: &Gles2Context,
        program: glow::NativeProgram,
    ) -> Result<Self, CornerMaskError> {
        let gl = DrawGl::for_context(context).map_err(CornerMaskError::GlAccess)?;
        let attribute = |name: &'static str| {
            gl.attribute_location(program, name)
                .ok_or(CornerMaskError::MissingShaderInput { name })
        };
        let ndc_attribute = attribute("a_ndc")?;
        let edge_distance_attribute = attribute("a_edge_distance")?;
        let radius_uniform = gl
            .uniform_location(program, "u_radius")
            .ok_or(CornerMaskError::MissingShaderInput { name: "u_radius" })?;
        let vertex_buffer = gl.create_buffer().map_err(CornerMaskError::Gl)?;
        Ok(Self {
            owner: context.id(),
            program,
            ndc_attribute,
            edge_distance_attribute,
            radius_uniform,
            vertex_buffer,
            geometry: None,
            stats: CornerMaskStats::default(),
            destroyed: false,
        })
    }

    /// Счётчики.
    pub fn stats(&self) -> CornerMaskStats {
        self.stats
    }

    /// Наложить маску на всю цель кадра. Вызывать последним проходом перед показом; VAO в этот
    /// момент должен быть отвязан (художник egui отвязывает свой), иначе описание атрибутов
    /// маски попало бы в чужой VAO.
    pub fn draw(
        &mut self,
        frame: &ActiveFrame<'_>,
        radius: CornerRadius,
        pixels_per_point: f32,
        encoding: CornerAlphaEncoding,
    ) -> Result<CornerMaskOutcome, CornerMaskError> {
        if frame.context_id() != self.owner {
            return Err(CornerMaskError::ForeignContext);
        }
        let size = frame.size();
        let target_size = [size.width(), size.height()];
        let Some(radius_pixels) = physical_radius(radius.points(), pixels_per_point, target_size)
        else {
            return Ok(CornerMaskOutcome::SkippedSquare);
        };
        let gl = DrawGl::for_active_frame(frame);
        gl.bind_array_buffer(Some(self.vertex_buffer));
        let geometry_uploaded = self.ensure_geometry(gl, target_size, radius_pixels)?;
        self.draw_corners(gl, target_size, radius_pixels, encoding)?;
        self.stats.draws += 1;
        Ok(CornerMaskOutcome::Drawn { geometry_uploaded })
    }

    /// Удалить GL-объекты. После потери контекста — только забыть.
    pub fn destroy(mut self, context: &Gles2Context) -> Result<(), CornerMaskError> {
        if context.id() != self.owner {
            return Err(CornerMaskError::ForeignContext);
        }
        self.destroyed = true;
        match DrawGl::for_context(context) {
            Ok(gl) => {
                gl.delete_buffer(self.vertex_buffer);
                gl.use_program(None);
                context
                    .delete_program(self.program)
                    .map_err(CornerMaskError::GlAccess)
            }
            Err(GlAccessError::ContextLost(_)) => Ok(()),
            Err(error) => Err(CornerMaskError::GlAccess(error)),
        }
    }

    /// Загрузить вершины, если размер или радиус сменились. Буфер уже привязан.
    fn ensure_geometry(
        &mut self,
        gl: DrawGl<'_>,
        target_size: [u32; 2],
        radius_pixels: f32,
    ) -> Result<bool, CornerMaskError> {
        let key = GeometryKey {
            target_size,
            radius_bits: radius_pixels.to_bits(),
        };
        if self.geometry == Some(key) {
            return Ok(false);
        }
        let vertices = corner_vertices(target_size, radius_pixels);
        gl.upload_static_bound_buffer(BufferTarget::Vertices, bytemuck::cast_slice(&vertices))
            .map_err(CornerMaskError::Gl)?;
        self.geometry = Some(key);
        self.stats.geometry_uploads += 1;
        Ok(true)
    }

    fn draw_corners(
        &self,
        gl: DrawGl<'_>,
        target_size: [u32; 2],
        radius_pixels: f32,
        encoding: CornerAlphaEncoding,
    ) -> Result<(), CornerMaskError> {
        let width = i32::try_from(target_size[0]).map_err(|_| overflow("target width"))?;
        let height = i32::try_from(target_size[1]).map_err(|_| overflow("target height"))?;
        gl.set_plain_2d_state();
        gl.disable_scissor();
        gl.viewport(width, height);
        gl.enable_blend(BlendFunction {
            source_rgb: glow::ZERO,
            destination_rgb: match encoding {
                CornerAlphaEncoding::Premultiplied => glow::SRC_ALPHA,
                CornerAlphaEncoding::Postmultiplied => glow::ONE,
            },
            source_alpha: glow::ZERO,
            destination_alpha: glow::SRC_ALPHA,
        });
        gl.use_program(Some(self.program));
        gl.set_uniform_float(&self.radius_uniform, radius_pixels);
        // 16 байт на вершину (`CornerVertex`), смещение расстояния — 8 байт.
        let stride = std::mem::size_of::<CornerVertex>() as i32;
        gl.enable_vertex_attribute(VertexAttribute {
            location: self.ndc_attribute,
            components: 2,
            component_type: glow::FLOAT,
            normalized: false,
            stride,
            byte_offset: 0,
        });
        gl.enable_vertex_attribute(VertexAttribute {
            location: self.edge_distance_attribute,
            components: 2,
            component_type: glow::FLOAT,
            normalized: false,
            stride,
            byte_offset: 8,
        });
        // 24 вершины — ровно столько, сколько загрузил `ensure_geometry`.
        gl.draw_triangles(CORNER_MASK_VERTEX_COUNT as i32);
        gl.disable_vertex_attribute(self.ndc_attribute);
        gl.disable_vertex_attribute(self.edge_distance_attribute);
        gl.disable_blend();
        gl.bind_array_buffer(None);
        gl.use_program(None);
        Ok(())
    }
}

fn overflow(what: &'static str) -> CornerMaskError {
    CornerMaskError::Gl(DrawGlError::IntegerOverflow { what })
}

impl Drop for CornerMaskRenderer {
    fn drop(&mut self) {
        if !self.destroyed {
            log::warn!(
                "corner mask dropped without destroy(): its program and buffer live until the \
                 GLES2 context is destroyed"
            );
        }
    }
}
