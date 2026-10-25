//! Импорт плоскости DMA-BUF в GL-текстуру на настоящем EGL: байты буфера доходят до пикселей.
//!
//! Буфер создаёт ядро: `/dev/udmabuf` оборачивает обычную память `memfd` в dma-buf fd (всегда
//! linear). Тест рисует импортированную текстуру на offscreen-цель шейдером GLSL ES 1.00 и
//! сверяет каждый пиксель с тем, что записано в память. Без `/dev/udmabuf` тест пропускается с
//! причиной в stderr (EGL при этом обязателен, если задан `GLES2_EGUI_REQUIRE_EGL`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    unsafe_code,
    reason = "integration test: loud failures, raw GL and libc calls with SAFETY notes"
)]

mod support;

use std::io::{Seek, SeekFrom, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};

use gles2_egui::{
    DmaBufPlaneTexture, DmaBufTextureError, DrmFourcc, DrmModifier, Es100ProgramSources,
    Gles2Context, ImageRelease, ModifierAttribute, OffscreenAlpha, OffscreenPixels,
};
use glow::HasContext;
use support::offscreen_or_skip;

const WIDTH: u32 = 20;
const HEIGHT: u32 = 10;
/// Шаг строки с запасом: AMD требует выравнивания pitch на 256 байт.
const PITCH: u32 = 256;
const UDMABUF_DEVICE_PATH: &str = "/dev/udmabuf";
/// `UDMABUF_CREATE = _IOW('u', 0x42, struct udmabuf_create)`, 24 байта (`linux/udmabuf.h`).
const UDMABUF_CREATE_IOCTL: libc::c_ulong = 0x4018_7542;
const UDMABUF_FLAGS_CLOEXEC: u32 = 0x01;
/// Заполнитель отступа строки: пиксели в него не должны попадать.
const PADDING_BYTE: u8 = 0xEE;

/// Раскладка `struct udmabuf_create`.
#[repr(C)]
struct UdmabufCreateRequest {
    memfd: u32,
    flags: u32,
    offset: u64,
    size: u64,
}

/// Буфер из памяти memfd + его dma-buf fd. memfd держим открытым ради перезаписи.
struct Udmabuf {
    dma_buf: OwnedFd,
    memfd: std::fs::File,
}

