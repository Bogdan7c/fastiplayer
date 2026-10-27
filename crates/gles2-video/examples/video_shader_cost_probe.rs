//! Стенд сессии 12a плана `user/gles2-renderer/`: сколько стоит пиксель видеопрохода на GPU.
//!
//! Один и тот же кадр 1080p (I420 и NV12, текстуры ровно размера плоскостей, загружены один
//! раз) рисуется в offscreen-цель 1440×900 с letterbox разными фрагментными шейдерами. Для
//! каждого варианта — серия проходов с одним `glFinish` в конце, итог — миллисекунды GPU на
//! проход. Варианты чередуются по кругам, чтобы частота GPU и нагрев влияли на всех одинаково.
//!
//! Варианты (`--variants a,b,…`, по умолчанию все):
//! - `planar-prod` — шейдер 8 бит из `gles2-video` как есть (координаты выборок и выбор
//!   co-sited цветности считаются на каждом пикселе, ветвление letterbox);
//! - `planar-prod-nobranch` — то же, но квад только на прямоугольник видео, полосы — `glClear`
//!   под scissor (цена ветвления);
//! - `planar-vary` — координаты из вершин: яркость билинейно по `v_source_uv` (CLAMP_TO_EDGE
//!   у текстуры ровно в видимую ширину — то же, что зажим в шейдере), цветность NEAREST по
//!   своей varying-координате (тот же индекс co-sited, что `floor` в шейдере);
//! - `planar-vary-nobranch` — то же без ветвления;
//! - `planar-vary-clamp-nobranch` — то же плюс зажим координат у края в шейдере;
//! - `nv12-prod` / `nv12-nobranch` — шейдер NV12 из `gles2-video` и он же без ветвления;
//! - `trivial` — три выборки без матрицы цвета (нижняя граница «выборки + запись»);
//! - `clear` — только `glClear` всей цели (заливка).
//!
//! Запуск: `cargo run --release -p gles2-video --example video_shader_cost_probe --
//! [--passes 240] [--rounds 3] [--variants planar-prod,planar-vary]`.

#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "стенд: сырые вызовы GL с SAFETY, громкие падения вместо обработки"
)]

use std::time::Instant;

use gles2_egui::{Es100ProgramSources, FrameStart, Gles2Context, SurfaceSize};
use glow::HasContext;

/// `GL_RED_EXT` / `GL_RG_EXT` (`GL_EXT_texture_rg`), как у `gles2-video`.
const GL_RED_EXT: u32 = 0x1903;
const GL_RG_EXT: u32 = 0x8227;

/// Цель — панель T410.
const TARGET: [u32; 2] = [1440, 900];
/// Кадр — 1080p 4:2:0.
const FRAME: [u32; 2] = [1920, 1080];

/// Вершинный шейдер `gles2-video` (letterbox и поворот аффинно в вершинах) + координата
/// цветности для вариантов `*-vary`: `v_source_uv * W / (2·cw)` даёт NEAREST-индекс
/// `floor(u·W/2)` = `floor(floor(u·W)/2)` — тот же co-sited тексель, что в шейдере.
const VERTEX_SHADER: &str = "#version 100
precision highp float;
attribute vec2 a_position;
uniform vec4 u_display_mapping;
uniform vec3 u_orientation_row0;
uniform vec3 u_orientation_row1;
uniform vec2 u_chroma_uv_scale;
varying vec2 v_display_uv;
varying vec2 v_source_uv;
varying vec2 v_chroma_uv;
void main() {
    gl_Position = vec4(a_position, 0.0, 1.0);
    vec2 viewport_uv = vec2((a_position.x + 1.0) * 0.5, (1.0 - a_position.y) * 0.5);
    v_display_uv = viewport_uv * u_display_mapping.xy + u_display_mapping.zw;
    vec3 affine_input = vec3(v_display_uv, 1.0);
    v_source_uv = vec2(dot(u_orientation_row0, affine_input), dot(u_orientation_row1, affine_input));
    v_chroma_uv = v_source_uv * u_chroma_uv_scale;
}";

/// Общая шапка фрагментных шейдеров (как `fragment_shader_source` при highp).
const FRAGMENT_HEADER: &str = "#version 100
precision highp float;
#define CHROMA_SUBSAMPLED_X 1
#define CHROMA_SUBSAMPLED_Y 1
";

