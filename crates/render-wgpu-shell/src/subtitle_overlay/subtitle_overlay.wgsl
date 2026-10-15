// Рисует картинки субтитров: одна маска покрытия (R8) + один цвет на инстанс.

struct OverlayUniforms {
    // Размер surface в пикселях.
    screen_size: vec2<f32>,
    // Левый верхний угол холста субтитров на экране (целые пиксели).
    canvas_origin: vec2<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: OverlayUniforms;
@group(0) @binding(1) var coverage_atlas: texture_2d<f32>;

struct InstanceInput {
    // [x, y, width, height] картинки в пикселях холста.
    @location(0) canvas_rect: vec4<f32>,
    // Левый верхний угол маски в атласе.
    @location(1) atlas_origin: vec2<u32>,
    // Прямой RGBA цвет (Unorm8x4 -> 0..1).
    @location(2) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    // Позиция внутри картинки в пикселях (0..size), интерполируется по квадрату.
    @location(0) local_pixel: vec2<f32>,
    @location(1) @interpolate(flat) atlas_origin: vec2<u32>,
    @location(2) @interpolate(flat) color: vec4<f32>,
    @location(3) @interpolate(flat) size: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32, instance: InstanceInput) -> VertexOutput {
    // Triangle strip из 4 вершин: (0,0) (1,0) (0,1) (1,1).
    let corner = vec2<f32>(f32(vertex_index & 1u), f32(vertex_index >> 1u));
    let size = instance.canvas_rect.zw;
    let screen_pixel = uniforms.canvas_origin + instance.canvas_rect.xy + corner * size;
    // Пиксели (0,0 слева сверху) -> NDC (-1..1, y вверх).
    let ndc = vec2<f32>(
        screen_pixel.x / uniforms.screen_size.x * 2.0 - 1.0,
        1.0 - screen_pixel.y / uniforms.screen_size.y * 2.0,
    );
    var output: VertexOutput;
    output.position = vec4<f32>(ndc, 0.0, 1.0);
    output.local_pixel = corner * size;
    output.atlas_origin = instance.atlas_origin;
    output.color = instance.color;
    output.size = size;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Центры пикселей экрана дают local_pixel = n + 0.5, поэтому floor — точный индекс texel-а.
    // clamp страхует от выхода на соседнюю маску в атласе на краю квадрата.
    let texel = clamp(vec2<i32>(floor(input.local_pixel)), vec2<i32>(0), vec2<i32>(input.size) - vec2<i32>(1));
    let coverage = textureLoad(coverage_atlas, vec2<i32>(input.atlas_origin) + texel, 0).r;
    let alpha = coverage * input.color.a;
    // Выход PREMULTIPLIED: pipeline смешивает One / OneMinusSrcAlpha.
    return vec4<f32>(input.color.rgb * alpha, alpha);
}
