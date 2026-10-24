//! Шейдеры видеослоя: планарный YUV 8 бит → RGB, letterbox, ориентация, цветокоррекция.
//!
//! Формулы — строгий паритет с `render-wgpu-video/shaders/host_yuv420_8bit_to_rgba.wgsl`:
//! - треугольник на весь viewport, `viewport_uv` с началом слева сверху (`uv.y = 0` — верхняя
//!   строка, как у wgpu: у GL clip `y = +1` — тоже верх области);
//! - `display_uv = viewport_uv × uv_scale + uv_offset`; вне `[0, 1]` — чёрный (полосы
//!   letterbox), проверка до поворота;
//! - поворот: `source_uv` = аффинное преобразование `display_uv`;
//! - яркость — билинейная выборка в `source_uv`;
//! - цветность по прореженной оси — центр текселя `floor(тексель_яркости / 2)`, то есть
//!   фактически nearest co-sited (как wgpu-хост); по непрореженной оси — билинейная, как яркость;
//! - цвет — одна аффинная матрица ([`crate::color`]) и `clamp`.
//!
//! Как это уложено под Ironlake (замер T410, сессия 05): пошаговая версия стоила ~19 мс GPU на
//! кадр 1440×900 при бюджете 16,6. По ассемблеру crocus основное время — три выборки текстур
//! по очереди (адрес каждой считался в шейдере) и ветвления по uniform-ам. Поэтому:
//! - `display_uv` и `source_uv` — аффинные функции позиции, их считает вершинный шейдер, а
//!   растеризатор интерполирует (совпадает с расчётом на пиксель до округления `f32`);
//! - прореженность цветности — свойство раскладки: по варианту программы на раскладку
//!   (`#define`), без ветвлений на пиксель;
//! - вместо делений на размер текстуры — умножение на готовые величины из uniform-ов;
//! - цвет — три скалярных произведения.
//!
//! Отличия от WGSL, вызванные GLSL ES 1.00, а не выбором:
//! - нет `textureDimensions`: размеры плоскостей приходят uniform-ами;
//! - текстура может быть шире видимой части (stride без `EXT_unpack_subimage`): координата
//!   билинейной выборки упирается в центр последнего видимого текселя. Для текстуры ровно в
//!   видимую ширину это то же самое, что `CLAMP_TO_EDGE` у wgpu, а для широкой — не даёт
//!   подмешать мусор хвоста строки;
//! - целочисленные `u32` заменены на `floor()` над `float` (точно для целых значений).
//!
//! Точность: координаты и цвет считаются в `highp`, где он есть (сессия 04: `mediump` на
//! radeonsi — настоящий fp16). Без `highp` номер текселя точен только до ширины 2048 — это
//! видит [`ShaderPrecision::exact_texel_limit`]. Во фрагментном шейдере 8 векторов uniform-ов и 3
//! сэмплера: укладывается в минимум GLES 2.0 (16 векторов, 8 текстурных блоков).

use gles2_egui::{Es100ProgramSources, Gles2Capabilities, Gles2Context};

use crate::VideoLayerError;
use crate::gl_calls::VideoGl;
use crate::planes::ChromaSubsampling;

/// Вершинный шейдер: треугольник на весь viewport; letterbox и поворот — здесь, по вершинам.
const VERTEX_SHADER: &str = "#version 100
precision highp float;

attribute vec2 a_position;
// xy — uv_scale, zw — uv_offset (letterbox).
uniform vec4 u_display_mapping;
uniform vec3 u_orientation_row0;
uniform vec3 u_orientation_row1;
varying vec2 v_display_uv;
varying vec2 v_source_uv;

void main() {
    gl_Position = vec4(a_position, 0.0, 1.0);
    // Те же значения, что у wgpu в вершинах (0,1), (2,1), (0,-1): начало — слева сверху.
    vec2 viewport_uv = vec2((a_position.x + 1.0) * 0.5, (1.0 - a_position.y) * 0.5);
    v_display_uv = viewport_uv * u_display_mapping.xy + u_display_mapping.zw;
    vec3 affine_input = vec3(v_display_uv, 1.0);
    v_source_uv = vec2(dot(u_orientation_row0, affine_input), dot(u_orientation_row1, affine_input));
}
";

