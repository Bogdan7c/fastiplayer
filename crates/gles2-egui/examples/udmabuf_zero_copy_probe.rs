//! Стенд сессии 12 плана `user/gles2-renderer/`: можно ли показать программно декодированный
//! кадр на GLES2 без CPU-копии.
//!
//! Одна и та же программа рисует три R8-плоскости кадра I420 (раскладка как у блоков декодера
//! FFmpeg из cpu-parity 08b: шаг строк — нечётное кратное 256, плоскости в одном блоке) тремя
//! способами подачи:
//! - `upload` — как сейчас в плеере: кадр в обычной памяти, `glTexSubImage2D` на плоскость
//!   (драйвер копирует);
//! - `import-cached` — блок из `memfd`, через `/dev/udmabuf` становится dma-buf, плоскости
//!   один раз на блок импортируются в текстуры (EGLImage); на кадр — только сброс кэша CPU;
//! - `import-per-frame` — то же, но udmabuf + EGLImage создаются на каждый кадр и
//!   уничтожаются после fence.
//!
//! «Декодер» здесь — запись CPU в блок (заливка плоскостей значением кадра); её время
//! меряется отдельно и в цену пути не входит. На каждый кадр меряется CPU потока на
//! подготовку (копия / сброс кэша / импорт) и на рисование + показ; отдельной серией — время
//! GPU-прохода одного кадра (`glFinish`). `--verify` читает пиксель цели на каждом кадре и
//! сверяет его с записанными значениями: без сброса кэша на GPU без общего с CPU кэша
//! (Ironlake) со второго круга по блокам будут видны старые данные.
//!
//! Запуск: `cargo run --release -p gles2-egui --example udmabuf_zero_copy_probe -- --mode
//! import-cached --frames 600 [--verify] [--no-flush]`.

#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "стенд: сырые вызовы libc/GL с SAFETY, громкие падения вместо обработки"
)]

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gles2_egui::{
    DmaBufPlaneTexture, DrmFourcc, DrmModifier, Es100ProgramSources, FrameStart, Gles2Context,
    GpuFence, SurfaceSize,
};
use glow::HasContext;

/// `GL_RED_EXT` (`GL_EXT_texture_rg`): одноканальная текстура, как у `gles2-video`.
const GL_RED_EXT: u32 = 0x1903;
/// `GL_UNPACK_ROW_LENGTH_EXT` (`GL_EXT_unpack_subimage`).
const GL_UNPACK_ROW_LENGTH_EXT: u32 = 0x0CF2;
/// `UDMABUF_CREATE = _IOW('u', 0x42, struct udmabuf_create)` (`linux/udmabuf.h`).
const UDMABUF_CREATE_IOCTL: libc::c_ulong = 0x4018_7542;
/// `UDMABUF_FLAGS_CLOEXEC`.
const UDMABUF_FLAGS_CLOEXEC: u32 = 0x01;
/// Строка кэша x86_64: `clflush` сбрасывает ровно одну.
const CACHE_LINE_BYTES: usize = 64;
/// Выравнивание начала плоскости и запас за её концом — как `HostFrameBlockLayout`.
const PLANE_ALIGNMENT: usize = 256;
const PLANE_TAIL_SLACK: usize = 16 + 64;
/// Сколько кадров ждать ответа fence, прежде чем считать стенд зависшим.
const FENCE_TIMEOUT: Duration = Duration::from_secs(5);

/// `struct udmabuf_create`.
#[repr(C)]
struct UdmabufCreateRequest {
    memfd: u32,
    flags: u32,
    offset: u64,
    size: u64,
}

/// Способ подачи кадра в текстуры.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FeedMode {
    Upload,
    ImportCached,
    ImportPerFrame,
}

/// Параметры запуска.
#[derive(Debug, Clone)]
struct ProbeOptions {
    mode: FeedMode,
    frames: usize,
    frame_width: usize,
    frame_height: usize,
    target: SurfaceSize,
    blocks: usize,
    flush_cpu_cache: bool,
    verify: bool,
    flush_range: FlushRange,
    measure_reflush: bool,
    gpu_probe_frames: usize,
}