/// Копия `EIGHT_BIT_FRAGMENT_BODY` (`gles2-video/src/program/shaders.rs`) с флагом ветвления.
const PLANAR_PROD_BODY: &str = "
uniform sampler2D u_luma_plane;
uniform sampler2D u_chroma_u_plane;
uniform sampler2D u_chroma_v_plane;
uniform vec4 u_luma_coord_scale_min;
uniform vec4 u_luma_coord_max_size;
uniform vec4 u_chroma_coord_scale_min;
uniform vec4 u_chroma_coord_max_last;
uniform vec2 u_chroma_texel_size;
uniform vec4 u_rgb_row0;
uniform vec4 u_rgb_row1;
uniform vec4 u_rgb_row2;
varying vec2 v_display_uv;
varying vec2 v_source_uv;
varying vec2 v_chroma_uv;
void main() {
#if LETTERBOX_BRANCH
    if (v_display_uv.x < 0.0 || v_display_uv.x > 1.0 || v_display_uv.y < 0.0 || v_display_uv.y > 1.0) {
        gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }
#endif
    vec2 luma_coord = clamp(v_source_uv * u_luma_coord_scale_min.xy, u_luma_coord_scale_min.zw,
        u_luma_coord_max_size.xy);
    vec2 luma_size = u_luma_coord_max_size.zw;
    vec2 luma_texel = min(floor(clamp(v_source_uv, 0.0, 1.0) * luma_size), luma_size - 1.0);
    vec2 chroma_coord = (min(floor(luma_texel * 0.5), u_chroma_coord_max_last.zw) + 0.5)
        * u_chroma_texel_size;
    vec4 yuv = vec4(texture2D(u_luma_plane, luma_coord).r,
        texture2D(u_chroma_u_plane, chroma_coord).r,
        texture2D(u_chroma_v_plane, chroma_coord).r, 1.0);
    vec3 rgb = vec3(dot(u_rgb_row0, yuv), dot(u_rgb_row1, yuv), dot(u_rgb_row2, yuv));
    gl_FragColor = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}";

/// Координаты из вершин: ни одной арифметики до выборок.
const PLANAR_VARY_BODY: &str = "
uniform sampler2D u_luma_plane;
uniform sampler2D u_chroma_u_plane;
uniform sampler2D u_chroma_v_plane;
uniform vec4 u_rgb_row0;
uniform vec4 u_rgb_row1;
uniform vec4 u_rgb_row2;
varying vec2 v_display_uv;
varying vec2 v_source_uv;
varying vec2 v_chroma_uv;
void main() {
#if LETTERBOX_BRANCH
    if (v_display_uv.x < 0.0 || v_display_uv.x > 1.0 || v_display_uv.y < 0.0 || v_display_uv.y > 1.0) {
        gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }
#endif
    vec4 yuv = vec4(texture2D(u_luma_plane, v_source_uv).r,
        texture2D(u_chroma_u_plane, v_chroma_uv).r,
        texture2D(u_chroma_v_plane, v_chroma_uv).r, 1.0);
    vec3 rgb = vec3(dot(u_rgb_row0, yuv), dot(u_rgb_row1, yuv), dot(u_rgb_row2, yuv));
    gl_FragColor = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}";

/// Координаты из вершин плюс зажим у края (для текстур шириной в stride).
const PLANAR_VARY_CLAMP_BODY: &str = "
uniform sampler2D u_luma_plane;
uniform sampler2D u_chroma_u_plane;
uniform sampler2D u_chroma_v_plane;
uniform vec4 u_luma_coord_scale_min;
uniform vec4 u_luma_coord_max_size;
uniform vec4 u_chroma_coord_scale_min;
uniform vec4 u_chroma_coord_max_last;
uniform vec4 u_rgb_row0;
uniform vec4 u_rgb_row1;
uniform vec4 u_rgb_row2;
varying vec2 v_display_uv;
varying vec2 v_source_uv;
varying vec2 v_chroma_uv;
void main() {
    vec2 luma_coord = clamp(v_source_uv, u_luma_coord_scale_min.zw, u_luma_coord_max_size.xy);
    vec2 chroma_coord = clamp(v_chroma_uv, u_chroma_coord_scale_min.zw, u_chroma_coord_max_last.xy);
    vec4 yuv = vec4(texture2D(u_luma_plane, luma_coord).r,
        texture2D(u_chroma_u_plane, chroma_coord).r,
        texture2D(u_chroma_v_plane, chroma_coord).r, 1.0);
    vec3 rgb = vec3(dot(u_rgb_row0, yuv), dot(u_rgb_row1, yuv), dot(u_rgb_row2, yuv));
    gl_FragColor = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}";

