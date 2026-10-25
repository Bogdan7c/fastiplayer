//! Программа NV12 из DMA-BUF: две текстуры (Y и UV), общий вершинный шейдер.
//!
//! Отдельный тип, а не вариант [`super::ProgramVariant`]: у NV12 нет адресной арифметики и
//! вообще большинства uniform-ов планарных программ (размеры плоскостей, нормализация кодов,
//! таблица HDR) — держать их `Option`-ами значило бы размыть инварианты остальных программ.

use gles2_egui::{Es100ProgramSources, Gles2Context};

use super::{ShaderPrecision, delete_program_logged, shaders};
use crate::VideoLayerError;
use crate::gl_calls::VideoGl;

/// Собранная программа NV12.
#[derive(Debug)]
pub(crate) struct Nv12Program {
    pub(crate) program: glow::NativeProgram,
    pub(crate) position_attribute: u32,
    pub(crate) display_mapping: glow::NativeUniformLocation,
    pub(crate) orientation_rows: [glow::NativeUniformLocation; 2],
    pub(crate) rgb_rows: [glow::NativeUniformLocation; 3],
}

impl Nv12Program {
    /// Компилирует, линкует, находит входы и привязывает сэмплеры к блокам 0 (Y) и 1 (UV).
    pub(super) fn build(
        context: &Gles2Context,
        precision: ShaderPrecision,
    ) -> Result<Self, VideoLayerError> {
        let precision_line = match precision {
            ShaderPrecision::High => "precision highp float;",
            ShaderPrecision::Medium => "precision mediump float;",
        };
        let fragment = format!(
            "#version 100\n{precision_line}\n{}",
            shaders::NV12_FRAGMENT_BODY
        );
        let program = context
            .compile_es100_program(Es100ProgramSources {
                vertex: shaders::VERTEX_SHADER,
                fragment: &fragment,
            })
            .map_err(VideoLayerError::Shader)?;
        // Программа уже создана: отказ поиска входов удаляет её, чтобы не оставить объект.
        Self::locate(context, program).inspect_err(|_| delete_program_logged(context, program))
    }

    fn locate(
        context: &Gles2Context,
        program: glow::NativeProgram,
    ) -> Result<Self, VideoLayerError> {
        let gl = VideoGl::for_context(context).map_err(VideoLayerError::GlAccess)?;
        let uniform = |name: &'static str| {
            gl.uniform_location(program, name)
                .ok_or(VideoLayerError::MissingShaderInput { name })
        };
        let position_attribute = gl
            .attribute_location(program, "a_position")
            .ok_or(VideoLayerError::MissingShaderInput { name: "a_position" })?;
        let samplers = [
            (uniform("u_luma_plane")?, 0),
            (uniform("u_chroma_plane")?, 1),
        ];
        let found = Self {
            program,
            position_attribute,
            display_mapping: uniform("u_display_mapping")?,
            orientation_rows: [
                uniform("u_orientation_row0")?,
                uniform("u_orientation_row1")?,
            ],
            rgb_rows: [
                uniform("u_rgb_row0")?,
                uniform("u_rgb_row1")?,
                uniform("u_rgb_row2")?,
            ],
        };
        // Сэмплеры привязываются к блокам один раз: значение uniform-а живёт в программе.
        gl.use_program(Some(program));
        for (sampler, unit) in &samplers {
            gl.set_uniform_int(sampler, *unit);
        }
        gl.use_program(None);
        Ok(found)
    }
}