fn create_udmabuf(contents: &[u8]) -> Result<Udmabuf, String> {
    let device = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(UDMABUF_DEVICE_PATH)
        .map_err(|error| format!("{UDMABUF_DEVICE_PATH} is not accessible: {error}"))?;
    // SAFETY: `sysconf` только читает системную константу.
    let page_size = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
    let buffer_size = (contents.len() as u64).div_ceil(page_size) * page_size;
    // SAFETY: имя — статическая C-строка; результат проверяется и сразу идёт во владение.
    let raw_memfd =
        unsafe { libc::memfd_create(c"gles2-egui-test".as_ptr(), libc::MFD_ALLOW_SEALING) };
    assert!(
        raw_memfd >= 0,
        "memfd_create: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: fd только что создан и больше ничьим владением не является.
    let mut memfd = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw_memfd) });
    memfd.set_len(buffer_size).unwrap();
    memfd.write_all(contents).unwrap();
    // udmabuf принимает только memfd, который нельзя уменьшить.
    // SAFETY: fd валиден на время вызова; fcntl владение не забирает.
    let sealed = unsafe { libc::fcntl(memfd.as_raw_fd(), libc::F_ADD_SEALS, libc::F_SEAL_SHRINK) };
    assert_eq!(
        sealed,
        0,
        "F_SEAL_SHRINK: {}",
        std::io::Error::last_os_error()
    );
    let request = UdmabufCreateRequest {
        memfd: u32::try_from(memfd.as_raw_fd()).unwrap(),
        flags: UDMABUF_FLAGS_CLOEXEC,
        offset: 0,
        size: buffer_size,
    };
    // SAFETY: `request` совпадает с `struct udmabuf_create` и живёт весь вызов; fd валидны.
    let raw_dma_buf = unsafe { libc::ioctl(device.as_raw_fd(), UDMABUF_CREATE_IOCTL, &request) };
    if raw_dma_buf < 0 {
        return Err(format!(
            "UDMABUF_CREATE: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: ioctl вернул новый fd, которым больше никто не владеет.
    let dma_buf = unsafe { OwnedFd::from_raw_fd(raw_dma_buf) };
    Ok(Udmabuf { dma_buf, memfd })
}

/// Перезаписать содержимое с начала ТАК, КАК ОБЯЗАН ПИСАТЬ CPU-ПРОИЗВОДИТЕЛЬ в dma-buf.
///
/// Простая запись в memfd оставляет новые байты в кэше CPU. На GPU с кэшем, общим с CPU
/// (AMD APU, Intel с LLC), этого достаточно, а на Intel Ironlake (T410, Mesa crocus, i915)
/// общего LLC нет: GPU читает чужой импортированный объект мимо кэша CPU и видит СТАРЫЕ
/// байты. Первое рисование верно (ядро сбрасывает кэш при первой привязке страниц), а все
/// последующие записи ядро не отслеживает. Доказано экспериментом на T410: запись через
/// `DMA_BUF_IOCTL_SYNC` одна НЕ помогает (на x86 `udmabuf` не сбрасывает кэш: для
/// когерентного по DMA x86 `dma_sync_*` — пустышка), помогает только явный `clflush`
/// по записанному диапазону. Поэтому производитель: открывает окно CPU-доступа
/// (`DMA_BUF_IOCTL_SYNC`, Documentation/driver-api/dma-buf.rst, «CPU Access to DMA Buffer
/// Objects» — официальный протокол ядра, на других архитектурах он и делает сброс кэша),
/// пишет, сбрасывает кэш CPU по диапазону (x86_64) и закрывает окно.
fn overwrite_contents_as_cpu_producer(buffer: &mut Udmabuf, contents: &[u8]) {
    let length = buffer.memfd.metadata().unwrap().len();
    sync_cpu_access(&buffer.dma_buf, CpuAccessWindow::Begin);
    buffer.memfd.seek(SeekFrom::Start(0)).unwrap();
    buffer.memfd.write_all(contents).unwrap();
    buffer.memfd.sync_all().unwrap();
    flush_cpu_cache(&buffer.memfd, length);
    sync_cpu_access(&buffer.dma_buf, CpuAccessWindow::End);
}

/// Начало или конец окна CPU-доступа к dma-buf.
#[derive(Clone, Copy)]
enum CpuAccessWindow {
    Begin,
    End,
}

/// `DMA_BUF_IOCTL_SYNC = _IOW('b', 0, struct dma_buf_sync)`, 8 байт (`linux/dma-buf.h`).
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
/// `DMA_BUF_SYNC_WRITE`: CPU собирается писать.
const DMA_BUF_SYNC_WRITE: u64 = 2;
/// `DMA_BUF_SYNC_END`: конец окна (начало — нулевой флаг `DMA_BUF_SYNC_START`).
const DMA_BUF_SYNC_END: u64 = 4;
/// Размер строки кэша x86_64: `clflush` сбрасывает ровно одну такую строку.
#[cfg(target_arch = "x86_64")]
const CACHE_LINE_BYTES: usize = 64;

/// `DMA_BUF_IOCTL_SYNC` с флагом записи; ошибка ядра — громкое падение теста.
fn sync_cpu_access(dma_buf: &OwnedFd, window: CpuAccessWindow) {
    let flags = match window {
        CpuAccessWindow::Begin => DMA_BUF_SYNC_WRITE,
        CpuAccessWindow::End => DMA_BUF_SYNC_END | DMA_BUF_SYNC_WRITE,
    };
    // SAFETY: `flags` — `struct dma_buf_sync` из одного u64, живёт весь вызов; fd валиден.
    let result = unsafe { libc::ioctl(dma_buf.as_raw_fd(), DMA_BUF_IOCTL_SYNC, &flags) };
    assert_eq!(
        result,
        0,
        "DMA_BUF_IOCTL_SYNC: {}",
        std::io::Error::last_os_error()
    );
}

/// Сбросить из кэша CPU в память первые `length` байт memfd (`clflush` по каждой строке кэша).
/// Страницы memfd отображаются отдельным mmap: те же физические страницы, что видит GPU.
#[cfg(target_arch = "x86_64")]
fn flush_cpu_cache(memfd: &std::fs::File, length: u64) {
    let length = usize::try_from(length).unwrap();
    // SAFETY: отображение shared RW на весь memfd; результат проверяется, `munmap` ниже.
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            memfd.as_raw_fd(),
            0,
        )
    };
    assert_ne!(
        mapping,
        libc::MAP_FAILED,
        "mmap: {}",
        std::io::Error::last_os_error()
    );
    let base = mapping.cast::<u8>();
    // SAFETY: все адреса `base + offset` лежат внутри только что созданного отображения;
    // барьеры упорядочивают запись CPU до сброса и сброс до последующего чтения GPU.
    unsafe {
        core::arch::x86_64::_mm_mfence();
        for offset in (0..length).step_by(CACHE_LINE_BYTES) {
            core::arch::x86_64::_mm_clflush(base.add(offset));
        }
        core::arch::x86_64::_mm_mfence();
        libc::munmap(mapping, length);
    }
}

