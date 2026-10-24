//! Исходники GLSL ES 1.00 видеослоя. Почему они устроены так — в документации модуля
//! [`super`]; здесь только текст.

/// Вершинный шейдер: треугольник на весь viewport; letterbox и поворот — здесь, по вершинам.
pub(super) const VERTEX_SHADER: &str = "#version 100
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

/// Тело фрагментного шейдера 8 бит (после строк `#version`, точности и `#define` раскладки).
pub(super) const EIGHT_BIT_FRAGMENT_BODY: &str = "
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

/// Сборка кода из пары байтов `GL_LUMINANCE_ALPHA`: `L·255 + A·255·256`. Байт приходит в шейдер
/// как `b / 255`, поэтому умножение возвращает целое `b` (с ошибкой fp32 ~1e-5, на шесть порядков
/// меньше шага кода); `floor`/округление не нужно — дальше код только нормализуется линейно.
pub(super) const PLANE_CODE_FUNCTION: &str = "
const vec2 BYTE_PAIR_TO_CODE = vec2(255.0, 65280.0);

float plane_code(sampler2D plane, vec2 coord) {
    return dot(texture2D(plane, coord).ra, BYTE_PAIR_TO_CODE);
}
";

/// Тело фрагментного шейдера 10/12 бит (после `#version`, `highp`, `#define HDR_TABLE` и
/// [`PLANE_CODE_FUNCTION`]).
pub(super) const HIGH_BIT_FRAGMENT_BODY: &str = "
uniform sampler2D u_luma_plane;
uniform sampler2D u_chroma_u_plane;
uniform sampler2D u_chroma_v_plane;

// Nearest-координаты: clamp(source_uv * scale, центр первого текселя, центр последнего видимого).
// Выбор текселя (floor) делает выборка GL_NEAREST: для яркости floor(uv * видимая_ширина), для
// прореженной цветности scale = (яркость / 2) / ширина текстуры, то есть floor(floor(uv * w) / 2)
// wgpu без вычислений на пиксель.
uniform vec4 u_luma_coord_scale_min;
uniform vec4 u_luma_coord_max_size;
uniform vec4 u_chroma_coord_scale_min;
uniform vec4 u_chroma_coord_max_last;
// Нормализация кодов (Y, U, V): clamp(code * scale + offset, min, max), как wgpu.
uniform vec3 u_code_scale;
uniform vec3 u_code_offset;
uniform vec3 u_normalized_min;
uniform vec3 u_normalized_max;
// Матрица: SDR — кодировка и цветокоррекция, HDR — YUV → R'G'B' BT.2020.
uniform vec4 u_rgb_row0;
uniform vec4 u_rgb_row1;
uniform vec4 u_rgb_row2;
#if HDR_TABLE
uniform sampler2D u_hdr_table;
// x — узлов на ось − 1, y — срезов в ряду атласа, z — узлов на ось.
uniform vec3 u_hdr_table_layout;
// 1 / размер атласа.
uniform vec2 u_hdr_table_texel_size;
// Значение узла = байт / 255 * x + y.
uniform vec2 u_hdr_table_decode;
#endif

varying vec2 v_display_uv;
varying vec2 v_source_uv;

#if HDR_TABLE
// Левый верхний тексель среза с номером slice в атласе.
vec2 hdr_table_slice_origin(float slice) {
    float row = floor((slice + 0.5) / u_hdr_table_layout.y);
    float column = slice - row * u_hdr_table_layout.y;
    return vec2(column, row) * u_hdr_table_layout.z;
}

// Трилинейная выборка таблицы: билинейная внутри двух соседних срезов по B + смешивание.
vec3 hdr_table_lookup(vec3 nonlinear_rgb) {
    float last_node = u_hdr_table_layout.x;
    vec3 position = nonlinear_rgb * last_node;
    float slice = min(floor(position.b), last_node - 1.0);
    float blend = position.b - slice;
    // Центры узлов: тексель + 0.5; доля между узлами — вес билинейной выборки.
    vec2 in_slice = position.rg + 0.5;
    vec3 lower = texture2D(
        u_hdr_table,
        (hdr_table_slice_origin(slice) + in_slice) * u_hdr_table_texel_size
    ).rgb;
    vec3 upper = texture2D(
        u_hdr_table,
        (hdr_table_slice_origin(slice + 1.0) + in_slice) * u_hdr_table_texel_size
    ).rgb;
    return mix(lower, upper, blend) * u_hdr_table_decode.x + u_hdr_table_decode.y;
}
#endif

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
        vec3 codes = vec3(
            plane_code(u_luma_plane, luma_coord),
            plane_code(u_chroma_u_plane, chroma_coord),
            plane_code(u_chroma_v_plane, chroma_coord)
        );
        vec4 yuv = vec4(
            clamp(codes * u_code_scale + u_code_offset, u_normalized_min, u_normalized_max),
            1.0
        );
        vec3 rgb = vec3(dot(u_rgb_row0, yuv), dot(u_rgb_row1, yuv), dot(u_rgb_row2, yuv));
#if HDR_TABLE
        rgb = hdr_table_lookup(clamp(rgb, 0.0, 1.0));
#endif
        gl_FragColor = vec4(clamp(rgb, 0.0, 1.0), 1.0);
    }
}
";