fn parse_options() -> ProbeOptions {
    let mut options = ProbeOptions {
        mode: FeedMode::Upload,
        frames: 600,
        frame_width: 1920,
        frame_height: 1080,
        target: SurfaceSize::new(1440, 900).unwrap(),
        blocks: 8,
        flush_cpu_cache: true,
        verify: false,
        flush_range: FlushRange::VisibleRows,
        measure_reflush: false,
        gpu_probe_frames: 60,
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .unwrap_or_else(|| panic!("{flag}: нет значения"))
        };
        match flag.as_str() {
            "--mode" => {
                options.mode = match value().as_str() {
                    "upload" => FeedMode::Upload,
                    "import-cached" => FeedMode::ImportCached,
                    "import-per-frame" => FeedMode::ImportPerFrame,
                    other => panic!("неизвестный режим {other}"),
                }
            }
            "--frames" => options.frames = value().parse().unwrap(),
            "--size" => {
                let text = value();
                let (width, height) = text.split_once('x').unwrap();
                options.frame_width = width.parse().unwrap();
                options.frame_height = height.parse().unwrap();
            }
            "--target" => {
                let text = value();
                let (width, height) = text.split_once('x').unwrap();
                options.target =
                    SurfaceSize::new(width.parse().unwrap(), height.parse().unwrap()).unwrap();
            }
            "--blocks" => options.blocks = value().parse().unwrap(),
            "--no-flush" => options.flush_cpu_cache = false,
            "--verify" => options.verify = true,
            "--flush-range" => {
                options.flush_range = match value().as_str() {
                    "visible" => FlushRange::VisibleRows,
                    "block" => FlushRange::WholeBlock,
                    other => panic!("неизвестный диапазон {other}"),
                }
            }
            "--measure-reflush" => options.measure_reflush = true,
            "--gpu-probe" => options.gpu_probe_frames = value().parse().unwrap(),
            other => panic!("неизвестный флаг {other}"),
        }
    }
    options
}

/// Одна плоскость кадра внутри блока.
#[derive(Debug, Clone, Copy)]
struct PlaneLayout {
    offset: usize,
    stride: usize,
    width: usize,
    height: usize,
    /// Строк в блоке (высота, выровненная декодером).
    allocated_rows: usize,
}

impl PlaneLayout {
    fn block_bytes(self) -> usize {
        self.stride * self.allocated_rows
    }
}

/// Раскладка кадра I420 в блоке так, как её строит `direct_frame_buffers` для 8-бит.
#[derive(Debug, Clone, Copy)]
struct FrameLayout {
    planes: [PlaneLayout; 3],
    block_len: usize,
}

/// Нечётное кратное 256, не меньше `natural` (`cache_friendly_linesizes`).
fn cache_friendly_stride(natural: usize) -> usize {
    let mut multiples = natural.div_ceil(PLANE_ALIGNMENT);
    if multiples.is_multiple_of(2) {
        multiples += 1;
    }
    multiples * PLANE_ALIGNMENT
}

fn frame_layout(width: usize, height: usize, page_size: usize) -> FrameLayout {
    // H.264 выравнивает высоту до 16 (1080 → 1088), как `avcodec_align_dimensions2`.
    let allocated_rows = height.next_multiple_of(16);
    let chroma_width = width.div_ceil(2);
    let chroma_height = height.div_ceil(2);
    let luma = PlaneLayout {
        offset: 0,
        stride: cache_friendly_stride(width),
        width,
        height,
        allocated_rows,
    };
    let chroma_stride = cache_friendly_stride(chroma_width);
    let chroma_u = PlaneLayout {
        offset: (luma.offset + luma.block_bytes() + PLANE_TAIL_SLACK)
            .next_multiple_of(PLANE_ALIGNMENT),
        stride: chroma_stride,
        width: chroma_width,
        height: chroma_height,
        allocated_rows: allocated_rows / 2,
    };
    let chroma_v = PlaneLayout {
        offset: (chroma_u.offset + chroma_u.block_bytes() + PLANE_TAIL_SLACK)
            .next_multiple_of(PLANE_ALIGNMENT),
        ..chroma_u
    };
    let end = chroma_v.offset + chroma_v.block_bytes() + PLANE_TAIL_SLACK;
    FrameLayout {
        planes: [luma, chroma_u, chroma_v],
        block_len: end.next_multiple_of(page_size),
    }
}

