//! Самопроверка видимости записи CPU для программного кадра без копии (сессия 12).
//!
//! # Зачем
//! Программный декодер пишет кадр в память (CPU), а GPU читает её через импортированный
//! dma-buf. На GPU с кэшем, общим с CPU (AMD APU, Intel с LLC, llvmpipe), записанное видно
//! сразу. На GPU без общего кэша (Intel Ironlake, T410: crocus, нет LLC) GPU видит СТАРЫЕ байты,
//! пока производитель не сбросит записанные строки из кэша CPU (замер сессии 12: без сброса
//! 231 из 240 кадров были старыми). Решение владельца 2: нужен ли сброс, определяет не имя GPU,
//! а эта проверка — настоящая запись CPU и настоящее чтение GPU через тот же путь импорта.
//!
//! # Что делает проверка
//! Импортирует одну плоскость `R8` 64×64 из буфера производителя в текстуру, рисует её центр в
//! собственный крошечный FBO своей программой и читает пиксель `glReadPixels`:
//! 1. записать A, импортировать, прочитать — первая привязка страниц сама сбрасывает кэш;
//! 2. если прочитано A: записать B БЕЗ сброса и прочитать; если B и затем C — записи видны сразу
//!    ([`CpuWriteVisibility::CoherentWithoutCacheFlush`]; два разных значения исключают случайность);
//! 3. иначе записать D, сбросить кэш ([`CpuWriteProbeProducer::flush_cpu_cache`]), прочитать:
//!    D — [`CpuWriteVisibility::VisibleAfterCacheFlush`], иначе
//!    [`CpuWriteVisibility::NotVisible`] (путь без копии на этой машине использовать нельзя).
//!
//! Вызывать ВНЕ кадра показа (до первого видео). Состояние GL, которое трогает проверка
//! (привязка framebuffer, viewport, программа, привязка текстуры блока 0, буфер вершин, флаги
//! смешивания/отбраковки/дизеринга/scissor, маска цвета), сохраняется и возвращается при любом
//! исходе; созданные объекты удаляются при любом исходе. Указатель атрибута вершин не
//! возвращается: его задаёт каждый проход рисования сам (контракт слоя кадра).

use std::os::fd::BorrowedFd;

use thiserror::Error;

use gles2_egui::{
    DmaBufPlaneTexture, DmaBufTextureError, DrmFourcc, DrmModifier, Es100ProgramSources,
    GlAccessError, Gles2Context, ShaderError,
};

use crate::gl_calls::probe::ProbeRenderTarget;
use crate::gl_calls::{VideoGl, VideoGlError};
use crate::program::FULLSCREEN_TRIANGLE;

/// Ширина и высота плоскости проверки, отсчётов.
const PROBE_PLANE_SIDE: u32 = 64;
/// Шаг строк плоскости проверки, байт. С запасом над шириной: линейный импорт на AMD требует
/// выравнивания шага (как у остальных тестовых буферов крейта).
const PROBE_PLANE_PITCH: u32 = 256;
/// Сторона цели рисования проверки, пикселей.
const PROBE_TARGET_SIDE: u32 = 4;
/// Допуск сравнения прочитанного байта: округление `f32` в шейдере.
const READ_TOLERANCE: u8 = 1;

/// Минимальный размер буфера производителя, байт: плоскость 64 строки по 256 байт.
pub const CPU_WRITE_PROBE_MIN_BUFFER_BYTES: usize = (PROBE_PLANE_SIDE * PROBE_PLANE_PITCH) as usize;

/// Значения, которые проверка пишет по очереди. Разные, не равны нулю и друг другу, чтобы свежая
/// память (нули) и предыдущая запись не сошлись с ожидаемым.
const VALUE_FIRST: u8 = 0x3C;
const VALUE_WITHOUT_FLUSH: u8 = 0xA5;
const VALUE_WITHOUT_FLUSH_AGAIN: u8 = 0x5A;
const VALUE_WITH_FLUSH: u8 = 0xC3;

/// Вершинный шейдер: треугольник на всю цель.
const PROBE_VERTEX_SHADER: &str = "#version 100
attribute vec2 a_position;
void main() {
    gl_Position = vec4(a_position, 0.0, 1.0);
}
";

/// Фрагментный шейдер: красный канал = центр плоскости `R8` (однородной по значению).
const PROBE_FRAGMENT_SHADER: &str = "#version 100
precision mediump float;
precision mediump sampler2D;
uniform sampler2D u_plane;
void main() {
    gl_FragColor = vec4(texture2D(u_plane, vec2(0.5, 0.5)).r, 0.0, 0.0, 1.0);
}
";