/// Не x86_64: сброс кэша делает `DMA_BUF_IOCTL_SYNC` (там `dma_sync_*` реальный).
#[cfg(not(target_arch = "x86_64"))]
fn flush_cpu_cache(_memfd: &std::fs::File, _length: u64) {}

/// Какой формат проверяем.
#[derive(Clone, Copy)]
struct PlaneKind {
    fourcc: DrmFourcc,
    bytes_per_pixel: u32,
}

const R8: PlaneKind = PlaneKind {
    fourcc: DrmFourcc::R8,
    bytes_per_pixel: 1,
};
const GR88: PlaneKind = PlaneKind {
    fourcc: DrmFourcc::GR88,
    bytes_per_pixel: 2,
};

/// Байт канала `channel` пикселя (x, y) для данного «поколения» содержимого.
fn channel_value(generation: u32, channel: u32, x: u32, y: u32) -> u8 {
    u8::try_from((x * 13 + y * 7 + channel * 59 + generation * 101) % 251).unwrap()
}

/// Содержимое буфера: градиент в первых `width * bpp` байтах строки, остальное — заполнитель.
fn plane_bytes(kind: PlaneKind, generation: u32) -> Vec<u8> {
    let mut bytes = vec![PADDING_BYTE; (PITCH * HEIGHT) as usize];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            for channel in 0..kind.bytes_per_pixel {
                let index = (y * PITCH + x * kind.bytes_per_pixel + channel) as usize;
                bytes[index] = channel_value(generation, channel, x, y);
            }
        }
    }
    bytes
}

const VERTEX_SHADER: &str = "#version 100
attribute vec2 position;
varying vec2 texture_coordinate;
void main() {
    // Верх экрана (y = +1) — первая строка текстуры (t = 0).
    texture_coordinate = vec2(position.x * 0.5 + 0.5, 0.5 - position.y * 0.5);
    gl_Position = vec4(position, 0.0, 1.0);
}
";
const FRAGMENT_SHADER: &str = "#version 100
precision mediump float;
varying vec2 texture_coordinate;
uniform sampler2D plane;
void main() {
    vec4 texel = texture2D(plane, texture_coordinate);
    gl_FragColor = vec4(texel.r, texel.g, 0.0, 1.0);
}
";