/// Блок памяти кадра: `memfd` + отображение для записи «декодером».
struct FrameBlock {
    memfd: OwnedFd,
    mapping: *mut u8,
    len: usize,
    /// Fence последнего кадра показа, в котором GPU читал этот блок.
    last_read: Option<Arc<GpuFence>>,
    /// Текстуры импорта этого блока (`import-cached`).
    cached_textures: Option<[glow::NativeTexture; 3]>,
}

impl FrameBlock {
    fn allocate(len: usize) -> Self {
        // SAFETY: имя — статическая C-строка; результат проверяется и сразу идёт во владение.
        let raw = unsafe {
            libc::memfd_create(
                c"udmabuf-probe".as_ptr(),
                libc::MFD_ALLOW_SEALING | libc::MFD_CLOEXEC,
            )
        };
        assert!(
            raw >= 0,
            "memfd_create: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: fd только что создан и больше ничьим владением не является.
        let memfd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: fd валиден; длина помещается в off_t.
        let truncated = unsafe { libc::ftruncate(memfd.as_raw_fd(), len as libc::off_t) };
        assert_eq!(
            truncated,
            0,
            "ftruncate: {}",
            std::io::Error::last_os_error()
        );
        // udmabuf принимает только memfd, который нельзя уменьшить.
        // SAFETY: fd валиден; fcntl владение не забирает.
        let sealed =
            unsafe { libc::fcntl(memfd.as_raw_fd(), libc::F_ADD_SEALS, libc::F_SEAL_SHRINK) };
        assert_eq!(
            sealed,
            0,
            "F_SEAL_SHRINK: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: отображение shared RW всего memfd; результат проверяется, `munmap` в Drop.
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
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
        Self {
            memfd,
            mapping: mapping.cast(),
            len,
            last_read: None,
            cached_textures: None,
        }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: отображение длиной `len` живо, пока жив блок; `&mut self` — единственный
        // писатель на стороне CPU.
        unsafe { std::slice::from_raw_parts_mut(self.mapping, self.len) }
    }

    /// dma-buf над всем блоком.
    fn create_dma_buf(&self, device: &std::fs::File) -> OwnedFd {
        let request = UdmabufCreateRequest {
            memfd: u32::try_from(self.memfd.as_raw_fd()).unwrap(),
            flags: UDMABUF_FLAGS_CLOEXEC,
            offset: 0,
            size: self.len as u64,
        };
        // SAFETY: `request` совпадает с `struct udmabuf_create` и живёт весь вызов.
        let raw = unsafe { libc::ioctl(device.as_raw_fd(), UDMABUF_CREATE_IOCTL, &request) };
        assert!(
            raw >= 0,
            "UDMABUF_CREATE: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: ioctl вернул новый fd, которым больше никто не владеет.
        unsafe { OwnedFd::from_raw_fd(raw) }
    }

    /// Сбросить записанный диапазон из кэша CPU в память (на GPU без общего кэша иначе
    /// видны старые байты).
    fn flush_cpu_cache(&self, ranges: &[(usize, usize)]) {
        // SAFETY: все адреса внутри отображения (диапазоны строятся из раскладки блока);
        // барьеры упорядочивают запись до сброса и сброс до последующей отправки команд GPU.
        unsafe {
            core::arch::x86_64::_mm_mfence();
            for &(start, length) in ranges {
                let first_line = start / CACHE_LINE_BYTES * CACHE_LINE_BYTES;
                let end = (start + length).min(self.len);
                for offset in (first_line..end).step_by(CACHE_LINE_BYTES) {
                    core::arch::x86_64::_mm_clflush(self.mapping.add(offset));
                }
            }
            core::arch::x86_64::_mm_mfence();
        }
    }
}

/// Какие байты блока сбрасывать из кэша CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlushRange {
    /// Только видимые строки плоскостей (то, что записал декодер и читает GPU).
    VisibleRows,
    /// Весь блок от начала до конца последней плоскости.
    WholeBlock,
}

fn flush_ranges(layout: &FrameLayout, range: FlushRange) -> Vec<(usize, usize)> {
    match range {
        FlushRange::WholeBlock => {
            vec![(0, layout.planes[2].offset + layout.planes[2].block_bytes())]
        }
        FlushRange::VisibleRows => layout
            .planes
            .iter()
            .flat_map(|plane| {
                (0..plane.height).map(move |row| (plane.offset + row * plane.stride, plane.width))
            })
            .collect(),
    }
}

/// user и system время текущего потока.
fn thread_user_system_time() -> (Duration, Duration) {
    // SAFETY: нулевая `rusage` — допустимый выходной буфер.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` — корректный выходной буфер.
    let result = unsafe { libc::getrusage(libc::RUSAGE_THREAD, &mut usage) };
    assert_eq!(result, 0);
    let to_duration = |time: libc::timeval| {
        Duration::from_secs(time.tv_sec as u64) + Duration::from_micros(time.tv_usec as u64)
    };
    (to_duration(usage.ru_utime), to_duration(usage.ru_stime))
}

impl Drop for FrameBlock {
    fn drop(&mut self) {
        // SAFETY: отображение создано в `allocate` с этой длиной и больше не используется.
        unsafe { libc::munmap(self.mapping.cast(), self.len) };
    }
}

/// Значения плоскостей кадра `index`: разные у Y/U/V и меняются каждый кадр.
fn frame_values(index: usize) -> [u8; 3] {
    let base = (index * 37) % 200;
    [
        (base + 20) as u8,
        (base * 3 % 200 + 30) as u8,
        (base * 7 % 200 + 25) as u8,
    ]
}

/// «Декодер»: записать видимые строки всех плоскостей значением кадра.
fn produce_frame(bytes: &mut [u8], layout: &FrameLayout, values: [u8; 3]) {
    for (plane, value) in layout.planes.iter().zip(values) {
        for row in 0..plane.height {
            let start = plane.offset + row * plane.stride;
            bytes[start..start + plane.width].fill(value);
        }
    }
}

/// CPU-время текущего потока.
fn thread_cpu_time() -> Duration {
    let mut spec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `spec` — корректный выходной буфер.
    let result = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut spec) };
    assert_eq!(result, 0);
    Duration::new(spec.tv_sec as u64, spec.tv_nsec as u32)
}