/// Производитель кадров с точки зрения самопроверки: владеет буфером ядра (dma-buf, не менее
/// [`CPU_WRITE_PROBE_MIN_BUFFER_BYTES`] байт, линейный, например `udmabuf`) и умеет писать в
/// него так, как пишет декодер, и сбрасывать кэш CPU так, как будет сбрасывать плеер.
pub trait CpuWriteProbeProducer {
    /// Дескриптор dma-buf с памятью проверки (заимствуется на время проверки).
    fn dma_buf(&self) -> BorrowedFd<'_>;

    /// Записать `value` во ВСЕ байты буфера обычной записью CPU, без сброса кэша.
    fn write_all_bytes(&mut self, value: u8);

    /// Сбросить записанное из кэша CPU в память (на x86 — `clflush` по записанным строкам).
    fn flush_cpu_cache(&mut self);
}

/// Что показала самопроверка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuWriteVisibility {
    /// Запись CPU видна GPU сразу, без сброса кэша (общий кэш или программный рендер).
    CoherentWithoutCacheFlush,
    /// Запись видна только после сброса кэша CPU: производитель обязан сбрасывать записанные
    /// строки каждого кадра ([`CpuWriteProbeProducer::flush_cpu_cache`]).
    VisibleAfterCacheFlush,
    /// Запись не видна даже после сброса: кадры без копии на этой машине не использовать.
    NotVisible,
}

