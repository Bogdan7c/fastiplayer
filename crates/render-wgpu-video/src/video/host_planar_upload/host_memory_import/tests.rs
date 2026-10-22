//! GPU-тесты загрузки кадра прямо из памяти декодера (cpu-parity 08b).
//!
//! Настоящая цепочка: провайдер отдаёт кадр, plane-ы которого лежат в блоке обычной
//! памяти, выровненном по странице, → `HostPlanarWgpuFrameMaterializer` импортирует блок
//! (`VK_EXT_external_memory_host`) и пишет GPU-копию → `WgpuVideoRenderer` → submit →
//! чтение пикселя. Проверяется видимый результат (белый кадр белый), счётчики путей
//! (CPU-копии не было) и время жизни: кадр отпускается только после GPU-копии.
//!
//! Нужен Vulkan-адаптер с `VK_EXT_external_memory_host`. Без него тест импорта
//! пропускается (как остальные GPU-тесты crate-а), а тест запасного пути — нет.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use render_core::RenderViewport;
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
    PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup,
};
use video_core::{
    DecodedFrame, FrameResourceDescriptor, FrameResourceHandle, HOST_IMPORTABLE_BLOCK_ALIGNMENT,
    HostImportableMemoryBlock, HostMemoryBlockId, HostMemoryRegion, HostPlanarFrameDescriptor,
    HostPlanarFrameOwner, HostPlanarImportableMemory, HostPlaneDescriptor, HostPlaneRole,
    VideoFrameDiagnostics,
};
use video_frame_contract::VideoFrameContract;

use super::*;
use crate::video::{
    HostPlanarWgpuFrameMaterializer, HostPlanarWgpuTextureViewLookup, HostPlanarWgpuTextureViews,
    WgpuRenderableFrame, WgpuVideoRenderInput, WgpuVideoRenderer,
};

const FRAME_WIDTH: u32 = 4;
const FRAME_HEIGHT: u32 = 2;
/// Шаг строк кадра в блоке — как у нашего FFmpeg `get_buffer2`.
const ROW_PITCH: usize = 256;
/// Offset-ы plane-ов в блоке: Y — 2 строки, U и V — по одной.
const PLANE_OFFSETS: [usize; 3] = [0, 2 * ROW_PITCH, 3 * ROW_PITCH];
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const GPU_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Ограниченный диапазон BT.709: Y = 235 — белый, Y = 16 — чёрный, U = V = 128.
const WHITE_LUMA: u8 = 235;
const BLACK_LUMA: u8 = 16;
const NEUTRAL_CHROMA: u8 = 128;
const WHITE_PIXEL: [u8; 4] = [255, 255, 255, 255];
const BLACK_PIXEL: [u8; 4] = [0, 0, 0, 255];
const PIXEL_TOLERANCE: u8 = 4;

/// Блок памяти кадра: обнулённая память процесса, выровненная по странице.
#[derive(Debug)]
struct TestFrameBlock {
    id: HostMemoryBlockId,
    address: NonNull<u8>,
    layout: Layout,
}

// SAFETY: блок эксклюзивно владеет allocation-ой; после заполнения в `new` память только
// читается (GPU и staging-копия), наружу через safe API уходят id и диапазон.
unsafe impl Send for TestFrameBlock {}
// SAFETY: см. `Send`.
unsafe impl Sync for TestFrameBlock {}

impl TestFrameBlock {
    /// Блок с кадром цвета `luma` (chroma нейтральная).
    fn solid(luma: u8) -> Arc<Self> {
        let layout = Layout::from_size_align(
            HOST_IMPORTABLE_BLOCK_ALIGNMENT,
            HOST_IMPORTABLE_BLOCK_ALIGNMENT,
        )
        .expect("valid layout");
        // SAFETY: layout ненулевой.
        let address = NonNull::new(unsafe { alloc_zeroed(layout) }).expect("allocation");
        let block = Self {
            id: HostMemoryBlockId::allocate(),
            address,
            layout,
        };
        let plane_bytes = [
            (luma, FRAME_HEIGHT),
            (NEUTRAL_CHROMA, 1),
            (NEUTRAL_CHROMA, 1),
        ];
        for ((value, rows), offset) in plane_bytes.into_iter().zip(PLANE_OFFSETS) {
            for row in 0..rows as usize {
                // SAFETY: строка `row` plane-ы лежит внутри блока (4 КиБ), блок ещё не
                // отдан никому, кроме этого конструктора.
                unsafe {
                    std::ptr::write_bytes(
                        block.address.as_ptr().add(offset + row * ROW_PITCH),
                        value,
                        FRAME_WIDTH as usize,
                    );
                }
            }
        }
        Arc::new(block)
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: память выделена на `layout.size()` байт и после `solid` не меняется.
        unsafe { std::slice::from_raw_parts(self.address.as_ptr(), self.layout.size()) }
    }
}