/// Программа: три R8-плоскости → (y, u, v) в цвет без преобразования (для сверки пикселя).
struct ProbeProgram {
    program: glow::NativeProgram,
    vertex_buffer: glow::NativeBuffer,
    position_attribute: u32,
}

const VERTEX_SHADER: &str = "#version 100
attribute vec2 a_position;
varying vec2 v_uv;
void main() {
    v_uv = vec2(a_position.x * 0.5 + 0.5, 0.5 - a_position.y * 0.5);
    gl_Position = vec4(a_position, 0.0, 1.0);
}";

const FRAGMENT_SHADER: &str = "#version 100
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_luma;
uniform sampler2D u_chroma_u;
uniform sampler2D u_chroma_v;
void main() {
    gl_FragColor = vec4(texture2D(u_luma, v_uv).r, texture2D(u_chroma_u, v_uv).r,
                        texture2D(u_chroma_v, v_uv).r, 1.0);
}";

impl ProbeProgram {
    fn new(context: &Gles2Context) -> Self {
        let program = context
            .compile_es100_program(Es100ProgramSources {
                vertex: VERTEX_SHADER,
                fragment: FRAGMENT_SHADER,
            })
            .unwrap();
        let gl = context.gl().unwrap();
        // SAFETY: контекст текущий; объекты создаются и настраиваются по спецификации ES 2.0.
        unsafe {
            let position_attribute = gl.get_attrib_location(program, "a_position").unwrap();
            let vertex_buffer = gl.create_buffer().unwrap();
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vertex_buffer));
            let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
            let quad_bytes: Vec<u8> = quad.iter().flat_map(|value| value.to_ne_bytes()).collect();
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &quad_bytes, glow::STATIC_DRAW);
            gl.use_program(Some(program));
            for (unit, name) in ["u_luma", "u_chroma_u", "u_chroma_v"].iter().enumerate() {
                let location = gl.get_uniform_location(program, name);
                gl.uniform_1_i32(location.as_ref(), unit as i32);
            }
            Self {
                program,
                vertex_buffer,
                position_attribute,
            }
        }
    }

    fn draw(&self, gl: &glow::Context, textures: &[glow::NativeTexture; 3]) {
        // SAFETY: кадр начат (контекст текущий), текстуры живы.
        unsafe {
            gl.use_program(Some(self.program));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vertex_buffer));
            gl.enable_vertex_attrib_array(self.position_attribute);
            gl.vertex_attrib_pointer_f32(self.position_attribute, 2, glow::FLOAT, false, 0, 0);
            for (unit, texture) in textures.iter().enumerate() {
                gl.active_texture(glow::TEXTURE0 + unit as u32);
                gl.bind_texture(glow::TEXTURE_2D, Some(*texture));
            }
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
    }
}

