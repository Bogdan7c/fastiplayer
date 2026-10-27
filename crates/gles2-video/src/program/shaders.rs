//! Исходники GLSL ES 1.00 видеослоя. Почему они устроены так — в документации модуля
//! [`super`]; здесь только текст.

/// Вершинный шейдер: треугольник на весь viewport; letterbox, поворот и координаты выборок
/// плоскостей — здесь, по вершинам. Все они аффинны по позиции, поэтому растеризатор
/// интерполирует их точно (до округления `f32`), а фрагментный шейдер берёт выборки по varying
/// как есть: на Ironlake любая арифметика до выборки стоит дороже самой выборки (сессия 12a).
pub(super) const VERTEX_SHADER: &str = "#version 100
precision highp float;

attribute vec2 a_position;
// xy — uv_scale, zw — uv_offset (letterbox).
uniform vec4 u_display_mapping;
uniform vec3 u_orientation_row0;
uniform vec3 u_orientation_row1;
// Координата выборки плоскости = source_uv * scale (видимый размер / размер текстуры; у
// прореженной nearest-цветности — половина яркости / размер текстуры).
uniform vec2 u_luma_uv_scale;
uniform vec2 u_chroma_uv_scale;
varying vec2 v_source_uv;
varying vec2 v_luma_uv;
varying vec2 v_chroma_uv;

void main() {
    gl_Position = vec4(a_position, 0.0, 1.0);
    // Те же значения, что у wgpu в вершинах (0,1), (2,1), (0,-1): начало — слева сверху.
    vec2 viewport_uv = vec2((a_position.x + 1.0) * 0.5, (1.0 - a_position.y) * 0.5);
    vec2 display_uv = viewport_uv * u_display_mapping.xy + u_display_mapping.zw;
    vec3 affine_input = vec3(display_uv, 1.0);
    v_source_uv = vec2(dot(u_orientation_row0, affine_input), dot(u_orientation_row1, affine_input));
    v_luma_uv = v_source_uv * u_luma_uv_scale;
    v_chroma_uv = v_source_uv * u_chroma_uv_scale;
}
";

/// Тело фрагментного шейдера 8 бит (после строк `#version`, точности и `#define` варианта).
///
/// Рисуется только внутри прямоугольника картинки (scissor), поэтому ветвления letterbox нет.
/// Яркость — билинейно по `v_luma_uv`; цветность 4:2:0 — `GL_NEAREST` по `v_chroma_uv`
/// (`floor(uv × яркость / 2)` — тексель co-sited wgpu); 4:4:4 — билинейно; у 4:2:2 фильтр
/// текстуры один на обе оси, поэтому co-sited по X считается здесь, а Y остаётся билинейным.
pub(super) const EIGHT_BIT_FRAGMENT_BODY: &str = "
uniform sampler2D u_luma_plane;
uniform sampler2D u_chroma_u_plane;
uniform sampler2D u_chroma_v_plane;

#if SHADER_EDGE_CLAMP
// Текстура шире видимой части (stride без EXT_unpack_subimage): координата упирается в центр
// последнего видимого текселя. xy — центр первого текселя, zw — центр последнего видимого.
uniform vec4 u_luma_coord_clamp;
uniform vec4 u_chroma_coord_clamp;
#endif
#if CHROMA_CO_SITED_X_IN_SHADER
// x — видимая ширина яркости, y — номер последнего видимого текселя цветности,
// z — 1 / ширина текстуры цветности.
uniform vec3 u_chroma_co_sited_x;
#endif
// Цвет: rgb = clamp(rows * (Y, U, V, 1), 0, 1).
uniform vec4 u_rgb_row0;
uniform vec4 u_rgb_row1;
uniform vec4 u_rgb_row2;

varying vec2 v_source_uv;
varying vec2 v_luma_uv;
varying vec2 v_chroma_uv;