/// Всё, что нужно для рисования плоскости на цель.
struct PlaneRenderer {
    program: glow::NativeProgram,
    vertex_buffer: glow::NativeBuffer,
}

impl PlaneRenderer {
    fn new(context: &Gles2Context) -> Self {
        let program = context
            .compile_es100_program(Es100ProgramSources {
                vertex: VERTEX_SHADER,
                fragment: FRAGMENT_SHADER,
            })
            .expect("shader compiles");
        let gl = context.gl().expect("GL access");
        let corners: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
        // SAFETY: контекст текущий (`context.gl()` сделал его таким); буфер создаётся и
        // наполняется данными нужного размера.
        let vertex_buffer = unsafe {
            let buffer = gl.create_buffer().unwrap();
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck_cast(&corners),
                glow::STATIC_DRAW,
            );
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            buffer
        };
        Self {
            program,
            vertex_buffer,
        }
    }

    /// Рисует `texture` на всю цель и возвращает пиксели.
    fn draw(&self, context: &mut Gles2Context, texture: glow::NativeTexture) -> OffscreenPixels {
        match context.begin_frame().expect("frame starts") {
            gles2_egui::FrameStart::Ready(frame) => {
                let gl = frame.gl();
                // SAFETY: контекст кадра текущий; программа, буфер и текстура созданы этим
                // контекстом и живы; атрибут 0 привязан к `position` программы.
                unsafe {
                    gl.disable(glow::SCISSOR_TEST);
                    gl.disable(glow::BLEND);
                    gl.use_program(Some(self.program));
                    gl.active_texture(glow::TEXTURE0);
                    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                    let sampler = gl.get_uniform_location(self.program, "plane");
                    gl.uniform_1_i32(sampler.as_ref(), 0);
                    let position = gl.get_attrib_location(self.program, "position").unwrap();
                    gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vertex_buffer));
                    gl.enable_vertex_attrib_array(position);
                    gl.vertex_attrib_pointer_f32(position, 2, glow::FLOAT, false, 0, 0);
                    gl.clear_color(0.0, 0.0, 0.0, 1.0);
                    gl.clear(glow::COLOR_BUFFER_BIT);
                    gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                    gl.disable_vertex_attrib_array(position);
                    gl.bind_buffer(glow::ARRAY_BUFFER, None);
                    gl.bind_texture(glow::TEXTURE_2D, None);
                    gl.use_program(None);
                }
                frame.present().expect("offscreen present");
            }
            gles2_egui::FrameStart::Skipped(reason) => panic!("frame skipped: {reason:?}"),
        }
        context.read_offscreen_pixels().expect("readback")
    }

    fn delete(self, context: &Gles2Context) {
        let gl = context.gl().expect("GL access");
        // SAFETY: контекст текущий; объекты созданы им и больше не используются.
        unsafe { gl.delete_buffer(self.vertex_buffer) };
        context
            .delete_program(self.program)
            .expect("delete program");
    }
}

/// `&[f32]` как байты (в тесте без лишней зависимости).
fn bytemuck_cast(values: &[f32; 8]) -> &[u8] {
    // SAFETY: f32 не имеет отступов и допустимых-невалидных битов; длина в байтах считается из
    // размера массива.
    unsafe { std::slice::from_raw_parts(values.as_ptr().cast(), std::mem::size_of_val(values)) }
}

fn new_texture(context: &Gles2Context) -> glow::NativeTexture {
    let gl = context.gl().expect("GL access");
    // SAFETY: контекст текущий; параметры — константы ES 2.0 (NPOT-текстуре нужны NEAREST/LINEAR
    // без mip и CLAMP_TO_EDGE).
    unsafe {
        let texture = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        for (parameter, value) in [
            (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
            (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, parameter, value as i32);
        }
        gl.bind_texture(glow::TEXTURE_2D, None);
        texture
    }
}

/// Сверяет все пиксели цели с ожидаемым поколением содержимого.
fn assert_pixels(pixels: &OffscreenPixels, kind: PlaneKind, generation: u32, what: &str) {
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let red = channel_value(generation, 0, x, y);
            let green = if kind.bytes_per_pixel == 2 {
                channel_value(generation, 1, x, y)
            } else {
                0
            };
            assert_eq!(
                pixels.pixel(x, y),
                Some([red, green, 0, 255]),
                "{what}: pixel ({x}, {y})"
            );
        }
    }
}