/// Фильтр и wrap: без них текстура на EGLImage неполная (чёрный кадр).
fn set_sampling(gl: &glow::Context, texture: glow::NativeTexture) {
    // SAFETY: контекст текущий, текстура жива.
    unsafe {
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
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
    }
}

/// Импортировать три плоскости блока в новые текстуры.
fn import_block_textures(
    context: &Gles2Context,
    dma_buf: &OwnedFd,
    layout: &FrameLayout,
) -> [glow::NativeTexture; 3] {
    let textures = {
        let gl = context.gl().unwrap();
        // SAFETY: контекст текущий.
        [0, 1, 2].map(|_| unsafe { gl.create_texture() }.unwrap())
    };
    for (texture, plane) in textures.iter().zip(layout.planes) {
        context
            .attach_dma_buf_plane_to_texture(
                *texture,
                &DmaBufPlaneTexture {
                    fourcc: DrmFourcc::R8,
                    width: plane.width as u32,
                    height: plane.height as u32,
                    fd: dma_buf.as_fd(),
                    offset: plane.offset as u32,
                    pitch: plane.stride as u32,
                    modifier: DrmModifier::LINEAR,
                },
            )
            .unwrap_or_else(|error| panic!("импорт плоскости: {error:?}"));
        set_sampling(context.gl().unwrap(), *texture);
    }
    textures
}

/// Текстуры загрузки (`upload`): кольцо слотов, как `UploadRing`.
struct UploadSlot {
    textures: [glow::NativeTexture; 3],
    last_read: Option<Arc<GpuFence>>,
}

fn create_upload_slot(gl: &glow::Context, layout: &FrameLayout) -> UploadSlot {
    let textures = layout.planes.map(|plane| {
        // SAFETY: контекст текущий; хранилище выделяется без данных.
        unsafe {
            let texture = gl.create_texture().unwrap();
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                GL_RED_EXT as i32,
                plane.width as i32,
                plane.height as i32,
                0,
                GL_RED_EXT,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            texture
        }
    });
    for texture in textures {
        set_sampling(gl, texture);
    }
    UploadSlot {
        textures,
        last_read: None,
    }
}

