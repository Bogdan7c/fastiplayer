//! GPU-тест полного пути «DMA-BUF с известным содержимым → production импорт →
//! production NV12 рендер → readback».
//!
//! Настоящий DMA-BUF создаётся без VA-API: ядро (`/dev/udmabuf`) оборачивает
//! обычную память `memfd` в dma-buf fd. Такой буфер всегда linear
//! (`DRM_FORMAT_MOD_LINEAR`), поэтому тест покрывает multi-planar путь с
//! `VK_IMAGE_TILING_LINEAR`; тайловый путь VA-API проверяет playback smoke.
//!
//! Тест доказывает сохранность содержимого, но НЕ чистоту под validation layer:
//! RADV не объявляет DMA-BUF импорт для `VK_IMAGE_TILING_LINEAR` NV12
//! (`VUID-VkImageCreateInfo-pNext-00990`, `...-handleType-09861`). Это
//! известный дефект LINEAR-пути (правильнее DRM modifier tiling с
//! `DRM_FORMAT_MOD_LINEAR`), вынесенный в бэклог сессии 03 обновления egui/wgpu.
//!
//! Тест помечен `#[ignore]`: ему нужны Vulkan GPU с NV12 + DMA-BUF и доступ к
//! `/dev/udmabuf`. Запуск:
//! `cargo test -p render-wgpu-video --locked udmabuf -- --ignored`.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::mpsc;
use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use render_core::RenderViewport;
use video_core::{
    DecodedFrame, DmaBufFrameDescriptor, DmaBufFrameExportLayout, DmaBufLayerDescriptor,
    DmaBufObjectDescriptor, DmaBufObjectIdentity, FrameResourceHandle, VideoFrameDiagnostics,
};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use super::{DRM_FORMAT_MOD_LINEAR, DRM_FORMAT_NV12, DmaBufImporter};
use crate::{
    WgpuRenderableFrame, WgpuVideoRenderInput, WgpuVideoRenderer,
    required_wgpu_video_texture_features,
};

/// Ширина тестового кадра; кратна 256, чтобы linear row pitch подходил любому драйверу.
const FRAME_WIDTH: u32 = 256;

/// Высота тестового кадра (чётная для 4:2:0).
const FRAME_HEIGHT: u32 = 64;

/// Формат offscreen target-а (без sRGB, чтобы readback был прямыми байтами shader-а).
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Предел ожидания GPU в тесте.
const GPU_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Путь к kernel udmabuf device.
const UDMABUF_DEVICE_PATH: &str = "/dev/udmabuf";

/// `UDMABUF_CREATE = _IOW('u', 0x42, struct udmabuf_create)`, размер struct 24 байта
/// (`linux/udmabuf.h`).
const UDMABUF_CREATE_IOCTL: libc::c_ulong = 0x4018_7542;

/// `UDMABUF_FLAGS_CLOEXEC` из `linux/udmabuf.h`.
const UDMABUF_FLAGS_CLOEXEC: u32 = 0x01;

/// Аргумент ioctl `UDMABUF_CREATE` (раскладка как `struct udmabuf_create`).
#[repr(C)]
struct UdmabufCreateRequest {
    /// memfd с печатью `F_SEAL_SHRINK`.
    memfd: u32,
    /// Флаги создаваемого dma-buf fd.
    flags: u32,
    /// Смещение внутри memfd (кратно странице).
    offset: u64,
    /// Размер dma-buf (кратен странице).
    size: u64,
}

/// Цвет в BT.709 limited range: (Y, Cb, Cr).
#[derive(Debug, Clone, Copy)]
struct LimitedYCbCr {
    /// Luma, 16..=235.
    luma: u8,
    /// Синяя цветоразность, 16..=240.
    blue_difference: u8,
    /// Красная цветоразность, 16..=240.
    red_difference: u8,
}

/// Чистый красный (1, 0, 0) в BT.709 limited range.
const BT709_LIMITED_RED: LimitedYCbCr = LimitedYCbCr {
    luma: 63,
    blue_difference: 102,
    red_difference: 240,
};

/// Чистый синий (0, 0, 1) в BT.709 limited range.
const BT709_LIMITED_BLUE: LimitedYCbCr = LimitedYCbCr {
    luma: 32,
    blue_difference: 240,
    red_difference: 118,
};