impl Drop for TestFrameBlock {
    fn drop(&mut self) {
        // SAFETY: выделено `alloc_zeroed` с этим layout-ом, освобождается один раз.
        unsafe { dealloc(self.address.as_ptr(), self.layout) };
    }
}

// SAFETY: диапазон постоянный, выровнен по странице, длина — одна страница; память жива
// до Drop; id выдан `HostMemoryBlockId::allocate`.
unsafe impl HostImportableMemoryBlock for TestFrameBlock {
    fn id(&self) -> HostMemoryBlockId {
        self.id
    }

    fn region(&self) -> HostMemoryRegion {
        HostMemoryRegion::new(self.address, self.layout.size())
    }
}

/// Владелец кадра в блоке: отдаёт блок для импорта и строки для запасного пути.
#[derive(Debug)]
struct BlockFrameOwner {
    block: Arc<TestFrameBlock>,
}

impl HostPlanarFrameOwner for BlockFrameOwner {
    fn visible_row_bytes(
        &self,
        plane_index: usize,
        plane: &HostPlaneDescriptor,
        row_index: u32,
        visible_row_bytes: usize,
    ) -> anyhow::Result<&[u8]> {
        let start = PLANE_OFFSETS[plane_index] + row_index as usize * plane.stride;
        Ok(&self.block.bytes()[start..start + visible_row_bytes])
    }

    fn visible_plane_block(
        &self,
        plane_index: usize,
        _plane: &HostPlaneDescriptor,
        block_len: usize,
    ) -> anyhow::Result<&[u8]> {
        let start = PLANE_OFFSETS[plane_index];
        Ok(&self.block.bytes()[start..start + block_len])
    }

    fn importable_memory(
        &self,
        _planes: &[HostPlaneDescriptor],
    ) -> Option<HostPlanarImportableMemory> {
        let block: Arc<dyn HostImportableMemoryBlock> = Arc::clone(&self.block) as _;
        Some(HostPlanarImportableMemory::new(
            block,
            PLANE_OFFSETS.to_vec(),
        ))
    }
}

fn block_frame_descriptor(owner: Arc<BlockFrameOwner>) -> HostPlanarFrameDescriptor {
    let plane = |role, width, height| HostPlaneDescriptor {
        role,
        offset: 0,
        stride: ROW_PITCH,
        visible_width: width,
        visible_height: height,
        bytes_per_sample: 1,
    };
    HostPlanarFrameDescriptor::new(
        owner,
        vec![
            plane(HostPlaneRole::Luma, FRAME_WIDTH, FRAME_HEIGHT),
            plane(HostPlaneRole::ChromaU, FRAME_WIDTH / 2, FRAME_HEIGHT / 2),
            plane(HostPlaneRole::ChromaV, FRAME_WIDTH / 2, FRAME_HEIGHT / 2),
        ],
    )
}

/// Провайдер, которому тест кладёт кадры по handle-ам и может их забрать.
#[derive(Clone, Default)]
struct BlockFrameProvider {
    frames: Arc<Mutex<HashMap<FrameResourceHandle, HostPlanarFrameDescriptor>>>,
}

impl BlockFrameProvider {
    /// Кладёт кадр и возвращает слабую ссылку на его владельца (для проверки времени жизни).
    fn insert(
        &self,
        handle: FrameResourceHandle,
        block: Arc<TestFrameBlock>,
    ) -> Weak<BlockFrameOwner> {
        let owner = Arc::new(BlockFrameOwner { block });
        let weak_owner = Arc::downgrade(&owner);
        self.frames
            .lock()
            .expect("provider lock")
            .insert(handle, block_frame_descriptor(owner));
        weak_owner
    }

    /// Забирает кадр, как декодер после release: провайдер его больше не держит.
    fn remove(&self, handle: FrameResourceHandle) {
        self.frames.lock().expect("provider lock").remove(&handle);
    }
}