/// Самопроверка не смогла сделать измерение (это не ответ «не видно»).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CpuWriteProbeError {
    /// Контекст недоступен.
    #[error("GLES2 context unavailable: {0}")]
    GlAccess(#[source] GlAccessError),
    /// Не собралась программа проверки.
    #[error("probe program: {0}")]
    Shader(#[source] ShaderError),
    /// В программе проверки нет нужного входа.
    #[error("probe program has no input named `{name}`")]
    MissingShaderInput {
        /// Имя входа.
        name: &'static str,
    },
    /// Отказ GL при создании объектов проверки.
    #[error("probe GL: {0}")]
    Gl(#[source] VideoGlError),
    /// Драйвер не принял плоскость проверки.
    #[error("probe plane import failed: {0}")]
    Attach(#[source] DmaBufTextureError),
}

/// Выяснить, видит ли GPU запись CPU в dma-buf производителя.
///
/// # Errors
/// [`CpuWriteProbeError`]: контекст недоступен, программа не собралась, объекты GL не созданы,
/// драйвер не принял плоскость (тогда без копии кадры на этой машине всё равно не показать).
/// Состояние GL при ошибке возвращено так же, как при успехе.
pub fn probe_cpu_write_visibility(
    context: &Gles2Context,
    producer: &mut dyn CpuWriteProbeProducer,
) -> Result<CpuWriteVisibility, CpuWriteProbeError> {
    let gl = VideoGl::for_context(context).map_err(CpuWriteProbeError::GlAccess)?;
    let saved_state = gl.save_probe_state();
    let mut objects = ProbeObjects::default();
    let outcome = measure(context, gl, &mut objects, producer);
    // Удаление и возврат состояния — при любом исходе, ДО возврата результата.
    objects.release(context, gl);
    gl.restore_probe_state(&saved_state);
    outcome
}

/// Объекты GL проверки. Заполняются по мере создания, чтобы отказ посреди сборки удалил
/// ровно уже созданное.
#[derive(Default)]
struct ProbeObjects {
    program: Option<glow::NativeProgram>,
    vertices: Option<glow::NativeBuffer>,
    texture: Option<glow::NativeTexture>,
    target: Option<ProbeRenderTarget>,
}

impl ProbeObjects {
    fn release(&mut self, context: &Gles2Context, gl: VideoGl<'_>) {
        if let Some(program) = self.program.take()
            && let Err(error) = context.delete_program(program)
        {
            log::warn!("gles2-video: probe could not delete its program: {error}");
        }
        if let Some(vertices) = self.vertices.take() {
            gl.delete_buffer(vertices);
        }
        if let Some(texture) = self.texture.take() {
            gl.delete_texture(texture);
        }
        if let Some(target) = self.target.take() {
            gl.delete_probe_render_target(target);
        }
    }
}

/// Готовая к измерению проверка: объекты и входы программы.
struct ProbeRig {
    program: glow::NativeProgram,
    position_attribute: u32,
    vertices: glow::NativeBuffer,
    texture: glow::NativeTexture,
}

impl ProbeRig {
    /// Нарисовать плоскость в цель и прочитать красный канал.
    fn sample_red(&self, gl: VideoGl<'_>, target: &ProbeRenderTarget) -> u8 {
        gl.draw_probe_sample(
            target,
            self.program,
            self.position_attribute,
            self.vertices,
            self.texture,
        );
        gl.read_probe_red(target)
    }
}

fn measure(
    context: &Gles2Context,
    gl: VideoGl<'_>,
    objects: &mut ProbeObjects,
    producer: &mut dyn CpuWriteProbeProducer,
) -> Result<CpuWriteVisibility, CpuWriteProbeError> {
    let rig = build_rig(context, gl, objects)?;
    let target = gl
        .create_probe_render_target(PROBE_TARGET_SIDE)
        .map_err(CpuWriteProbeError::Gl)?;
    // Цель в `objects` сразу: дальнейший отказ удалит и её.
    let target = &*objects.target.insert(target);

    // 1. Первая привязка страниц сама сбрасывает кэш: запись до импорта видна сразу.
    producer.write_all_bytes(VALUE_FIRST);
    attach_probe_plane(context, producer.dma_buf(), rig.texture)?;
    gl.set_imported_texture_sampling(rig.texture);
    let sees = |expected: u8, actual: u8| actual.abs_diff(expected) <= READ_TOLERANCE;
    if sees(VALUE_FIRST, rig.sample_red(gl, target)) {
        // 2. Дальше запись без сброса: если видны два разных значения подряд — кэш общий.
        producer.write_all_bytes(VALUE_WITHOUT_FLUSH);
        if sees(VALUE_WITHOUT_FLUSH, rig.sample_red(gl, target)) {
            producer.write_all_bytes(VALUE_WITHOUT_FLUSH_AGAIN);
            if sees(VALUE_WITHOUT_FLUSH_AGAIN, rig.sample_red(gl, target)) {
                return Ok(CpuWriteVisibility::CoherentWithoutCacheFlush);
            }
        }
    }
    // 3. Запись без сброса не прошла (или не прошла первая): пробуем со сбросом.
    producer.write_all_bytes(VALUE_WITH_FLUSH);
    producer.flush_cpu_cache();
    if sees(VALUE_WITH_FLUSH, rig.sample_red(gl, target)) {
        Ok(CpuWriteVisibility::VisibleAfterCacheFlush)
    } else {
        Ok(CpuWriteVisibility::NotVisible)
    }
}

/// Собрать программу, буфер вершин и текстуру; записывает созданное в `objects`.
fn build_rig(
    context: &Gles2Context,
    gl: VideoGl<'_>,
    objects: &mut ProbeObjects,
) -> Result<ProbeRig, CpuWriteProbeError> {
    let program = context
        .compile_es100_program(Es100ProgramSources {
            vertex: PROBE_VERTEX_SHADER,
            fragment: PROBE_FRAGMENT_SHADER,
        })
        .map_err(CpuWriteProbeError::Shader)?;
    objects.program = Some(program);
    let position_attribute = gl
        .attribute_location(program, "a_position")
        .ok_or(CpuWriteProbeError::MissingShaderInput { name: "a_position" })?;
    let sampler = gl
        .uniform_location(program, "u_plane")
        .ok_or(CpuWriteProbeError::MissingShaderInput { name: "u_plane" })?;
    // Сэмплер смотрит в блок 0, где `draw_probe_sample` держит текстуру плоскости.
    gl.use_program(Some(program));
    gl.set_uniform_int(&sampler, 0);
    let vertices = gl.create_buffer().map_err(CpuWriteProbeError::Gl)?;
    objects.vertices = Some(vertices);
    gl.upload_static_vertices(vertices, &FULLSCREEN_TRIANGLE);
    let texture = gl.create_texture().map_err(CpuWriteProbeError::Gl)?;
    objects.texture = Some(texture);
    Ok(ProbeRig {
        program,
        position_attribute,
        vertices,
        texture,
    })
}

/// Сделать плоскость проверки хранилищем текстуры (линейный `R8`, смещение 0).
fn attach_probe_plane(
    context: &Gles2Context,
    dma_buf: BorrowedFd<'_>,
    texture: glow::NativeTexture,
) -> Result<(), CpuWriteProbeError> {
    let plane = DmaBufPlaneTexture {
        fourcc: DrmFourcc::R8,
        width: PROBE_PLANE_SIDE,
        height: PROBE_PLANE_SIDE,
        fd: dma_buf,
        offset: 0,
        pitch: PROBE_PLANE_PITCH,
        modifier: DrmModifier::LINEAR,
    };
    context
        .attach_dma_buf_plane_to_texture(texture, &plane)
        .map(|_| ())
        .map_err(CpuWriteProbeError::Attach)
}