/// Копия `NV12_FRAGMENT_BODY` с флагом ветвления.
const NV12_BODY: &str = "
uniform sampler2D u_luma_plane;
uniform sampler2D u_chroma_plane;
uniform vec4 u_rgb_row0;
uniform vec4 u_rgb_row1;
uniform vec4 u_rgb_row2;
varying vec2 v_display_uv;
varying vec2 v_source_uv;
varying vec2 v_chroma_uv;
void main() {
#if LETTERBOX_BRANCH
    if (v_display_uv.x < 0.0 || v_display_uv.x > 1.0 || v_display_uv.y < 0.0 || v_display_uv.y > 1.0) {
        gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }
#endif
    vec4 yuv = vec4(texture2D(u_luma_plane, v_source_uv).r,
        texture2D(u_chroma_plane, v_source_uv).rg, 1.0);
    vec3 rgb = vec3(dot(u_rgb_row0, yuv), dot(u_rgb_row1, yuv), dot(u_rgb_row2, yuv));
    gl_FragColor = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}";

/// Нижняя граница: три выборки и запись, без матрицы и ветвления.
const TRIVIAL_BODY: &str = "
uniform sampler2D u_luma_plane;
uniform sampler2D u_chroma_u_plane;
uniform sampler2D u_chroma_v_plane;
varying vec2 v_display_uv;
varying vec2 v_source_uv;
varying vec2 v_chroma_uv;
void main() {
    gl_FragColor = vec4(texture2D(u_luma_plane, v_source_uv).r,
        texture2D(u_chroma_u_plane, v_chroma_uv).r, texture2D(u_chroma_v_plane, v_chroma_uv).r, 1.0);
}";

/// Чем вариант кормит шейдер.
#[derive(Clone, Copy, PartialEq)]
enum Feed {
    Planar,
    Nv12,
    ClearOnly,
}

/// Как рисуются полосы letterbox.
#[derive(Clone, Copy, PartialEq)]
enum Letterbox {
    /// Квад на всю цель, полосы — ветка шейдера (как сейчас в `gles2-video`).
    ShaderBranch,
    /// Квад только на видео, полосы — `glClear` под scissor.
    ScissorClear,
}

#[derive(Clone, Copy)]
struct Variant {
    name: &'static str,
    body: &'static str,
    feed: Feed,
    letterbox: Letterbox,
    /// Цветность NEAREST (`*-vary`) или LINEAR.
    chroma_nearest: bool,
}

const VARIANTS: [Variant; 9] = [
    Variant {
        name: "planar-prod",
        body: PLANAR_PROD_BODY,
        feed: Feed::Planar,
        letterbox: Letterbox::ShaderBranch,
        chroma_nearest: false,
    },
    Variant {
        name: "planar-prod-nobranch",
        body: PLANAR_PROD_BODY,
        feed: Feed::Planar,
        letterbox: Letterbox::ScissorClear,
        chroma_nearest: false,
    },
    Variant {
        name: "planar-vary",
        body: PLANAR_VARY_BODY,
        feed: Feed::Planar,
        letterbox: Letterbox::ShaderBranch,
        chroma_nearest: true,
    },
    Variant {
        name: "planar-vary-nobranch",
        body: PLANAR_VARY_BODY,
        feed: Feed::Planar,
        letterbox: Letterbox::ScissorClear,
        chroma_nearest: true,
    },
    Variant {
        name: "planar-vary-clamp-nobranch",
        body: PLANAR_VARY_CLAMP_BODY,
        feed: Feed::Planar,
        letterbox: Letterbox::ScissorClear,
        chroma_nearest: true,
    },
    Variant {
        name: "nv12-prod",
        body: NV12_BODY,
        feed: Feed::Nv12,
        letterbox: Letterbox::ShaderBranch,
        chroma_nearest: false,
    },
    Variant {
        name: "nv12-nobranch",
        body: NV12_BODY,
        feed: Feed::Nv12,
        letterbox: Letterbox::ScissorClear,
        chroma_nearest: false,
    },
    Variant {
        name: "trivial",
        body: TRIVIAL_BODY,
        feed: Feed::Planar,
        letterbox: Letterbox::ScissorClear,
        chroma_nearest: true,
    },
    Variant {
        name: "clear",
        body: TRIVIAL_BODY,
        feed: Feed::ClearOnly,
        letterbox: Letterbox::ScissorClear,
        chroma_nearest: true,
    },
];