/// Одна копия на плоскость: `glTexSubImage2D` с длиной строки = шагу источника.
fn upload_planes(gl: &glow::Context, slot: &UploadSlot, bytes: &[u8], layout: &FrameLayout) {
    // SAFETY: контекст текущий; срез покрывает все читаемые строки (проверено индексами).
    unsafe {
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        for (texture, plane) in slot.textures.iter().zip(layout.planes) {
            let end = plane.offset + (plane.height - 1) * plane.stride + plane.width;
            gl.bind_texture(glow::TEXTURE_2D, Some(*texture));
            gl.pixel_store_i32(GL_UNPACK_ROW_LENGTH_EXT, plane.stride as i32);
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                plane.width as i32,
                plane.height as i32,
                GL_RED_EXT,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&bytes[plane.offset..end])),
            );
        }
        gl.pixel_store_i32(GL_UNPACK_ROW_LENGTH_EXT, 0);
    }
}

fn wait_fence(fence: Option<Arc<GpuFence>>) {
    if let Some(fence) = fence {
        let status = fence.wait(FENCE_TIMEOUT).unwrap();
        assert_eq!(
            status,
            gles2_egui::FenceStatus::Signaled,
            "fence не сработал за 5 с"
        );
    }
}

/// Сводка ряда длительностей в миллисекундах.
fn summary(name: &str, values: &mut [Duration]) -> String {
    values.sort();
    let total: Duration = values.iter().sum();
    let mean = total.as_secs_f64() * 1000.0 / values.len().max(1) as f64;
    let at = |fraction: f64| {
        let index = ((values.len() as f64 - 1.0) * fraction).round() as usize;
        values
            .get(index)
            .map_or(0.0, |value| value.as_secs_f64() * 1000.0)
    };
    format!(
        "\"{name}\":{{\"mean\":{mean:.3},\"p50\":{:.3},\"p90\":{:.3},\"max\":{:.3}}}",
        at(0.5),
        at(0.9),
        at(1.0)
    )
}

/// Подготовленные к рисованию текстуры кадра и то, что надо отпустить после fence.
struct PreparedFrame {
    textures: [glow::NativeTexture; 3],
    transient: Option<(OwnedFd, [glow::NativeTexture; 3])>,
}

