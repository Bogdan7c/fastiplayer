//! Программы оверлея: одна вершинная часть, два фрагментных варианта (маска и RGBA-картинка) —
//! перенос `subtitle_overlay.wgsl` wgpu-прохода плеера на GLSL ES 1.00.

use gles2_egui::{Es100ProgramSources, Gles2Context};

use crate::gl_calls::VideoGl;
use crate::gl_calls::overlay::OverlayVertexAttributes;
use crate::program::ShaderPrecision;

use super::api::{OverlayCanvasOrigins, OverlayLayerError};
use super::runs::OverlayLayerKind;

/// Вершинная часть: угол холста группы (`mix` двух uniform-ов по `a_group` 0/1 — углы целые,
/// поэтому смешивание с 0 или 1 даёт ровно один из них) + положение на холсте → пиксели цели →
/// NDC (ось Y вверх, как у wgpu: верх цели — `+1`). Вершинный шейдер в GLES 2.0 всегда `highp`.
const VERTEX_SHADER: &str = "#version 100
attribute vec2 a_canvas_position;
attribute vec2 a_atlas_uv;
attribute vec4 a_tint;
attribute float a_group;
uniform vec2 u_screen_size;
uniform vec2 u_main_origin;
uniform vec2 u_fixed_origin;
varying vec2 v_atlas_uv;
varying vec4 v_tint;
void main() {
    vec2 origin = mix(u_main_origin, u_fixed_origin, a_group);
    vec2 screen_pixel = origin + a_canvas_position;
    gl_Position = vec4(
        screen_pixel.x / u_screen_size.x * 2.0 - 1.0,
        1.0 - screen_pixel.y / u_screen_size.y * 2.0,
        0.0,
        1.0
    );
    v_atlas_uv = a_atlas_uv;
    v_tint = a_tint;
}
";

/// Фрагментная часть маски: `fs_main` wgpu. Выход премультиплицированный (`ONE,
/// ONE_MINUS_SRC_ALPHA`). Покрытие — `.r`: у `GL_LUMINANCE` это `L`, у `GL_RED_EXT` — `R`.
const COVERAGE_FRAGMENT_BODY: &str = "
uniform sampler2D u_atlas;
varying vec2 v_atlas_uv;
varying vec4 v_tint;
void main() {
    float coverage = texture2D(u_atlas, v_atlas_uv).r;
    float alpha = coverage * v_tint.a;
    gl_FragColor = vec4(v_tint.rgb * alpha, alpha);
}
";

/// Фрагментная часть картинки: `fs_picture` wgpu. Тексель уже премультиплицирован, `a` цвета —
/// множитель прозрачности.
const PICTURE_FRAGMENT_BODY: &str = "
uniform sampler2D u_atlas;
varying vec2 v_atlas_uv;
varying vec4 v_tint;
void main() {
    gl_FragColor = texture2D(u_atlas, v_atlas_uv) * v_tint.a;
}
";

/// Фрагментный исходник варианта с точностью драйвера.
///
/// `highp` — те же формулы `f32`, что у wgpu. Без него `mediump`: координаты атласа точны только
/// до стороны `AtlasSideLimit::MEDIUM_PRECISION_MAX_SIDE` (слой ограничивает атлас сам), цвет —
/// с ошибкой fp16 (±1/255). `sampler2D` по умолчанию `lowp` (GLSL ES 1.00 §4.5.3), но у 8-битных
/// атласов `lowp` хватает: значения — ровно коды 0..255.
fn fragment_shader_source(precision: ShaderPrecision, kind: OverlayLayerKind) -> String {
    let precision_line = match precision {
        ShaderPrecision::High => "precision highp float;",
        ShaderPrecision::Medium => "precision mediump float;",
    };
    let body = match kind {
        OverlayLayerKind::Coverage => COVERAGE_FRAGMENT_BODY,
        OverlayLayerKind::Picture => PICTURE_FRAGMENT_BODY,
    };
    format!("#version 100\n{precision_line}\n{body}")
}

/// Значения uniform-ов прохода: размер цели и углы холста групп.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct OverlayUniformValues {
    /// Размер цели в пикселях.
    pub(super) screen_size: [f32; 2],
    /// Углы холста.
    pub(super) origins: OverlayCanvasOrigins,
}

/// Слинкованная программа одного вида и её входы.
pub(super) struct OverlayProgram {
    program: glow::NativeProgram,
    attributes: OverlayVertexAttributes,
    screen_size: glow::NativeUniformLocation,
    main_origin: glow::NativeUniformLocation,
    fixed_origin: glow::NativeUniformLocation,
    /// Какие значения uniform-ов уже лежат в программе: GL хранит их в объекте программы, поэтому
    /// неизменные значения не пишутся повторно.
    applied: Option<OverlayUniformValues>,
}