struct Options {
    passes: usize,
    rounds: usize,
    variants: Vec<Variant>,
}

fn parse_options() -> Options {
    let mut options = Options {
        passes: 240,
        rounds: 3,
        variants: VARIANTS.to_vec(),
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .unwrap_or_else(|| panic!("{flag}: нет значения"))
        };
        match flag.as_str() {
            "--passes" => options.passes = value().parse().unwrap(),
            "--rounds" => options.rounds = value().parse().unwrap(),
            "--variants" => {
                let names = value();
                options.variants = names
                    .split(',')
                    .map(|name| {
                        *VARIANTS
                            .iter()
                            .find(|variant| variant.name == name)
                            .unwrap_or_else(|| panic!("неизвестный вариант {name}"))
                    })
                    .collect();
            }
            other => panic!("неизвестный флаг {other}"),
        }
    }
    options
}

/// Геометрия letterbox: видео 16:9 во всю ширину цели, полосы сверху и снизу.
struct Letterboxing {
    /// Доля высоты цели, занятая видео.
    video_height_fraction: f32,
}

impl Letterboxing {
    fn for_target() -> Self {
        let video_height = TARGET[0] as f32 * FRAME[1] as f32 / FRAME[0] as f32;
        Self {
            video_height_fraction: video_height / TARGET[1] as f32,
        }
    }

    /// Высота одной полосы в пикселях цели.
    fn bar_pixels(&self) -> i32 {
        ((1.0 - self.video_height_fraction) * TARGET[1] as f32 / 2.0).round() as i32
    }

    /// `u_display_mapping`: `viewport_uv` — координата цели (не квада), поэтому отображение
    /// одно для квада во всю цель и для квада только на видео.
    fn display_mapping(&self) -> [f32; 4] {
        let fraction = self.video_height_fraction;
        [
            1.0,
            1.0 / fraction,
            0.0,
            -(1.0 - fraction) / (2.0 * fraction),
        ]
    }
}

struct Program {
    program: glow::NativeProgram,
    position: u32,
}

fn compile(context: &Gles2Context, variant: &Variant) -> Program {
    let branch = u8::from(variant.letterbox == Letterbox::ShaderBranch);
    let fragment = format!(
        "{FRAGMENT_HEADER}#define LETTERBOX_BRANCH {branch}\n{}",
        variant.body
    );
    let program = context
        .compile_es100_program(Es100ProgramSources {
            vertex: VERTEX_SHADER,
            fragment: &fragment,
        })
        .unwrap_or_else(|error| panic!("{}: {error:?}", variant.name));
    let gl = context.gl().unwrap();
    // SAFETY: контекст текущий, программа только что собрана.
    let position = unsafe { gl.get_attrib_location(program, "a_position").unwrap() };
    Program { program, position }
}

/// Текстура плоскости ровно в её размер с узором (чтобы выборки не схлопнулись в кэше).
fn plane_texture(
    gl: &glow::Context,
    format: u32,
    channels: usize,
    size: [u32; 2],
    nearest: bool,
) -> glow::NativeTexture {
    let bytes: Vec<u8> = (0..size[0] as usize * size[1] as usize * channels)
        .map(|index| (index.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let filter = if nearest { glow::NEAREST } else { glow::LINEAR } as i32;
    // SAFETY: контекст текущий; данные покрывают всю текстуру.
    unsafe {
        let texture = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            format as i32,
            size[0] as i32,
            size[1] as i32,
            0,
            format,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(&bytes)),
        );
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, filter);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, filter);
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
        texture
    }
}

/// Все текстуры стенда: I420 (цветность в двух фильтрах) и NV12.
struct Textures {
    luma: glow::NativeTexture,
    chroma_u_linear: glow::NativeTexture,
    chroma_v_linear: glow::NativeTexture,
    chroma_u_nearest: glow::NativeTexture,
    chroma_v_nearest: glow::NativeTexture,
    nv12_chroma: glow::NativeTexture,
}

