//! Шейдеры строго GLSL ES 1.00 (`#version 100`).
//!
//! Страховка от ловушки разведки 00: на 780M и llvmpipe контекст ES 3.2, и шейдер
//! `#version 300 es` там скомпилируется, а на T410 (только ES 2.0) — нет. Поэтому версия
//! проверяется до драйвера: всё, что не `#version 100`, отклоняется [`ShaderError::NotEs100`]
//! на любой машине одинаково.

use crate::context::safety::gl::{CompiledShader, CurrentGl, LinkedProgram, ShaderStageKind};

/// Исходники программы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Es100ProgramSources<'a> {
    /// Вершинный шейдер.
    pub vertex: &'a str,
    /// Фрагментный шейдер.
    pub fragment: &'a str,
}

/// Стадия шейдера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderStage {
    /// Вершинный.
    Vertex,
    /// Фрагментный.
    Fragment,
}

/// Почему программа не собрана.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShaderError {
    /// Первая строка не `#version 100`.
    #[error("{stage:?} shader must start with `#version 100`")]
    NotEs100 {
        /// Стадия.
        stage: ShaderStage,
    },
    /// Драйвер не скомпилировал стадию.
    #[error("{stage:?} shader failed to compile: {log}")]
    Compile {
        /// Стадия.
        stage: ShaderStage,
        /// Журнал компилятора.
        log: String,
    },
    /// Драйвер не слинковал программу.
    #[error("program failed to link: {log}")]
    Link {
        /// Журнал линковщика.
        log: String,
    },
    /// GL не создал объект шейдера/программы.
    #[error("GL could not create a shader object: {message}")]
    ObjectCreation {
        /// Ответ GL.
        message: String,
    },
    /// Контекст недоступен (потерян или окно отпущено без surfaceless-контекста).
    #[error("GLES2 context is unavailable: {0}")]
    ContextUnavailable(crate::GlAccessError),
}

/// `true`, если исходник начинается директивой `#version 100` (пробелы в начале допустимы;
/// комментарии до директивы мы не пишем и не принимаем — проще и строже).
pub(crate) fn declares_es100(source: &str) -> bool {
    let Some(rest) = source.trim_start().strip_prefix("#version") else {
        return false;
    };
    // Директива и номер разделены пробелом, а после `100` на строке ничего нет
    // (профиля у GLSL ES 1.00 не бывает).
    let first_line = rest.lines().next().unwrap_or_default();
    first_line.starts_with([' ', '\t']) && first_line.split_whitespace().eq(["100"])
}

/// Собирает программу. Вызывающий владеет результатом и удаляет его
/// (`Gles2Context::delete_program`).
pub(crate) fn build_program(
    gl: CurrentGl<'_>,
    sources: Es100ProgramSources<'_>,
) -> Result<glow::NativeProgram, ShaderError> {
    for (stage, source) in [
        (ShaderStage::Vertex, sources.vertex),
        (ShaderStage::Fragment, sources.fragment),
    ] {
        if !declares_es100(source) {
            return Err(ShaderError::NotEs100 { stage });
        }
    }
    let vertex = compile(gl, ShaderStage::Vertex, sources.vertex)?;
    let fragment = match compile(gl, ShaderStage::Fragment, sources.fragment) {
        Ok(fragment) => fragment,
        Err(error) => {
            gl.delete_shader(vertex);
            return Err(error);
        }
    };
    match gl.link_program(vertex, fragment) {
        Ok(LinkedProgram::Linked(program)) => Ok(program),
        Ok(LinkedProgram::Failed { log }) => Err(ShaderError::Link { log }),
        Err(message) => Err(ShaderError::ObjectCreation { message }),
    }
}

fn compile(
    gl: CurrentGl<'_>,
    stage: ShaderStage,
    source: &str,
) -> Result<glow::NativeShader, ShaderError> {
    let kind = match stage {
        ShaderStage::Vertex => ShaderStageKind::Vertex,
        ShaderStage::Fragment => ShaderStageKind::Fragment,
    };
    match gl.compile_shader(kind, source) {
        Ok(CompiledShader::Compiled(shader)) => Ok(shader),
        Ok(CompiledShader::Failed { log }) => Err(ShaderError::Compile { stage, log }),
        Err(message) => Err(ShaderError::ObjectCreation { message }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_version_100_is_accepted() {
        assert!(declares_es100("#version 100\nvoid main() {}"));
        assert!(declares_es100("\n  #version 100\nvoid main() {}"));
        assert!(!declares_es100("#version 300 es\nvoid main() {}"));
        assert!(!declares_es100("#version 100 es\nvoid main() {}"));
        assert!(!declares_es100("void main() {}"));
        assert!(!declares_es100("// comment\n#version 100\n"));
        assert!(!declares_es100("#version\n100\n"));
    }
}