impl PresentFrameResourceProvider for BlockFrameProvider {
    fn resource_lookup(&self, _handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        PresentFrameResourceProviderLookup::Ready {
            resource_pool_lock_wait: Duration::ZERO,
        }
    }

    fn try_resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        let frames = self.frames.lock().expect("provider lock");
        match frames.get(&handle) {
            Some(descriptor) => PresentFrameResourceDescriptorLookup::Ready {
                descriptor: FrameResourceDescriptor::HostPlanar(descriptor.clone()),
                resource_pool_lock_wait: Duration::ZERO,
            },
            None => PresentFrameResourceDescriptorLookup::Missing {
                resource_pool_lock_wait: Duration::ZERO,
            },
        }
    }

    fn release_frame(&self, _handle: FrameResourceHandle) {}
}

fn decoded_frame(resource_handle: FrameResourceHandle) -> DecodedFrame {
    DecodedFrame {
        generation: 0,
        pts: Duration::ZERO,
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
        width: FRAME_WIDTH,
        height: FRAME_HEIGHT,
        render_width: FRAME_WIDTH,
        render_height: FRAME_HEIGHT,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle,
        diagnostics: VideoFrameDiagnostics::default(),
    }
}

fn request_adapter() -> Option<wgpu::Adapter> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .ok()
}

/// Устройство с `VK_EXT_external_memory_host`, как его создаёт shell; `None` — нет такого.
fn try_create_device_with_host_import() -> Option<(wgpu::Device, wgpu::Queue)> {
    let adapter = request_adapter()?;
    let (device, queue) =
        crate::create_video_device_with_external_sync(&adapter, &wgpu::DeviceDescriptor::default())
            .ok()
            .flatten()?;
    HostMemoryImporter::new(&device)?;
    Some((device, queue))
}

/// Обычное устройство wgpu без наших расширений (запасной путь).
fn try_create_plain_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let adapter = request_adapter()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

/// Материализатор, рендерер и цель с буфером обратного чтения.
struct ImportToScreenChain {
    device: wgpu::Device,
    queue: wgpu::Queue,
    provider: BlockFrameProvider,
    materializer: HostPlanarWgpuFrameMaterializer,
    renderer: WgpuVideoRenderer,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    readback: wgpu::Buffer,
}