fn create_textures(gl: &glow::Context) -> Textures {
    let chroma = [FRAME[0].div_ceil(2), FRAME[1].div_ceil(2)];
    Textures {
        luma: plane_texture(gl, GL_RED_EXT, 1, FRAME, false),
        chroma_u_linear: plane_texture(gl, GL_RED_EXT, 1, chroma, false),
        chroma_v_linear: plane_texture(gl, GL_RED_EXT, 1, chroma, false),
        chroma_u_nearest: plane_texture(gl, GL_RED_EXT, 1, chroma, true),
        chroma_v_nearest: plane_texture(gl, GL_RED_EXT, 1, chroma, true),
        nv12_chroma: plane_texture(gl, GL_RG_EXT, 2, chroma, false),
    }
}

/// Uniform-ы и текстуры варианта; квад — во всю цель или только на видео.
fn prepare(
    gl: &glow::Context,
    program: &Program,
    variant: &Variant,
    textures: &Textures,
    letterboxing: &Letterboxing,
    quad: glow::NativeBuffer,
) {
    let [width, height] = FRAME.map(|value| value as f32);
    let chroma = [FRAME[0].div_ceil(2) as f32, FRAME[1].div_ceil(2) as f32];
    let quad_height = match variant.letterbox {
        Letterbox::ShaderBranch => 1.0,
        Letterbox::ScissorClear => letterboxing.video_height_fraction,
    };
    // SAFETY: контекст текущий, объекты живы.
    unsafe {
        let p = program.program;
        gl.use_program(Some(p));
        let set4 = |name: &str, value: [f32; 4]| {
            gl.uniform_4_f32(
                gl.get_uniform_location(p, name).as_ref(),
                value[0],
                value[1],
                value[2],
                value[3],
            );
        };
        set4("u_display_mapping", letterboxing.display_mapping());
        set4(
            "u_luma_coord_scale_min",
            [1.0, 1.0, 0.5 / width, 0.5 / height],
        );
        set4(
            "u_luma_coord_max_size",
            [1.0 - 0.5 / width, 1.0 - 0.5 / height, width, height],
        );
        set4(
            "u_chroma_coord_scale_min",
            [1.0, 1.0, 0.5 / chroma[0], 0.5 / chroma[1]],
        );
        set4(
            "u_chroma_coord_max_last",
            [
                1.0 - 0.5 / chroma[0],
                1.0 - 0.5 / chroma[1],
                chroma[0] - 1.0,
                chroma[1] - 1.0,
            ],
        );
        // BT.709 limited → RGB одной матрицей (значения не важны для цены, важна форма).
        set4("u_rgb_row0", [1.164, 0.0, 1.793, -0.973]);
        set4("u_rgb_row1", [1.164, -0.213, -0.533, 0.301]);
        set4("u_rgb_row2", [1.164, 2.112, 0.0, -1.133]);
        gl.uniform_3_f32(
            gl.get_uniform_location(p, "u_orientation_row0").as_ref(),
            1.0,
            0.0,
            0.0,
        );
        gl.uniform_3_f32(
            gl.get_uniform_location(p, "u_orientation_row1").as_ref(),
            0.0,
            1.0,
            0.0,
        );
        gl.uniform_2_f32(
            gl.get_uniform_location(p, "u_chroma_texel_size").as_ref(),
            1.0 / chroma[0],
            1.0 / chroma[1],
        );
        gl.uniform_2_f32(
            gl.get_uniform_location(p, "u_chroma_uv_scale").as_ref(),
            width / (2.0 * chroma[0]),
            height / (2.0 * chroma[1]),
        );
        let bound: Vec<(&str, glow::NativeTexture)> = match variant.feed {
            Feed::Nv12 => vec![
                ("u_luma_plane", textures.luma),
                ("u_chroma_plane", textures.nv12_chroma),
            ],
            _ if variant.chroma_nearest => vec![
                ("u_luma_plane", textures.luma),
                ("u_chroma_u_plane", textures.chroma_u_nearest),
                ("u_chroma_v_plane", textures.chroma_v_nearest),
            ],
            _ => vec![
                ("u_luma_plane", textures.luma),
                ("u_chroma_u_plane", textures.chroma_u_linear),
                ("u_chroma_v_plane", textures.chroma_v_linear),
            ],
        };
        for (unit, (name, texture)) in bound.iter().enumerate() {
            gl.active_texture(glow::TEXTURE0 + unit as u32);
            gl.bind_texture(glow::TEXTURE_2D, Some(*texture));
            gl.uniform_1_i32(gl.get_uniform_location(p, name).as_ref(), unit as i32);
        }
        let y = quad_height;
        let vertices: [f32; 8] = [-1.0, -y, 1.0, -y, -1.0, y, 1.0, y];
        let vertex_bytes: Vec<u8> = vertices
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect();
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(quad));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &vertex_bytes, glow::STATIC_DRAW);
        gl.enable_vertex_attrib_array(program.position);
        gl.vertex_attrib_pointer_f32(program.position, 2, glow::FLOAT, false, 0, 0);
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
    }
}

