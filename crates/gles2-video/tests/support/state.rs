//! Снимок состояния GL контекста для тестов «метод не оставил следов»: привязки, флаги и число
//! живых объектов (по именам `1..=`[`NAME_SCAN_LIMIT`], их раздаёт драйвер с единицы).

#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "тестовый стенд: запросы состояния GL с SAFETY, громкие падения"
)]

use std::num::NonZeroU32;

use gles2_egui::Gles2Context;
use glow::HasContext;

/// Сколько имён объектов перебирает подсчёт живых объектов.
const NAME_SCAN_LIMIT: u32 = 512;

/// Всё, что видно из состояния GL до и после проверяемого вызова.
#[derive(Debug, PartialEq, Eq)]
pub struct GlStateSnapshot {
    pub framebuffer: Option<u32>,
    pub viewport: [i32; 4],
    pub program: Option<u32>,
    pub active_texture: i32,
    pub texture_unit0: Option<u32>,
    pub array_buffer: Option<u32>,
    pub capabilities: Vec<bool>,
    pub color_mask: [bool; 4],
    pub live_textures: usize,
    pub live_programs: usize,
    pub live_buffers: usize,
    pub live_framebuffers: usize,
}

pub fn snapshot(context: &Gles2Context) -> GlStateSnapshot {
    let gl = context.gl().expect("context is current");
    let name = |raw: NonZeroU32| raw.get();
    // SAFETY: контекст текущий (`context.gl()` сделал его таким); только запросы состояния
    // `glGet*`/`glIs*` ядра GLES 2.0, результаты пишутся в локальные значения.
    unsafe {
        let mut viewport = [0_i32; 4];
        gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
        let active_texture = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);
        gl.active_texture(glow::TEXTURE0);
        let texture_unit0 = gl
            .get_parameter_texture(glow::TEXTURE_BINDING_2D)
            .map(|t| name(t.0));
        gl.active_texture(active_texture.cast_unsigned());
        let names = || (1..=NAME_SCAN_LIMIT).filter_map(NonZeroU32::new);
        GlStateSnapshot {
            framebuffer: gl
                .get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING)
                .map(|f| name(f.0)),
            viewport,
            program: gl
                .get_parameter_program(glow::CURRENT_PROGRAM)
                .map(|p| name(p.0)),
            active_texture,
            texture_unit0,
            array_buffer: gl
                .get_parameter_buffer(glow::ARRAY_BUFFER_BINDING)
                .map(|b| name(b.0)),
            capabilities: [
                glow::BLEND,
                glow::CULL_FACE,
                glow::DITHER,
                glow::SCISSOR_TEST,
            ]
            .map(|capability| gl.is_enabled(capability))
            .to_vec(),
            color_mask: gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK),
            live_textures: names()
                .filter(|n| gl.is_texture(glow::NativeTexture(*n)))
                .count(),
            live_programs: names()
                .filter(|n| gl.is_program(glow::NativeProgram(*n)))
                .count(),
            live_buffers: names()
                .filter(|n| gl.is_buffer(glow::NativeBuffer(*n)))
                .count(),
            live_framebuffers: names()
                .filter(|n| gl.is_framebuffer(glow::NativeFramebuffer(*n)))
                .count(),
        }
    }
}
