//! Программа художника egui: GLSL ES 1.00, ровно те же формулы, что у egui-wgpu 0.36
//! (`egui.wgsl`, вариант `fs_main_gamma_framebuffer` — наша цель не sRGB).
//!
//! - Позиции вершин в точках → NDC через размер экрана в точках (`position_from_screen`).
//! - Цвет вершины — 4 байта (premultiplied sRGB, «гамма»), нормализуются в 0..=1 атрибутом
//!   (`GL_UNSIGNED_BYTE`, normalized) — то же, что `unpack_color` (байт / 255).
//! - Фрагмент: `цвет вершины × текстура` в гамма-пространстве, без перевода в линейное.
//! - Дизеринг (решение владельца 29): шум `interleaved_gradient_noise` по координате пикселя с
//!   началом в ВЕРХНЕМ левом углу, как `@builtin(position)` в WGSL. У GL `gl_FragCoord` растёт
//!   снизу, поэтому `y = высота − gl_FragCoord.y`. Шум считается только с `highp`: формула
//!   умножает координату до ~1440 на 52,98, и в `mediump` (fp16, 10 бит мантиссы) от неё
//!   остался бы мусор.

use crate::context::Gles2Context;
use crate::draw_gl::DrawGl;
use crate::shader::Es100ProgramSources;

use super::{PainterError, UiDithering};

/// Вершинный шейдер. В вершинном шейдере GLES 2.0 `highp` обязателен (GLSL ES 1.00, 4.5.2).
const VERTEX_SHADER: &str = "#version 100
precision highp float;

uniform vec2 u_screen_size_points;

attribute vec2 a_pos;
attribute vec2 a_tex_coord;
attribute vec4 a_color;

varying vec2 v_tex_coord;
varying vec4 v_color;

void main() {
    v_tex_coord = a_tex_coord;
    v_color = a_color;
    gl_Position = vec4(
        2.0 * a_pos.x / u_screen_size_points.x - 1.0,
        1.0 - 2.0 * a_pos.y / u_screen_size_points.y,
        0.0,
        1.0);
}
";

/// Тело фрагментного шейдера (после строк `#version`, точности и `#define`).
const FRAGMENT_SHADER_BODY: &str = "
uniform sampler2D u_texture;
uniform float u_target_height_pixels;

varying vec2 v_tex_coord;
varying vec4 v_color;

#if DITHERING
float interleaved_gradient_noise(vec2 n) {
    float f = 0.06711056 * n.x + 0.00583715 * n.y;
    return fract(52.9829189 * fract(f));
}

vec3 dither_interleaved(vec3 rgb, float levels, vec2 frag_coord) {
    float noise = interleaved_gradient_noise(frag_coord);
    noise = (noise - 0.5) * 0.95;
    return rgb + noise / (levels - 1.0);
}
#endif

void main() {
    vec4 tex_gamma = texture2D(u_texture, v_tex_coord);
    vec4 out_color_gamma = v_color * tex_gamma;
#if DITHERING
    vec2 frag_coord_top_left = vec2(gl_FragCoord.x, u_target_height_pixels - gl_FragCoord.y);
    out_color_gamma.rgb = dither_interleaved(out_color_gamma.rgb, 256.0, frag_coord_top_left);
#endif
    gl_FragColor = out_color_gamma;
}
";

/// Индексы атрибутов программы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AttributeLocations {
    pub(super) position: u32,
    pub(super) tex_coord: u32,
    pub(super) color: u32,
}

/// Собранная программа и её uniform/атрибуты.
#[derive(Debug)]
pub(super) struct PainterProgram {
    pub(super) program: glow::NativeProgram,
    pub(super) attributes: AttributeLocations,
    pub(super) screen_size_points: glow::NativeUniformLocation,
    pub(super) texture_sampler: glow::NativeUniformLocation,
    /// Есть только когда дизеринг включён: иначе компилятор выкидывает неиспользуемый uniform.
    pub(super) target_height_pixels: Option<glow::NativeUniformLocation>,
    pub(super) dithering: EffectiveDithering,
}

/// Что реально стоит в шейдере.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectiveDithering {
    /// Шум как у egui-wgpu.
    Active,
    /// Выключен по просьбе вызывающего.
    DisabledByOptions,
    /// Просили, но у фрагментного шейдера нет `highp`: шум не посчитать точно.
    UnavailableWithoutHighp,
}