fn main() {
    let options = parse_options();
    // SAFETY: `sysconf` только читает системную константу.
    let page_size = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
    let layout = frame_layout(options.frame_width, options.frame_height, page_size);
    let flush_ranges = flush_ranges(&layout, options.flush_range);
    let mut reflush_times = Vec::new();
    let mut context = Gles2Context::offscreen(options.target).unwrap();
    eprintln!(
        "renderer: {:?}; layout: {:?}",
        context.capabilities().tiers(),
        layout
    );
    let program = ProbeProgram::new(&context);
    let udmabuf_device = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/udmabuf")
        .unwrap_or_else(|error| panic!("/dev/udmabuf: {error}"));

    let mut blocks: Vec<FrameBlock> = (0..options.blocks)
        .map(|_| FrameBlock::allocate(layout.block_len))
        .collect();
    // Для `upload` источник — обычная анонимная память, как блоки декодера сейчас.
    let mut anonymous_blocks: Vec<Vec<u8>> = (0..options.blocks)
        .map(|_| vec![0u8; layout.block_len])
        .collect();
    let mut upload_slots: Vec<UploadSlot> = (0..3)
        .map(|_| create_upload_slot(context.gl().unwrap(), &layout))
        .collect();
    if options.mode == FeedMode::ImportCached {
        for block in &mut blocks {
            let dma_buf = block.create_dma_buf(&udmabuf_device);
            block.cached_textures = Some(import_block_textures(&context, &dma_buf, &layout));
        }
    }

    let mut produce_times = Vec::with_capacity(options.frames);
    let mut prepare_times = Vec::with_capacity(options.frames);
    let mut draw_times = Vec::with_capacity(options.frames);
    let mut pending_transient: Vec<(Arc<GpuFence>, OwnedFd, [glow::NativeTexture; 3])> = Vec::new();
    let mut mismatches = 0usize;
    let mut first_mismatch: Option<String> = None;
    let loop_started_cpu = thread_cpu_time();
    let (loop_started_user, loop_started_system) = thread_user_system_time();
    let loop_started = Instant::now();

    for frame_index in 0..options.frames {
        let block_index = frame_index % options.blocks;
        let values = frame_values(frame_index);

        // 1. «Декодер» пишет в блок, только когда GPU его дочитал (правило 30).
        let produce_started = thread_cpu_time();
        wait_fence(blocks[block_index].last_read.take());
        match options.mode {
            FeedMode::Upload => produce_frame(&mut anonymous_blocks[block_index], &layout, values),
            _ => produce_frame(blocks[block_index].bytes_mut(), &layout, values),
        }
        produce_times.push(thread_cpu_time() - produce_started);

        // 2. Подготовка текстур — то, что сравниваем.
        let prepare_started = thread_cpu_time();
        let prepared = match options.mode {
            FeedMode::Upload => {
                let slot = &mut upload_slots[frame_index % 3];
                wait_fence(slot.last_read.take());
                upload_planes(
                    context.gl().unwrap(),
                    slot,
                    &anonymous_blocks[block_index],
                    &layout,
                );
                PreparedFrame {
                    textures: slot.textures,
                    transient: None,
                }
            }
            FeedMode::ImportCached => {
                let block = &blocks[block_index];
                if options.flush_cpu_cache {
                    block.flush_cpu_cache(&flush_ranges);
                }
                PreparedFrame {
                    textures: block.cached_textures.unwrap(),
                    transient: None,
                }
            }
            FeedMode::ImportPerFrame => {
                let block = &blocks[block_index];
                if options.flush_cpu_cache {
                    block.flush_cpu_cache(&flush_ranges);
                }
                let dma_buf = block.create_dma_buf(&udmabuf_device);
                let textures = import_block_textures(&context, &dma_buf, &layout);
                PreparedFrame {
                    textures,
                    transient: Some((dma_buf, textures)),
                }
            }
        };
        prepare_times.push(thread_cpu_time() - prepare_started);
        // Цена сброса уже чистых строк (нижняя граница: в настоящем декодере часть кадра к
        // моменту сброса уже вытеснена из кэша). В цену пути не входит.
        if options.measure_reflush && options.mode != FeedMode::Upload {
            let reflush_started = thread_cpu_time();
            blocks[block_index].flush_cpu_cache(&flush_ranges);
            reflush_times.push(thread_cpu_time() - reflush_started);
        }

        // 3. Рисование и показ + fence кадра.
        let draw_started = thread_cpu_time();
        match context.begin_frame().unwrap() {
            FrameStart::Ready(active) => {
                program.draw(active.gl(), &prepared.textures);
                active.present().unwrap();
            }
            FrameStart::Skipped(reason) => panic!("кадр пропущен: {reason:?}"),
        }
        let fence = Arc::new(context.insert_fence().unwrap());
        draw_times.push(thread_cpu_time() - draw_started);

        // 4. Сверка пикселя (вне замеров по смыслу: дорогое чтение цели).
        if options.verify {
            let pixels = context.read_offscreen_pixels().unwrap();
            let size = pixels.size();
            let pixel = pixels.pixel(size.width() / 2, size.height() / 2).unwrap();
            let matches = pixel[..3]
                .iter()
                .zip(values)
                .all(|(seen, expected)| seen.abs_diff(expected) <= 1);
            if !matches {
                mismatches += 1;
                first_mismatch.get_or_insert_with(|| {
                    format!("frame {frame_index}: seen {pixel:?}, expected {values:?}")
                });
            }
        }

        // Fence держит и блок, и слот загрузки, и временный импорт.
        // У `upload` источник свободен сразу после `glTexSubImage2D` (ES 2.0 §3.7.1), а
        // текстуры слота — после fence.
        if options.mode == FeedMode::Upload {
            upload_slots[frame_index % 3].last_read = Some(Arc::clone(&fence));
        }
        if let Some((dma_buf, textures)) = prepared.transient {
            pending_transient.push((Arc::clone(&fence), dma_buf, textures));
        }
        if options.mode != FeedMode::Upload {
            blocks[block_index].last_read = Some(fence);
        }
        release_transient(&context, &mut pending_transient, false);
    }
    release_transient(&context, &mut pending_transient, true);
    let loop_wall = loop_started.elapsed();
    let loop_cpu = thread_cpu_time() - loop_started_cpu;
    let (loop_user, loop_system) = thread_user_system_time();
    let (loop_user, loop_system) = (
        loop_user - loop_started_user,
        loop_system - loop_started_system,
    );

    // 5. Время GPU-прохода одного кадра: рисование уже готовых текстур до `glFinish`.
    let gpu_draw_times = probe_gpu_draw(&mut context, &program, &blocks, &upload_slots, &options);

    let produce_total: Duration = produce_times.iter().sum();
    println!(
        "RESULT {{\"mode\":\"{:?}\",\"frames\":{},\"flush\":{},\"verify\":{},\"mismatches\":{},\"fps\":{:.1},\"loop_cpu_per_frame_ms\":{:.3},\"path_cpu_per_frame_ms\":{:.3},{},{},{},{},\"loop_user_per_frame_ms\":{:.3},\"loop_system_per_frame_ms\":{:.3},\"flush_range\":\"{:?}\",{},\"dma_buf_textures\":{:?}}}",
        options.mode,
        options.frames,
        options.flush_cpu_cache,
        options.verify,
        mismatches,
        options.frames as f64 / loop_wall.as_secs_f64(),
        loop_cpu.as_secs_f64() * 1000.0 / options.frames as f64,
        (loop_cpu.saturating_sub(produce_total)).as_secs_f64() * 1000.0 / options.frames as f64,
        summary("produce_cpu_ms", &mut produce_times),
        summary("prepare_cpu_ms", &mut prepare_times),
        summary("draw_cpu_ms", &mut draw_times),
        summary("gpu_draw_wall_ms", &mut gpu_draw_times.clone()),
        loop_user.as_secs_f64() * 1000.0 / options.frames as f64,
        loop_system.as_secs_f64() * 1000.0 / options.frames as f64,
        options.flush_range,
        summary("reflush_cpu_ms", &mut reflush_times),
        context.dma_buf_texture_stats(),
    );
    if let Some(first) = first_mismatch {
        println!("FIRST_MISMATCH {first}");
    }
}