void main() {
    vec2 luma_coord = v_luma_uv;
    vec2 chroma_coord = v_chroma_uv;
#if SHADER_EDGE_CLAMP
    luma_coord = clamp(luma_coord, u_luma_coord_clamp.xy, u_luma_coord_clamp.zw);
    chroma_coord = clamp(chroma_coord, u_chroma_coord_clamp.xy, u_chroma_coord_clamp.zw);
#endif
#if CHROMA_CO_SITED_X_IN_SHADER
    // Тексель яркости под source_uv: floor(clamp(uv) * w), не больше w - 1 (wgpu
    // source_uv_to_texel); цветность — центр текселя floor(luma_texel / 2).
    float luma_width = u_chroma_co_sited_x.x;
    float luma_texel = min(floor(clamp(v_source_uv.x, 0.0, 1.0) * luma_width), luma_width - 1.0);
    chroma_coord.x = (min(floor(luma_texel * 0.5), u_chroma_co_sited_x.y) + 0.5)
        * u_chroma_co_sited_x.z;
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
";

/// Точность выборки для 10/12 бит и таблицы HDR.
///
/// GLSL ES 1.00 §4.5.3: у `sampler2D` во фрагментном шейдере точность по умолчанию `lowp`, а
/// результат `texture2D` имеет точность сэмплера. Mesa llvmpipe реально округляет `lowp`-результат
/// (байт 254 приходил как ≈253,875, и код 12 бит терял 1/8 кода), а `highp` даёт точный `b / 255`.
/// Radeonsi и crocus `lowp` не урезают, поэтому ошибка видна только на программном рендере.
/// Оператор действует на все `sampler2D`, объявленные ниже (плоскости и атлас HDR).
pub(super) const SAMPLER_PRECISION_STATEMENT: &str = "precision highp sampler2D;\n";

/// Сборка кода из пары байтов: `младший·255 + старший·255·256`. Байт приходит в шейдер как
/// `b / 255`, поэтому умножение возвращает целое `b` (с ошибкой fp32 ~1e-5, на шесть порядков
/// меньше шага кода); `floor`/округление не нужно — дальше код только нормализуется линейно.
/// Где лежит пара, решает формат текстур кадра (`#define BYTE_PAIRS_IN_RED_GREEN`): загрузка —
/// `GL_LUMINANCE_ALPHA` (`.ra`), импорт без копии — DRM `GR88` (`.rg`, сессия 12a).
pub(super) const PLANE_CODE_FUNCTION: &str = "
const vec2 BYTE_PAIR_TO_CODE = vec2(255.0, 65280.0);

float plane_code(sampler2D plane, vec2 coord) {
#if BYTE_PAIRS_IN_RED_GREEN
    return dot(texture2D(plane, coord).rg, BYTE_PAIR_TO_CODE);
#else
    return dot(texture2D(plane, coord).ra, BYTE_PAIR_TO_CODE);
#endif
}
";

/// Тело фрагментного шейдера 10/12 бит (после `#version`, `highp`, `#define HDR_TABLE` и
/// [`PLANE_CODE_FUNCTION`]).
pub(super) const HIGH_BIT_FRAGMENT_BODY: &str = "
uniform sampler2D u_luma_plane;
uniform sampler2D u_chroma_u_plane;
uniform sampler2D u_chroma_v_plane;

// Nearest-координаты — varying v_luma_uv / v_chroma_uv. Выбор текселя (floor) делает выборка
// GL_NEAREST: для яркости floor(uv * видимая_ширина), для прореженной цветности масштаб
// (яркость / 2) / ширина текстуры, то есть floor(floor(uv * w) / 2) wgpu без вычислений на пиксель.
#if SHADER_EDGE_CLAMP
// Текстура шире видимой части: xy — центр первого текселя, zw — центр последнего видимого.
uniform vec4 u_luma_coord_clamp;
uniform vec4 u_chroma_coord_clamp;
#endif
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

varying vec2 v_luma_uv;
varying vec2 v_chroma_uv;

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
    vec2 luma_coord = v_luma_uv;
    vec2 chroma_coord = v_chroma_uv;
#if SHADER_EDGE_CLAMP
    luma_coord = clamp(luma_coord, u_luma_coord_clamp.xy, u_luma_coord_clamp.zw);
    chroma_coord = clamp(chroma_coord, u_chroma_coord_clamp.xy, u_chroma_coord_clamp.zw);
#endif
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
";

/// Тело фрагментного шейдера NV12 из DMA-BUF (после `#version` и строки точности).
///
/// Паритет с `nv12_to_rgba.wgsl` плеера: яркость и цветность читаются билинейно в ОДНОЙ
/// координате `source_uv` по всей импортированной текстуре (текстура ровно размера плоскости,
/// `CLAMP_TO_EDGE`; ни co-sited, ни зажима к видимой ширине). Цветность GR88 — `.rg` = (U, V):
/// DRM хранит `GR88` как слово little-endian `G:R`, то есть первый байт (U у NV12) лежит в R.
///
/// Под Ironlake: обе выборки идут по varying как есть — адрес не считается во фрагменте, нет
/// зависимых выборок и ветвлений по uniform-ам (урок замера сессии 05). Полос letterbox шейдер
/// не видит: рисование ограничено прямоугольником картинки (сессия 12a).
pub(super) const NV12_FRAGMENT_BODY: &str = "
uniform sampler2D u_luma_plane;
uniform sampler2D u_chroma_plane;
// Цвет: rgb = clamp(rows * (Y, U, V, 1), 0, 1) — вся SDR-цепочка одной матрицей.
uniform vec4 u_rgb_row0;
uniform vec4 u_rgb_row1;
uniform vec4 u_rgb_row2;

varying vec2 v_source_uv;

void main() {
    vec4 yuv = vec4(
        texture2D(u_luma_plane, v_source_uv).r,
        texture2D(u_chroma_plane, v_source_uv).rg,
        1.0
    );
    vec3 rgb = vec3(dot(u_rgb_row0, yuv), dot(u_rgb_row1, yuv), dot(u_rgb_row2, yuv));
    gl_FragColor = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}
";