/// Один проход варианта: полосы (если нужно) и видео.
fn draw_pass(gl: &glow::Context, variant: &Variant, letterboxing: &Letterboxing) {
    // SAFETY: контекст текущий, состояние подготовлено `prepare`.
    unsafe {
        match variant.feed {
            Feed::ClearOnly => {
                gl.clear(glow::COLOR_BUFFER_BIT);
                return;
            }
            _ if variant.letterbox == Letterbox::ScissorClear => {
                let bar = letterboxing.bar_pixels();
                gl.enable(glow::SCISSOR_TEST);
                gl.scissor(0, 0, TARGET[0] as i32, bar);
                gl.clear(glow::COLOR_BUFFER_BIT);
                gl.scissor(0, TARGET[1] as i32 - bar, TARGET[0] as i32, bar);
                gl.clear(glow::COLOR_BUFFER_BIT);
                gl.disable(glow::SCISSOR_TEST);
            }
            _ => {}
        }
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
    }
}

/// Серия проходов с одним `glFinish`: миллисекунды GPU на проход.
fn measure(
    context: &mut Gles2Context,
    program: &Program,
    variant: &Variant,
    textures: &Textures,
    letterboxing: &Letterboxing,
    quad: glow::NativeBuffer,
    passes: usize,
) -> f64 {
    let FrameStart::Ready(active) = context.begin_frame().unwrap() else {
        panic!("кадр пропущен")
    };
    let gl = active.gl();
    prepare(gl, program, variant, textures, letterboxing, quad);
    // Прогрев: программа и текстуры у драйвера «горячие», первый проход не в счёт.
    for _ in 0..8 {
        draw_pass(gl, variant, letterboxing);
    }
    // SAFETY: контекст текущий.
    unsafe { gl.finish() };
    let started = Instant::now();
    for pass in 0..passes {
        draw_pass(gl, variant, letterboxing);
        if pass % 16 == 15 {
            // SAFETY: контекст текущий; без flush драйвер копил бы весь батч.
            unsafe { gl.flush() };
        }
    }
    // SAFETY: контекст текущий.
    unsafe { gl.finish() };
    let elapsed = started.elapsed();
    active.present().unwrap();
    elapsed.as_secs_f64() * 1000.0 / passes as f64
}

fn main() {
    let options = parse_options();
    let mut context =
        Gles2Context::offscreen(SurfaceSize::new(TARGET[0], TARGET[1]).unwrap()).unwrap();
    let letterboxing = Letterboxing::for_target();
    let programs: Vec<Program> = options
        .variants
        .iter()
        .map(|variant| compile(&context, variant))
        .collect();
    let (textures, quad) = {
        let gl = context.gl().unwrap();
        // SAFETY: контекст текущий.
        (create_textures(gl), unsafe { gl.create_buffer().unwrap() })
    };
    let mut results: Vec<Vec<f64>> = vec![Vec::new(); options.variants.len()];
    for _ in 0..options.rounds {
        for (index, variant) in options.variants.iter().enumerate() {
            let cost = measure(
                &mut context,
                &programs[index],
                variant,
                &textures,
                &letterboxing,
                quad,
                options.passes,
            );
            results[index].push(cost);
        }
    }
    println!(
        "target {}x{}, frame {}x{}, letterbox bar {} px, passes {} × rounds {}",
        TARGET[0],
        TARGET[1],
        FRAME[0],
        FRAME[1],
        letterboxing.bar_pixels(),
        options.passes,
        options.rounds
    );
    for (variant, costs) in options.variants.iter().zip(&results) {
        let rounds: Vec<String> = costs.iter().map(|cost| format!("{cost:.2}")).collect();
        let mean = costs.iter().sum::<f64>() / costs.len() as f64;
        println!(
            "{:<22} {:>6.2} ms/pass  (rounds: {})",
            variant.name,
            mean,
            rounds.join(" ")
        );
    }
}