impl ImportToScreenChain {
    fn new((device, queue): (wgpu::Device, wgpu::Queue)) -> Self {
        let provider = BlockFrameProvider::default();
        let materializer = HostPlanarWgpuFrameMaterializer::new(
            &device,
            &queue,
            PresentFrameResourceProviderHandle::new(provider.clone()),
        );
        let mut renderer = WgpuVideoRenderer::new(&device, TARGET_FORMAT);
        renderer.resize(FRAME_WIDTH, FRAME_HEIGHT);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("host import test target"),
            size: wgpu::Extent3d {
                width: FRAME_WIDTH,
                height: FRAME_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("host import test readback"),
            size: u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT * FRAME_HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Self {
            device,
            queue,
            provider,
            materializer,
            renderer,
            target,
            target_view,
            readback,
        }
    }

    fn upload(&self, handle: FrameResourceHandle) -> HostPlanarWgpuTextureViews {
        match self
            .materializer
            .try_host_planar_texture_view_lookup(&decoded_frame(handle))
        {
            HostPlanarWgpuTextureViewLookup::Ready { views, .. } => *views,
            _ => panic!("материализатор должен загрузить кадр {handle:?}"),
        }
    }

    /// Рисует кадр из слота; возвращает копию загрузки (если есть) и неотправленный encoder.
    fn draw(
        &mut self,
        handle: FrameResourceHandle,
        views: &HostPlanarWgpuTextureViews,
    ) -> (Option<wgpu::CommandBuffer>, wgpu::CommandEncoder) {
        let decoded = decoded_frame(handle);
        let frame = WgpuRenderableFrame::from_decoded_host_planar(&decoded, views.plane_views())
            .expect("host-planar кадр из views материализатора");
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let outcome = self
            .renderer
            .render_or_clear(WgpuVideoRenderInput {
                frame: Some(&frame),
                video_viewport: RenderViewport {
                    x: 0,
                    y: 0,
                    width: FRAME_WIDTH,
                    height: FRAME_HEIGHT,
                },
                video_exclusion_rects: &[],
                target: &self.target_view,
                encoder: &mut encoder,
                device: &self.device,
                queue: &self.queue,
            })
            .expect("рендерер принимает host-planar кадр");
        assert!(outcome.frame_drawn, "кадр должен дойти до render pass-а");
        let host_upload = outcome
            .host_upload
            .map(crate::video::HostPlanarUploadCommands::into_command_buffer);
        (host_upload, encoder)
    }

    /// Отправляет кадр одним submit-ом, ждёт GPU и возвращает левый верхний пиксель.
    fn submit_and_read_pixel(
        &self,
        host_upload: Option<wgpu::CommandBuffer>,
        mut encoder: wgpu::CommandEncoder,
    ) -> [u8; 4] {
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
                    rows_per_image: Some(FRAME_HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: FRAME_WIDTH,
                height: FRAME_HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        let submission = self
            .queue
            .submit(host_upload.into_iter().chain([encoder.finish()]));
        let readback_slice = self.readback.slice(..);
        let (mapped_sender, mapped_receiver) = mpsc::sync_channel(1);
        readback_slice.map_async(wgpu::MapMode::Read, move |mapping| {
            let _ = mapped_sender.send(mapping);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(GPU_WAIT_TIMEOUT),
            })
            .expect("GPU завершает кадр");
        mapped_receiver
            .recv_timeout(GPU_WAIT_TIMEOUT)
            .expect("callback маппинга")
            .expect("маппинг буфера чтения");
        let pixel = {
            let mapped = readback_slice
                .get_mapped_range()
                .expect("буфер чтения отображён");
            [mapped[0], mapped[1], mapped[2], mapped[3]]
        };
        self.readback.unmap();
        pixel
    }

    /// Загружает и показывает кадр `handle`, возвращает пиксель экрана.
    fn show(&mut self, handle: FrameResourceHandle) -> [u8; 4] {
        let views = self.upload(handle);
        let (host_upload, encoder) = self.draw(handle, &views);
        self.submit_and_read_pixel(host_upload, encoder)
    }
}

fn assert_pixel_near(actual: [u8; 4], expected: [u8; 4], what: &str) {
    let close = actual
        .iter()
        .zip(expected)
        .all(|(&channel, expected_channel)| channel.abs_diff(expected_channel) <= PIXEL_TOLERANCE);
    assert!(close, "{what}: пиксель {actual:?}, ожидался ~{expected:?}");
}

#[test]
fn frame_in_importable_block_reaches_screen_without_cpu_copy() {
    let Some(gpu) = try_create_device_with_host_import() else {
        eprintln!("пропуск: нет Vulkan-адаптера с VK_EXT_external_memory_host");
        return;
    };
    let mut chain = ImportToScreenChain::new(gpu);
    let white = FrameResourceHandle(1);
    let black = FrameResourceHandle(2);
    chain
        .provider
        .insert(white, TestFrameBlock::solid(WHITE_LUMA));
    chain
        .provider
        .insert(black, TestFrameBlock::solid(BLACK_LUMA));

    let white_pixel = chain.show(white);
    let black_pixel = chain.show(black);

    assert_pixel_near(
        white_pixel,
        WHITE_PIXEL,
        "белый кадр из импортированного блока",
    );
    assert_pixel_near(
        black_pixel,
        BLACK_PIXEL,
        "чёрный кадр из импортированного блока",
    );
    let stats = chain.materializer.upload_path_stats();
    assert_eq!(stats.frames_without_cpu_copy, 2, "{stats:?}");
    assert_eq!(
        stats.frames_through_staging, 0,
        "CPU-копии быть не должно: {stats:?}"
    );
    assert_eq!(stats.host_memory_import.imports_created, 2, "{stats:?}");
}

#[test]
fn frame_stays_alive_until_its_gpu_copy_is_done() {
    let Some(gpu) = try_create_device_with_host_import() else {
        eprintln!("пропуск: нет Vulkan-адаптера с VK_EXT_external_memory_host");
        return;
    };
    let mut chain = ImportToScreenChain::new(gpu);
    let handle = FrameResourceHandle(7);
    let weak_owner = chain
        .provider
        .insert(handle, TestFrameBlock::solid(WHITE_LUMA));

    let views = chain.upload(handle);
    let (host_upload, encoder) = chain.draw(handle, &views);
    // Декодер «отпустил» кадр: держать его может только записанная GPU-копия.
    chain.provider.remove(handle);
    assert!(
        weak_owner.upgrade().is_some(),
        "до выполнения GPU-копии кадр обязан жить: иначе декодер перепишет его память"
    );

    let pixel = chain.submit_and_read_pixel(host_upload, encoder);

    assert_pixel_near(pixel, WHITE_PIXEL, "кадр после отпускания провайдером");
    // Callback обёртки срабатывает при обслуживании устройства после выполнения submit-а.
    chain
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    drop(views);
    assert!(
        weak_owner.upgrade().is_none(),
        "после GPU-копии кадр нужно отпустить, иначе декодер не получит блок обратно"
    );
}

/// Импорт держится ровно одну GPU-копию: живые `userptr`-буферы amdgpu проверяются ядром
/// на каждом submit, поэтому накапливать их нельзя (замер cpu-parity 08b).
#[test]
fn import_lives_only_until_its_gpu_copy_is_done() {
    let Some(gpu) = try_create_device_with_host_import() else {
        eprintln!("пропуск: нет Vulkan-адаптера с VK_EXT_external_memory_host");
        return;
    };
    let mut chain = ImportToScreenChain::new(gpu);
    let shared_block = TestFrameBlock::solid(WHITE_LUMA);
    let (first, second) = (FrameResourceHandle(11), FrameResourceHandle(12));
    // Два кадра подряд в одном блоке — как декодер, переиспользующий блок пула.
    chain.provider.insert(first, Arc::clone(&shared_block));
    chain.provider.insert(second, Arc::clone(&shared_block));

    let views = chain.upload(first);
    let (host_upload, encoder) = chain.draw(first, &views);
    let in_flight = chain.materializer.upload_path_stats().host_memory_import;
    assert_eq!(
        in_flight.live_imports, 1,
        "импорт жив, пока копия не выполнена: {in_flight:?}"
    );

    chain.submit_and_read_pixel(host_upload, encoder);
    chain
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    let after_copy = chain.materializer.upload_path_stats().host_memory_import;
    assert_eq!(
        after_copy.live_imports, 0,
        "после копии импорт уничтожен: {after_copy:?}"
    );

    let pixel = chain.show(second);
    chain
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");

    assert_pixel_near(pixel, WHITE_PIXEL, "второй кадр из того же блока");
    let stats = chain.materializer.upload_path_stats().host_memory_import;
    assert_eq!(
        stats.imports_created, 2,
        "импорт на каждую копию: {stats:?}"
    );
    assert_eq!(stats.live_imports, 0, "{stats:?}");
}

#[test]
fn device_without_host_import_uses_staging_copy() {
    let Some(gpu) = try_create_plain_device() else {
        eprintln!("пропуск: в окружении нет wgpu-адаптера");
        return;
    };
    let mut chain = ImportToScreenChain::new(gpu);
    let handle = FrameResourceHandle(21);
    chain
        .provider
        .insert(handle, TestFrameBlock::solid(WHITE_LUMA));

    let pixel = chain.show(handle);

    assert_pixel_near(
        pixel,
        WHITE_PIXEL,
        "кадр через staging на устройстве без импорта",
    );
    let stats = chain.materializer.upload_path_stats();
    assert_eq!(stats.frames_without_cpu_copy, 0, "{stats:?}");
    assert_eq!(stats.frames_through_staging, 1, "{stats:?}");
}

#[test]
fn import_memory_type_must_be_host_coherent() {
    let coherent = vk::MemoryType {
        property_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
            | vk::MemoryPropertyFlags::HOST_COHERENT,
        heap_index: 0,
    };
    let non_coherent = vk::MemoryType {
        property_flags: vk::MemoryPropertyFlags::HOST_VISIBLE,
        heap_index: 0,
    };

    assert_eq!(
        select_host_import_memory_type(0b11, &[non_coherent, coherent]),
        Some(1)
    );
    assert_eq!(
        select_host_import_memory_type(0b01, &[non_coherent, coherent]),
        None
    );
    assert_eq!(select_host_import_memory_type(0, &[coherent]), None);
}

#[test]
fn driver_alignment_above_page_disables_import() {
    assert!(page_aligned_blocks_satisfy(4096));
    assert!(page_aligned_blocks_satisfy(64));
    assert!(!page_aligned_blocks_satisfy(65536));
    assert!(!page_aligned_blocks_satisfy(0));
}