fn plane_of<'fd>(kind: PlaneKind, buffer: &'fd OwnedFd) -> DmaBufPlaneTexture<'fd> {
    DmaBufPlaneTexture {
        fourcc: kind.fourcc,
        width: WIDTH,
        height: HEIGHT,
        fd: buffer.as_fd(),
        offset: 0,
        pitch: PITCH,
        modifier: DrmModifier::LINEAR,
    }
}

/// Контекст + буфер или пропуск с причиной.
fn setup(test_name: &str, kind: PlaneKind) -> Option<(Gles2Context, Udmabuf)> {
    let context = offscreen_or_skip(
        test_name,
        support::size(WIDTH, HEIGHT),
        OffscreenAlpha::Opaque,
    )?;
    match create_udmabuf(&plane_bytes(kind, 0)) {
        Ok(buffer) => Some((context, buffer)),
        Err(reason) => {
            eprintln!("skip {test_name}: no kernel udmabuf: {reason}");
            None
        }
    }
}

/// Импорт, при невозможности — пропуск (например, драйвер без нужной пары формат+модификатор).
fn attach_or_skip(
    test_name: &str,
    context: &Gles2Context,
    texture: glow::NativeTexture,
    plane: &DmaBufPlaneTexture<'_>,
) -> Option<gles2_egui::DmaBufTextureAttached> {
    match context.attach_dma_buf_plane_to_texture(texture, plane) {
        Ok(attached) => Some(attached),
        Err(
            error @ (DmaBufTextureError::ZeroCopyUnsupported { .. }
            | DmaBufTextureError::FormatModifierNotSampleable { .. }
            | DmaBufTextureError::ImageTargetFunctionMissing),
        ) => {
            eprintln!("skip {test_name}: driver cannot import this plane: {error}");
            None
        }
        Err(error) => panic!("{test_name}: import failed: {error}"),
    }
}

/// Полный сценарий: импорт → рисование → перезапись памяти → рисование → закрытие всех fd →
/// рисование. Каждый шаг сверяет пиксели с записанными байтами.
fn full_scenario(test_name: &str, kind: PlaneKind) {
    let Some((mut context, mut buffer)) = setup(test_name, kind) else {
        return;
    };
    let renderer = PlaneRenderer::new(&context);
    let texture = new_texture(&context);
    let Some(attached) = attach_or_skip(
        test_name,
        &context,
        texture,
        &plane_of(kind, &buffer.dma_buf),
    ) else {
        return;
    };
    assert_eq!(
        attached.modifier,
        ModifierAttribute::Explicit(DrmModifier::LINEAR)
    );
    assert_eq!(attached.image_release, ImageRelease::Released);

    // Содержимое памяти доходит до пикселей.
    assert_pixels(&renderer.draw(&mut context, texture), kind, 0, "first draw");

    // EGLImage — ссылка на память, а не копия: перезапись видна БЕЗ повторного attach.
    overwrite_contents_as_cpu_producer(&mut buffer, &plane_bytes(kind, 1));
    assert_pixels(
        &renderer.draw(&mut context, texture),
        kind,
        1,
        "after overwrite",
    );

    // Закрываем ВСЕ свои fd: буфер держит текстура, а не наш дескриптор.
    let Udmabuf { dma_buf, memfd } = buffer;
    drop(dma_buf);
    drop(memfd);
    assert_pixels(
        &renderer.draw(&mut context, texture),
        kind,
        1,
        "after closing fds",
    );

    let stats = context.dma_buf_texture_stats();
    assert_eq!((stats.images_created, stats.images_destroyed), (1, 1));
    assert_eq!(stats.attaches_succeeded, 1);
    assert_eq!(stats.attach_failures, 0);

    let gl = context.gl().unwrap();
    // SAFETY: контекст текущий; текстура создана им и больше не нужна.
    unsafe { gl.delete_texture(texture) };
    renderer.delete(&context);
}