/// Тело фрагментного шейдера (после строк `#version`, точности и `#define` раскладки).
const FRAGMENT_SHADER_BODY: &str = "
uniform sampler2D u_luma_plane;
uniform sampler2D u_chroma_u_plane;
uniform sampler2D u_chroma_v_plane;

// Билинейная координата яркости: clamp(source_uv * scale, min, max).
// xy — видимый размер / размер текстуры, zw — центр первого текселя (0.5 / размер текстуры).
uniform vec4 u_luma_coord_scale_min;
// xy — центр последнего видимого текселя / размер текстуры, zw — видимый размер в текселях.
uniform vec4 u_luma_coord_max_size;
// То же для цветности по непрореженной оси.
uniform vec4 u_chroma_coord_scale_min;
// xy — центр последнего видимого текселя / размер текстуры, zw — номер последнего видимого текселя.
uniform vec4 u_chroma_coord_max_last;
// 1 / размер текстуры цветности.
uniform vec2 u_chroma_texel_size;
// Цвет: rgb = clamp(rows * (Y, U, V, 1), 0, 1).
uniform vec4 u_rgb_row0;
uniform vec4 u_rgb_row1;
uniform vec4 u_rgb_row2;

varying vec2 v_display_uv;
varying vec2 v_source_uv;

void main() {
    if (v_display_uv.x < 0.0 || v_display_uv.x > 1.0 || v_display_uv.y < 0.0 || v_display_uv.y > 1.0) {
        gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
    } else {
        vec2 luma_coord = clamp(
            v_source_uv * u_luma_coord_scale_min.xy,
            u_luma_coord_scale_min.zw,
            u_luma_coord_max_size.xy
        );
        vec2 chroma_coord = clamp(
            v_source_uv * u_chroma_coord_scale_min.xy,
            u_chroma_coord_scale_min.zw,
            u_chroma_coord_max_last.xy
        );
#if CHROMA_SUBSAMPLED_X || CHROMA_SUBSAMPLED_Y
        // Тексель яркости под source_uv: floor(clamp(uv) * size), не больше size - 1 (wgpu
        // source_uv_to_texel); цветность — центр текселя floor(luma_texel / 2).
        vec2 luma_size = u_luma_coord_max_size.zw;
        vec2 luma_texel = min(floor(clamp(v_source_uv, 0.0, 1.0) * luma_size), luma_size - 1.0);
        vec2 co_sited = (min(floor(luma_texel * 0.5), u_chroma_coord_max_last.zw) + 0.5)
            * u_chroma_texel_size;
#endif
#if CHROMA_SUBSAMPLED_X
        chroma_coord.x = co_sited.x;
#endif
#if CHROMA_SUBSAMPLED_Y
        chroma_coord.y = co_sited.y;
#endif
        vec4 yuv = vec4(
            texture2D(u_luma_plane, luma_coord).r,
            texture2D(u_chroma_u_plane, chroma_coord).r,
            texture2D(u_chroma_v_plane, chroma_coord).r,
            1.0
        );
        vec3 rgb = vec3(dot(u_rgb_row0, yuv), dot(u_rgb_row1, yuv), dot(u_rgb_row2, yuv));
        gl_FragColor = vec4(clamp(rgb, 0.0, 1.0), 1.0);
    }
}
";

/// Точность фрагментного шейдера видеослоя.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderPrecision {
    /// `highp` (fp32): формулы как у wgpu.
    High,
    /// Только `mediump` (fp16): номер текселя точен до ширины 2048.
    Medium,
}

impl ShaderPrecision {
    /// Точность, которую видеослой выберет на этом драйвере: `highp`, если он есть во
    /// фрагментном шейдере.
    #[must_use]
    pub fn for_capabilities(capabilities: &Gles2Capabilities) -> Self {
        if capabilities.fragment_high_float_precision_bits() > 0 {
            Self::High
        } else {
            Self::Medium
        }
    }