/// Отпустить временные импорты, чей fence сработал (или все — в конце).
fn release_transient(
    context: &Gles2Context,
    pending: &mut Vec<(Arc<GpuFence>, OwnedFd, [glow::NativeTexture; 3])>,
    wait_all: bool,
) {
    pending.retain(|(fence, _, textures)| {
        let done = if wait_all {
            fence.wait(FENCE_TIMEOUT).unwrap() == gles2_egui::FenceStatus::Signaled
        } else {
            fence.poll().unwrap() == gles2_egui::FenceStatus::Signaled
        };
        if done {
            let gl = context.gl().unwrap();
            for texture in textures {
                // SAFETY: GPU дочитал текстуру (fence сработал); контекст текущий.
                unsafe { gl.delete_texture(*texture) };
            }
        }
        !done
    });
}

/// Серия «нарисовать кадр и дождаться GPU» по уже готовым текстурам режима.
fn probe_gpu_draw(
    context: &mut Gles2Context,
    program: &ProbeProgram,
    blocks: &[FrameBlock],
    upload_slots: &[UploadSlot],
    options: &ProbeOptions,
) -> Vec<Duration> {
    let textures = match options.mode {
        FeedMode::Upload => upload_slots[0].textures,
        FeedMode::ImportCached => blocks[0].cached_textures.unwrap(),
        FeedMode::ImportPerFrame => return Vec::new(),
    };
    let mut times = Vec::with_capacity(options.gpu_probe_frames);
    for _ in 0..options.gpu_probe_frames {
        let started = Instant::now();
        match context.begin_frame().unwrap() {
            FrameStart::Ready(active) => {
                program.draw(active.gl(), &textures);
                // SAFETY: кадр начат, контекст текущий.
                unsafe { active.gl().finish() };
                active.present().unwrap();
            }
            FrameStart::Skipped(reason) => panic!("кадр пропущен: {reason:?}"),
        }
        times.push(started.elapsed());
    }
    times
}