/// Известный NV12 кадр: левая половина красная, правая синяя.
///
/// Раскладка как у VA-API composed export: Y plane `FRAME_WIDTH x FRAME_HEIGHT`
/// с pitch = ширине, сразу за ней interleaved CbCr plane половинного размера.
fn split_red_blue_nv12_frame() -> Vec<u8> {
    let half_width = FRAME_WIDTH / 2;
    let color_for_column = |column: u32| {
        if column < half_width {
            BT709_LIMITED_RED
        } else {
            BT709_LIMITED_BLUE
        }
    };

    let mut frame_bytes = Vec::new();
    for _row in 0..FRAME_HEIGHT {
        for column in 0..FRAME_WIDTH {
            frame_bytes.push(color_for_column(column).luma);
        }
    }
    for _chroma_row in 0..FRAME_HEIGHT / 2 {
        for chroma_column in 0..FRAME_WIDTH / 2 {
            // Один CbCr sample покрывает две luma колонки.
            let color = color_for_column(chroma_column * 2);
            frame_bytes.push(color.blue_difference);
            frame_bytes.push(color.red_difference);
        }
    }
    frame_bytes
}

/// Создаёт настоящий DMA-BUF fd с содержимым `frame_bytes` через memfd + udmabuf.
fn create_udmabuf_with_contents(frame_bytes: &[u8]) -> (OwnedFd, u32) {
    // SAFETY: `sysconf` только читает системную константу.
    let page_size = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
        .expect("page size must be positive");
    let content_size = u64::try_from(frame_bytes.len()).expect("frame size fits u64");
    let buffer_size = content_size.div_ceil(page_size) * page_size;

    // SAFETY: имя — валидная C-строка со статическим временем жизни; результат
    // проверяется на ошибку и сразу передаётся во владение `OwnedFd`.
    let raw_memfd = unsafe {
        libc::memfd_create(
            c"fastiplayer-udmabuf-test".as_ptr(),
            libc::MFD_ALLOW_SEALING,
        )
    };
    assert!(
        raw_memfd >= 0,
        "memfd_create failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `raw_memfd` только что создан и больше никем не владеется.
    let memfd = unsafe { OwnedFd::from_raw_fd(raw_memfd) };

    let mut memfd_file = std::fs::File::from(memfd);
    memfd_file.set_len(buffer_size).expect("resize memfd");
    std::io::Write::write_all(&mut memfd_file, frame_bytes).expect("write NV12 frame into memfd");

    // udmabuf принимает только memfd, который нельзя уменьшить.
    // SAFETY: fd валиден на время вызова; fcntl не забирает владение.
    let seal_result = unsafe {
        libc::fcntl(
            memfd_file.as_raw_fd(),
            libc::F_ADD_SEALS,
            libc::F_SEAL_SHRINK,
        )
    };
    assert_eq!(
        seal_result,
        0,
        "F_SEAL_SHRINK failed: {}",
        std::io::Error::last_os_error()
    );

    let udmabuf_device = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(UDMABUF_DEVICE_PATH)
        .expect("open /dev/udmabuf (нужен доступ пользователя к устройству)");
    let create_request = UdmabufCreateRequest {
        memfd: u32::try_from(memfd_file.as_raw_fd()).expect("memfd is non-negative"),
        flags: UDMABUF_FLAGS_CLOEXEC,
        offset: 0,
        size: buffer_size,
    };
    // SAFETY: `create_request` имеет раскладку `struct udmabuf_create` и живёт до
    // конца вызова; оба fd валидны. Ioctl возвращает новый dma-buf fd или -1.
    let raw_dma_buf_fd = unsafe {
        libc::ioctl(
            udmabuf_device.as_raw_fd(),
            UDMABUF_CREATE_IOCTL,
            &create_request,
        )
    };
    assert!(
        raw_dma_buf_fd >= 0,
        "UDMABUF_CREATE failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: ioctl вернул новый fd, которым больше никто не владеет. memfd можно
    // закрыть: dma-buf держит ссылку на его страницы.
    let dma_buf_fd = unsafe { OwnedFd::from_raw_fd(raw_dma_buf_fd) };

    (
        dma_buf_fd,
        u32::try_from(buffer_size).expect("test buffer fits u32"),
    )
}

/// Descriptor в форме VA-API composed NV12 export с linear modifier.
fn composed_linear_nv12_descriptor(dma_buf_fd: OwnedFd, object_size: u32) -> DmaBufFrameDescriptor {
    DmaBufFrameDescriptor {
        resource_id: 3,
        fourcc: DRM_FORMAT_NV12,
        export_layout: DmaBufFrameExportLayout::ComposedLayers,
        width: FRAME_WIDTH,
        height: FRAME_HEIGHT,
        objects: vec![DmaBufObjectDescriptor {
            fd: dma_buf_fd,
            size: object_size,
            drm_format_modifier: DRM_FORMAT_MOD_LINEAR,
            // Identity используется только для диагностики, импорт её не читает.
            identity: DmaBufObjectIdentity {
                device: 0,
                inode: 0,
                special_device: 0,
            },
        }],
        layers: vec![DmaBufLayerDescriptor {
            drm_format: DRM_FORMAT_NV12,
            num_planes: 2,
            object_index: [0, 0, 0, 0],
            offset: [0, FRAME_WIDTH * FRAME_HEIGHT, 0, 0],
            pitch: [FRAME_WIDTH, FRAME_WIDTH, 0, 0],
        }],
    }
}

/// Decoded frame metadata, соответствующая descriptor-у.
fn decoded_nv12_frame() -> DecodedFrame {
    DecodedFrame {
        generation: 1,
        pts: Duration::ZERO,
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
        width: FRAME_WIDTH,
        height: FRAME_HEIGHT,
        render_width: FRAME_WIDTH,
        render_height: FRAME_HEIGHT,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle: FrameResourceHandle(3),
        diagnostics: VideoFrameDiagnostics::default(),
    }
}

/// Vulkan device с теми же video features, что запрашивает production shell.
struct GpuTestContext {
    /// Instance нужен импортёру для raw Vulkan запросов.
    instance: wgpu::Instance,
    /// Adapter нужен импортёру для memory properties.
    adapter: wgpu::Adapter,
    /// Device, на котором импортируется и рисуется кадр.
    device: wgpu::Device,
    /// Queue для submit-а рендера и readback-а.
    queue: wgpu::Queue,
}

impl GpuTestContext {
    /// Создаёт Vulkan device либо падает с понятной причиной.
    fn new() -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            // `with_env` позволяет запуску с `WGPU_DEBUG=0 WGPU_VALIDATION=0` и
            // `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation` получить сообщения
            // validation layer прямо в stdout (без debug messenger-а wgpu).
            flags: wgpu::InstanceFlags::from_build_config().with_env(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("нужен Vulkan adapter для udmabuf GPU-теста");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("udmabuf import render test device"),
            required_features: required_wgpu_video_texture_features(&adapter),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .expect("создать Vulkan device с NV12 feature");

        Self {
            instance,
            adapter,
            device,
            queue,
        }
    }
}

/// Offscreen target и readback buffer одного кадра.
struct OffscreenTarget {
    /// Texture, в которую рисует renderer.
    texture: wgpu::Texture,
    /// View этой texture для render pass-а.
    view: wgpu::TextureView,
    /// MAP_READ buffer для копии target-а.
    readback_buffer: wgpu::Buffer,
    /// Выровненный по `COPY_BYTES_PER_ROW_ALIGNMENT` stride строки.
    padded_bytes_per_row: u32,
}

impl OffscreenTarget {
    /// Создаёт target размера кадра.
    fn new(device: &wgpu::Device) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("udmabuf test target"),
            size: frame_extent(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let padded_bytes_per_row = (FRAME_WIDTH * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("udmabuf test readback"),
            size: u64::from(padded_bytes_per_row) * u64::from(FRAME_HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        Self {
            texture,
            view,
            readback_buffer,
            padded_bytes_per_row,
        }
    }

    /// Записывает копию target-а в readback buffer.
    fn record_copy_to_readback(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: Some(FRAME_HEIGHT),
                },
            },
            frame_extent(),
        );
    }

    /// Ждёт submit и возвращает RGBA пиксель (x, y) из readback-а.
    fn read_pixels(
        &self,
        device: &wgpu::Device,
        submission_index: wgpu::SubmissionIndex,
        pixel_positions: &[(u32, u32)],
    ) -> Vec<[u8; 4]> {
        let readback_slice = self.readback_buffer.slice(..);
        let (mapping_sender, mapping_receiver) = mpsc::sync_channel(1);
        readback_slice.map_async(wgpu::MapMode::Read, move |mapping_result| {
            mapping_sender
                .send(mapping_result)
                .expect("передать результат map_async");
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission_index),
                timeout: Some(GPU_WAIT_TIMEOUT),
            })
            .expect("дождаться GPU submit-а");
        mapping_receiver
            .recv_timeout(GPU_WAIT_TIMEOUT)
            .expect("получить map callback")
            .expect("map readback buffer");

        let mapped_bytes = readback_slice
            .get_mapped_range()
            .expect("получить mapped range readback-а");
        let pixels = pixel_positions
            .iter()
            .map(|&(x, y)| {
                let byte_offset = usize::try_from(y * self.padded_bytes_per_row + x * 4)
                    .expect("pixel offset fits usize");
                let mut rgba = [0_u8; 4];
                rgba.copy_from_slice(&mapped_bytes[byte_offset..byte_offset + 4]);
                rgba
            })
            .collect();
        drop(mapped_bytes);
        self.readback_buffer.unmap();
        pixels
    }
}

