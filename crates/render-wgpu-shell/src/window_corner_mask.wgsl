// Параметры контура хранят физический размер target-а, радиус и ширину сглаживания.
struct WindowCornerUniforms {
    surface_width: f32,
    surface_height: f32,
    radius: f32,
    antialias_width: f32,
};

// Единственный uniform меняется каждый кадр вслед за resize и DPI scale.
@group(0) @binding(0) var<uniform> uniforms: WindowCornerUniforms;

// Полноэкранный треугольник не требует vertex buffer и гарантированно закрывает target.
@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    // Три вершины выходят за clip rect, чтобы избежать диагонального стыка двух triangles.
    let x = f32((vertex_index << 1u) & 2u);
    let y = f32(vertex_index & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

// Возвращает signed distance: отрицательное значение внутри rounded rectangle.
fn rounded_rectangle_distance(pixel: vec2<f32>) -> f32 {
    // Центрирование делает одну формулу симметричной для всех четырёх углов.
    let half_size = vec2<f32>(uniforms.surface_width, uniforms.surface_height) * 0.5;
    // q измеряет выход за прямоугольное ядро, уменьшенное на радиус дуги.
    let q = abs(pixel - half_size) - (half_size - vec2<f32>(uniforms.radius));
    // Внешняя евклидова часть образует четверть окружности, внутренняя сохраняет прямые края.
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - uniforms.radius;
}

// Источник содержит только coverage: blend state умножает уже готовый destination кадр.
@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    // Полоса шириной около физического пикселя даёт стабильное сглаживание на любом DPI.
    let coverage = clamp(0.5 - rounded_rectangle_distance(position.xy) / uniforms.antialias_width, 0.0, 1.0);
    // RGB намеренно нулевой: отдельные blend policies сохраняют straight RGB или умножают premultiplied RGB.
    return vec4<f32>(0.0, 0.0, 0.0, coverage);
}