impl OverlayProgram {
    /// Компилирует, линкует, находит входы, привязывает сэмплер к блоку 0.
    fn build(
        context: &Gles2Context,
        precision: ShaderPrecision,
        kind: OverlayLayerKind,
    ) -> Result<Self, OverlayLayerError> {
        let fragment = fragment_shader_source(precision, kind);
        let program = context
            .compile_es100_program(Es100ProgramSources {
                vertex: VERTEX_SHADER,
                fragment: &fragment,
            })
            .map_err(OverlayLayerError::Shader)?;
        // Программа уже создана: отказ поиска входов удаляет её, чтобы не оставить объект.
        Self::locate(context, program).inspect_err(|_| delete_program_logged(context, program))
    }

    fn locate(
        context: &Gles2Context,
        program: glow::NativeProgram,
    ) -> Result<Self, OverlayLayerError> {
        let gl = VideoGl::for_context(context).map_err(OverlayLayerError::GlAccess)?;
        let attribute = |name: &'static str| {
            gl.attribute_location(program, name)
                .ok_or(OverlayLayerError::MissingShaderInput { name })
        };
        let uniform = |name: &'static str| {
            gl.uniform_location(program, name)
                .ok_or(OverlayLayerError::MissingShaderInput { name })
        };
        let attributes = OverlayVertexAttributes {
            canvas_position: attribute("a_canvas_position")?,
            atlas_uv: attribute("a_atlas_uv")?,
            tint: attribute("a_tint")?,
            group: attribute("a_group")?,
        };
        let atlas = uniform("u_atlas")?;
        let located = Self {
            program,
            attributes,
            screen_size: uniform("u_screen_size")?,
            main_origin: uniform("u_main_origin")?,
            fixed_origin: uniform("u_fixed_origin")?,
            applied: None,
        };
        // Сэмплер смотрит в блок 0 всегда: значение живёт в программе, ставится один раз.
        gl.use_program(Some(program));
        gl.set_uniform_int(&atlas, 0);
        gl.use_program(None);
        Ok(located)
    }

    /// Сделать программу текущей и записать uniform-ы, если они изменились с прошлого раза.
    pub(super) fn activate(&mut self, gl: VideoGl<'_>, values: OverlayUniformValues) {
        gl.use_program(Some(self.program));
        if self.applied == Some(values) {
            return;
        }
        gl.set_uniform_vec2(&self.screen_size, values.screen_size);
        gl.set_uniform_vec2(&self.main_origin, values.origins.main);
        gl.set_uniform_vec2(&self.fixed_origin, values.origins.fixed);
        self.applied = Some(values);
    }

    /// Входы вершин программы.
    pub(super) const fn attributes(&self) -> OverlayVertexAttributes {
        self.attributes
    }
}

/// Программы обоих видов.
pub(super) struct OverlayPrograms {
    coverage: OverlayProgram,
    picture: OverlayProgram,
}

impl OverlayPrograms {
    /// Обе программы под точность драйвера.
    pub(super) fn build(
        context: &Gles2Context,
        precision: ShaderPrecision,
    ) -> Result<Self, OverlayLayerError> {
        let coverage = OverlayProgram::build(context, precision, OverlayLayerKind::Coverage)?;
        match OverlayProgram::build(context, precision, OverlayLayerKind::Picture) {
            Ok(picture) => Ok(Self { coverage, picture }),
            Err(error) => {
                delete_program_logged(context, coverage.program);
                Err(error)
            }
        }
    }

    /// Программа вида.
    pub(super) const fn for_kind(&mut self, kind: OverlayLayerKind) -> &mut OverlayProgram {
        match kind {
            OverlayLayerKind::Coverage => &mut self.coverage,
            OverlayLayerKind::Picture => &mut self.picture,
        }
    }

    /// Удалить обе программы.
    pub(super) fn delete(&self, context: &Gles2Context) {
        delete_program_logged(context, self.coverage.program);
        delete_program_logged(context, self.picture.program);
    }
}

fn delete_program_logged(context: &Gles2Context, program: glow::NativeProgram) {
    if let Err(error) = context.delete_program(program) {
        log::warn!("gles2-video: failed to delete overlay program: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_sources_follow_precision_and_kind() {
        let high = fragment_shader_source(ShaderPrecision::High, OverlayLayerKind::Coverage);
        assert!(high.starts_with("#version 100\nprecision highp float;"));
        assert!(high.contains("vec4(v_tint.rgb * alpha, alpha)"));
        let medium = fragment_shader_source(ShaderPrecision::Medium, OverlayLayerKind::Picture);
        assert!(medium.contains("precision mediump float;"));
        assert!(medium.contains("texture2D(u_atlas, v_atlas_uv) * v_tint.a"));
    }
}