    /// Самая широкая плоскость, для которой номер текселя считается точно.
    #[must_use]
    pub const fn exact_texel_limit(self) -> Option<u32> {
        match self {
            Self::High => None,
            // fp16: 11 бит мантиссы, целые точны до 2048.
            Self::Medium => Some(2048),
        }
    }
}

/// Uniform-ы программы.
#[derive(Debug)]
pub(crate) struct VideoProgramUniforms {
    pub(crate) display_mapping: glow::NativeUniformLocation,
    pub(crate) orientation_rows: [glow::NativeUniformLocation; 2],
    pub(crate) luma_coord_scale_min: glow::NativeUniformLocation,
    pub(crate) luma_coord_max_size: glow::NativeUniformLocation,
    /// Билинейная цветность — только у раскладок с непрореженной осью (4:2:2 по вертикали,
    /// 4:4:4); у 4:2:0 компилятор выкидывает uniform.
    pub(crate) chroma_coord_scale_min: Option<glow::NativeUniformLocation>,
    pub(crate) chroma_coord_max_last: glow::NativeUniformLocation,
    /// Центры текселей цветности — только у прореженных раскладок (4:2:0, 4:2:2).
    pub(crate) chroma_texel_size: Option<glow::NativeUniformLocation>,
    pub(crate) rgb_rows: [glow::NativeUniformLocation; 3],
}

/// Собранная программа одной раскладки.
#[derive(Debug)]
pub(crate) struct VideoProgram {
    pub(crate) program: glow::NativeProgram,
    pub(crate) position_attribute: u32,
    pub(crate) uniforms: VideoProgramUniforms,
}

/// Программы трёх раскладок (4:2:0, 4:2:2, 4:4:4) одного контекста.
#[derive(Debug)]
pub(crate) struct VideoPrograms {
    yuv420: VideoProgram,
    yuv422: VideoProgram,
    yuv444: VideoProgram,
    pub(crate) precision: ShaderPrecision,
}

impl VideoPrograms {
    /// Компилирует программы всех раскладок; отказ любой удаляет уже собранные.
    pub(crate) fn build(context: &Gles2Context) -> Result<Self, VideoLayerError> {
        let precision = ShaderPrecision::for_capabilities(context.capabilities());
        let yuv420 = VideoProgram::build(context, precision, ChromaSubsampling::Yuv420)?;
        let yuv422 = VideoProgram::build(context, precision, ChromaSubsampling::Yuv422)
            .inspect_err(|_| delete_program_logged(context, yuv420.program))?;
        let yuv444 = VideoProgram::build(context, precision, ChromaSubsampling::Yuv444)
            .inspect_err(|_| {
                delete_program_logged(context, yuv420.program);
                delete_program_logged(context, yuv422.program);
            })?;
        Ok(Self {
            yuv420,
            yuv422,
            yuv444,
            precision,
        })
    }

    /// Программа раскладки `chroma`.
    pub(crate) const fn for_layout(&self, chroma: ChromaSubsampling) -> &VideoProgram {
        match chroma {
            ChromaSubsampling::Yuv420 => &self.yuv420,
            ChromaSubsampling::Yuv422 => &self.yuv422,
            ChromaSubsampling::Yuv444 => &self.yuv444,
        }
    }

    /// Удалить все программы.
    pub(crate) fn delete(&self, context: &Gles2Context) {
        for program in [&self.yuv420, &self.yuv422, &self.yuv444] {
            delete_program_logged(context, program.program);
        }
    }
}

fn delete_program_logged(context: &Gles2Context, program: glow::NativeProgram) {
    if let Err(error) = context.delete_program(program) {
        log::warn!("gles2-video: failed to delete program: {error}");
    }
}

impl VideoProgram {
    /// Компилирует, линкует, находит входы и привязывает сэмплеры к блокам 0–2.
    fn build(
        context: &Gles2Context,
        precision: ShaderPrecision,
        chroma: ChromaSubsampling,
    ) -> Result<Self, VideoLayerError> {
        let fragment_source = fragment_shader_source(precision, chroma);
        let program = context
            .compile_es100_program(Es100ProgramSources {
                vertex: VERTEX_SHADER,
                fragment: &fragment_source,
            })
            .map_err(VideoLayerError::Shader)?;
        // Программа уже создана: отказ поиска входов удаляет её, чтобы не оставить объект.
        Self::locate(context, program, chroma)
            .inspect_err(|_| delete_program_logged(context, program))
    }