/// Размер кадра и target-а.
const fn frame_extent() -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: FRAME_WIDTH,
        height: FRAME_HEIGHT,
        depth_or_array_layers: 1,
    }
}

/// Проверяет, что канал `dominant` яркий, а два других тёмные.
fn assert_dominant_channel(pixel: [u8; 4], dominant: usize, description: &str) {
    const BRIGHT_MIN: u8 = 200;
    const DARK_MAX: u8 = 60;
    for (channel, &value) in pixel[..3].iter().enumerate() {
        if channel == dominant {
            assert!(
                value >= BRIGHT_MIN,
                "{description}: channel {channel} too dark in {pixel:?}"
            );
        } else {
            assert!(
                value <= DARK_MAX,
                "{description}: channel {channel} too bright in {pixel:?}"
            );
        }
    }
}

/// Известный кадр из настоящего DMA-BUF проходит production импорт и рендер без
/// потери содержимого: слева красный, справа синий, а не нули/мусор.
#[test]
#[ignore = "нужны Vulkan GPU с NV12 + DMA-BUF и доступ к /dev/udmabuf"]
fn udmabuf_nv12_frame_survives_import_and_renders_known_colors() {
    let gpu = GpuTestContext::new();
    let (dma_buf_fd, object_size) = create_udmabuf_with_contents(&split_red_blue_nv12_frame());
    let descriptor = composed_linear_nv12_descriptor(dma_buf_fd, object_size);

    let importer = DmaBufImporter::new(
        gpu.device.clone(),
        gpu.instance.clone(),
        gpu.adapter.clone(),
    );
    let imported = importer
        .import_exported_dma_buf_image(&descriptor)
        .expect("production импорт linear NV12 DMA-BUF");
    let decoded_frame = decoded_nv12_frame();
    let renderable_frame =
        WgpuRenderableFrame::from_decoded_nv12(&decoded_frame, &imported.y_view, &imported.uv_view)
            .expect("собрать renderable NV12 frame");

    let target = OffscreenTarget::new(&gpu.device);
    let mut renderer = WgpuVideoRenderer::new(&gpu.device, TARGET_FORMAT);
    renderer.resize(FRAME_WIDTH, FRAME_HEIGHT);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("udmabuf test encoder"),
        });
    let drew_video = renderer
        .render_or_clear(WgpuVideoRenderInput {
            frame: Some(&renderable_frame),
            video_viewport: RenderViewport {
                x: 0,
                y: 0,
                width: FRAME_WIDTH,
                height: FRAME_HEIGHT,
            },
            video_exclusion_rects: &[],
            target: &target.view,
            encoder: &mut encoder,
            device: &gpu.device,
            queue: &gpu.queue,
        })
        .expect("renderer принимает импортированный кадр");
    assert!(drew_video, "кадр должен быть нарисован, а не только очищен");
    target.record_copy_to_readback(&mut encoder);
    let submission_index = gpu.queue.submit([encoder.finish()]);

    // Точки в середине каждой половины, вдали от границы цветов.
    let left_center = (FRAME_WIDTH / 4, FRAME_HEIGHT / 2);
    let right_center = (FRAME_WIDTH * 3 / 4, FRAME_HEIGHT / 2);
    let pixels = target.read_pixels(&gpu.device, submission_index, &[left_center, right_center]);

    assert_dominant_channel(pixels[0], 0, "левая половина должна быть красной");
    assert_dominant_channel(pixels[1], 2, "правая половина должна быть синей");
}