/// Точность фрагментного шейдера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FragmentPrecision {
    High,
    Medium,
}

impl PainterProgram {
    /// Компилирует и линкует программу, находит uniform/атрибуты.
    pub(super) fn build(
        context: &Gles2Context,
        requested_dithering: UiDithering,
    ) -> Result<Self, PainterError> {
        let precision = if context.capabilities().fragment_high_float_precision_bits() > 0 {
            FragmentPrecision::High
        } else {
            FragmentPrecision::Medium
        };
        let dithering = match (requested_dithering, precision) {
            (UiDithering::Disabled, _) => EffectiveDithering::DisabledByOptions,
            (UiDithering::MatchEguiWgpu, FragmentPrecision::High) => EffectiveDithering::Active,
            (UiDithering::MatchEguiWgpu, FragmentPrecision::Medium) => {
                EffectiveDithering::UnavailableWithoutHighp
            }
        };
        let fragment_source = fragment_shader_source(precision, dithering);
        let program = context
            .compile_es100_program(Es100ProgramSources {
                vertex: VERTEX_SHADER,
                fragment: &fragment_source,
            })
            .map_err(PainterError::Shader)?;
        match Self::locate(context, program, dithering) {
            Ok(located) => Ok(located),
            Err(error) => {
                // Программа уже создана: удалить её, чтобы отказ не оставил объект в контексте.
                if let Err(delete_error) = context.delete_program(program) {
                    log::warn!("egui painter: failed to delete rejected program: {delete_error}");
                }
                Err(error)
            }
        }
    }

    fn locate(
        context: &Gles2Context,
        program: glow::NativeProgram,
        dithering: EffectiveDithering,
    ) -> Result<Self, PainterError> {
        let gl = DrawGl::for_context(context).map_err(PainterError::GlAccess)?;
        let attribute = |name: &'static str| {
            gl.attribute_location(program, name)
                .ok_or(PainterError::MissingShaderInput { name })
        };
        let uniform = |name: &'static str| {
            gl.uniform_location(program, name)
                .ok_or(PainterError::MissingShaderInput { name })
        };
        let target_height_pixels = match dithering {
            EffectiveDithering::Active => Some(uniform("u_target_height_pixels")?),
            EffectiveDithering::DisabledByOptions | EffectiveDithering::UnavailableWithoutHighp => {
                None
            }
        };
        Ok(Self {
            program,
            attributes: AttributeLocations {
                position: attribute("a_pos")?,
                tex_coord: attribute("a_tex_coord")?,
                color: attribute("a_color")?,
            },
            screen_size_points: uniform("u_screen_size_points")?,
            texture_sampler: uniform("u_texture")?,
            target_height_pixels,
            dithering,
        })
    }
}

/// Исходник фрагментного шейдера: `#version 100` обязан быть первой строкой, поэтому точность
/// и переключатель дизеринга вставляются после неё.
fn fragment_shader_source(precision: FragmentPrecision, dithering: EffectiveDithering) -> String {
    let precision_line = match precision {
        FragmentPrecision::High => "precision highp float;",
        FragmentPrecision::Medium => "precision mediump float;",
    };
    let dithering_flag = u8::from(dithering == EffectiveDithering::Active);
    format!(
        "#version 100\n{precision_line}\n#define DITHERING {dithering_flag}\n{FRAGMENT_SHADER_BODY}"
    )
}

#[cfg(test)]
mod tests {
    use super::{EffectiveDithering, FragmentPrecision, fragment_shader_source};
    use crate::shader::declares_es100;

    #[test]
    fn fragment_source_starts_with_version_and_switches_dithering() {
        let active = fragment_shader_source(FragmentPrecision::High, EffectiveDithering::Active);
        assert!(declares_es100(&active));
        assert!(active.contains("precision highp float;"));
        assert!(active.contains("#define DITHERING 1"));

        let medium = fragment_shader_source(
            FragmentPrecision::Medium,
            EffectiveDithering::UnavailableWithoutHighp,
        );
        assert!(declares_es100(&medium));
        assert!(medium.contains("precision mediump float;"));
        assert!(medium.contains("#define DITHERING 0"));
    }
}