    fn locate(
        context: &Gles2Context,
        program: glow::NativeProgram,
        chroma: ChromaSubsampling,
    ) -> Result<Self, VideoLayerError> {
        let gl = VideoGl::for_context(context).map_err(VideoLayerError::GlAccess)?;
        let uniform = |name: &'static str| {
            gl.uniform_location(program, name)
                .ok_or(VideoLayerError::MissingShaderInput { name })
        };
        // Вход нужен этой раскладке — обязателен; не нужен — компилятор вправе его выкинуть.
        let uniform_if = |name: &'static str, needed: bool| {
            if needed {
                uniform(name).map(Some)
            } else {
                Ok(gl.uniform_location(program, name))
            }
        };
        let (subsampled_x, subsampled_y) = chroma_subsampled_axes(chroma);
        let position_attribute = gl
            .attribute_location(program, "a_position")
            .ok_or(VideoLayerError::MissingShaderInput { name: "a_position" })?;
        let samplers = [
            uniform("u_luma_plane")?,
            uniform("u_chroma_u_plane")?,
            uniform("u_chroma_v_plane")?,
        ];
        let uniforms = VideoProgramUniforms {
            display_mapping: uniform("u_display_mapping")?,
            orientation_rows: [
                uniform("u_orientation_row0")?,
                uniform("u_orientation_row1")?,
            ],
            luma_coord_scale_min: uniform("u_luma_coord_scale_min")?,
            luma_coord_max_size: uniform("u_luma_coord_max_size")?,
            chroma_coord_scale_min: uniform_if(
                "u_chroma_coord_scale_min",
                !(subsampled_x && subsampled_y),
            )?,
            chroma_coord_max_last: uniform("u_chroma_coord_max_last")?,
            chroma_texel_size: uniform_if("u_chroma_texel_size", subsampled_x || subsampled_y)?,
            rgb_rows: [
                uniform("u_rgb_row0")?,
                uniform("u_rgb_row1")?,
                uniform("u_rgb_row2")?,
            ],
        };
        // Сэмплеры привязываются к блокам один раз: значение uniform-а живёт в программе.
        gl.use_program(Some(program));
        for (unit, sampler) in (0..).zip(&samplers) {
            gl.set_uniform_int(sampler, unit);
        }
        gl.use_program(None);
        Ok(Self {
            program,
            position_attribute,
            uniforms,
        })
    }
}

/// Исходник фрагментного шейдера: `#version 100` обязан быть первой строкой, поэтому точность и
/// прореженность цветности вставляются после неё.
fn fragment_shader_source(precision: ShaderPrecision, chroma: ChromaSubsampling) -> String {
    let precision_line = match precision {
        ShaderPrecision::High => "precision highp float;",
        ShaderPrecision::Medium => "precision mediump float;",
    };
    let (subsampled_x, subsampled_y) = chroma_subsampled_axes(chroma);
    let (subsampled_x, subsampled_y) = (u8::from(subsampled_x), u8::from(subsampled_y));
    format!(
        "#version 100\n{precision_line}\n#define CHROMA_SUBSAMPLED_X {subsampled_x}\n\
         #define CHROMA_SUBSAMPLED_Y {subsampled_y}\n{FRAGMENT_SHADER_BODY}"
    )
}

/// Прорежена ли цветность по осям `(x, y)`.
const fn chroma_subsampled_axes(chroma: ChromaSubsampling) -> (bool, bool) {
    match chroma {
        ChromaSubsampling::Yuv420 => (true, true),
        ChromaSubsampling::Yuv422 => (true, false),
        ChromaSubsampling::Yuv444 => (false, false),
    }
}

/// Вершины треугольника, накрывающего весь viewport (как у wgpu): `(-1,-1)`, `(3,-1)`, `(-1,3)`.
pub(crate) const FULLSCREEN_TRIANGLE: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