#[test]
fn r8_plane_from_udmabuf_renders_exact_bytes_and_follows_memory() {
    full_scenario("r8_plane_from_udmabuf", R8);
}

#[test]
fn gr88_plane_from_udmabuf_renders_both_channels() {
    full_scenario("gr88_plane_from_udmabuf", GR88);
}

#[test]
fn fds_closed_immediately_after_attach_do_not_break_the_texture() {
    let name = "fds_closed_immediately_after_attach";
    let Some((mut context, buffer)) = setup(name, R8) else {
        return;
    };
    let renderer = PlaneRenderer::new(&context);
    let texture = new_texture(&context);
    let Some(_) = attach_or_skip(name, &context, texture, &plane_of(R8, &buffer.dma_buf)) else {
        return;
    };
    drop(buffer);

    assert_pixels(
        &renderer.draw(&mut context, texture),
        R8,
        0,
        "draw after close",
    );
}

/// Программные драйверы Mesa (llvmpipe/softpipe) не проверяют fd при `eglCreateImage`: проверено
/// в сессии 07 — они «импортируют» и `/dev/null`, и конец pipe (содержимое читается лишь при
/// первом использовании). Отказ драйвера на них не воспроизвести, поэтому тест отказа
/// пропускается с причиной; на любом аппаратном драйвере принятый не-dma-buf — падение теста.
fn skip_if_driver_defers_dma_buf_fd_validation(name: &str, context: &Gles2Context) -> bool {
    let renderer = context.capabilities().gl_renderer();
    let software = ["llvmpipe", "softpipe"]
        .iter()
        .any(|driver| renderer.contains(driver));
    if software {
        eprintln!(
            "skip {name}: software driver ({renderer}) does not validate dma-buf fds at import"
        );
    }
    software
}

#[test]
fn driver_rejects_a_non_dma_buf_fd_with_typed_error_and_clean_state() {
    let name = "driver_rejects_a_non_dma_buf_fd";
    let Some((context, _buffer)) = setup(name, R8) else {
        return;
    };
    let texture = new_texture(&context);
    if skip_if_driver_defers_dma_buf_fd_validation(name, &context) {
        return;
    }
    // /dev/null — не dma-buf: eglCreateImage аппаратного драйвера обязан отказать.
    let not_dma_buf = OwnedFd::from(std::fs::File::open("/dev/null").unwrap());
    let error = match context.attach_dma_buf_plane_to_texture(texture, &plane_of(R8, &not_dma_buf))
    {
        Err(error) => error,
        Ok(_) => panic!("{name}: /dev/null must not import"),
    };
    match error {
        DmaBufTextureError::ImageCreation(_) => {}
        DmaBufTextureError::ZeroCopyUnsupported { .. }
        | DmaBufTextureError::FormatModifierNotSampleable { .. }
        | DmaBufTextureError::ImageTargetFunctionMissing => {
            eprintln!("skip {name}: driver cannot import this plane: {error}");
            return;
        }
        other => panic!("{name}: unexpected error {other}"),
    }
    let stats = context.dma_buf_texture_stats();
    assert_eq!((stats.images_created, stats.images_destroyed), (0, 0));
    assert_eq!(stats.attach_failures, 1);
    // Контекст жив, fd не закрыт API: его закроет Drop вызывающего.
    assert!(context.context_loss().is_none());
    assert!(std::path::Path::new(&format!("/proc/self/fd/{}", not_dma_buf.as_raw_fd())).exists());
}
