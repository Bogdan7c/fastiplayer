//! Путь «программный кадр без CPU-копии» на настоящем EGL (без окна) и настоящем `/dev/udmabuf`.
//!
//! Главная проверка — пиксели: кадр из memfd-блока, показанный без копии, равен тому же кадру,
//! показанному копией (`glTexSubImage2D`). Без EGL тесты пропускаются с причиной
//! (`GLES2_EGUI_REQUIRE_EGL=1` превращает пропуск в падение), без `/dev/udmabuf` — тоже с причиной.

#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "тесты: тестовый блок памяти реализует unsafe-трейт импорта, громкие падения"
)]

use std::ptr::NonNull;
use std::sync::Arc;

use gles2_egui::{CreateError, FrameStart, Gles2Context, SurfaceSize};
use gles2_video::{UploadOutcome, VideoLayerOptions, VideoLayerRenderer};
use render_core::{RenderLiveSettings, RenderViewport};
use video_core::{
    DecodedFrame, FrameResourceDescriptor, FrameResourceHandle, HostImportableMemoryBlock,
    HostMemoryBlockId, HostMemoryRegion, HostPlanarFrameDescriptor, HostPlanarFrameOwner,
    HostPlanarImportableMemory, HostPlaneDescriptor, HostPlaneRole, SealedMemfdRegion,
};
use video_frame_contract::VideoFramePixelLayout;

use super::kernel::{SealedMemfdPages, UdmabufDevice};
use super::*;
use crate::test_support::host_frame;
use crate::{VideoDrawPlan, VideoPlacement, renderable_frame};

const REQUIRE_EGL_VARIABLE: &str = "GLES2_EGUI_REQUIRE_EGL";
const SIZE: [u32; 2] = [16, 16];
/// Шаг строк плоскостей в блоке (как у блоков декодера — кратен 256).
const STRIDE: usize = 256;
/// Места плоскостей 4:2:0 16×16 в блоке: Y, U, V.
const PLANE_OFFSETS: [usize; 3] = [0, 16 * STRIDE, 16 * STRIDE + 8 * STRIDE];
const BLOCK_BYTES: usize = 8192;

/// Блок памяти кадра в запечатанном memfd — как блок декодера `video-ffmpeg`.
#[derive(Debug)]
struct MemfdBlock {
    pages: SealedMemfdPages,
    id: HostMemoryBlockId,
}

// SAFETY: отображение принадлежит блоку и не меняется; тесты пишут в него только через
// `&mut` (`Arc::get_mut`), когда других ссылок нет.
unsafe impl Send for MemfdBlock {}
// SAFETY: см. `Send`: общие ссылки только читают.
unsafe impl Sync for MemfdBlock {}

// SAFETY: регион — всё отображение memfd (страницы, длина кратна странице), не меняется и жив,
// пока жив блок; memfd запечатан `F_SEAL_SHRINK` без `F_SEAL_WRITE`, отображение с нуля.
unsafe impl HostImportableMemoryBlock for MemfdBlock {
    fn id(&self) -> HostMemoryBlockId {
        self.id
    }

    fn region(&self) -> HostMemoryRegion {
        let address = NonNull::new(self.pages.bytes().as_ptr().cast_mut()).unwrap();
        HostMemoryRegion::new(address, self.pages.len())
    }

    fn sealed_memfd(&self) -> Option<SealedMemfdRegion<'_>> {
        Some(SealedMemfdRegion::new(self.pages.memfd(), 0))
    }
}

impl MemfdBlock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            pages: SealedMemfdPages::create(BLOCK_BYTES).unwrap(),
            id: HostMemoryBlockId::allocate(),
        })
    }

    /// Записать кадр 4:2:0: плоскости Y, U, V заполнены `samples`.
    fn write_picture(self: &mut Arc<Self>, samples: [u8; 3]) {
        let block = Arc::get_mut(self).expect("no other references to the block");
        let bytes = block.pages.bytes_mut();
        for ((offset, rows, width), sample) in [
            (PLANE_OFFSETS[0], 16, 16),
            (PLANE_OFFSETS[1], 8, 8),
            (PLANE_OFFSETS[2], 8, 8),
        ]
        .into_iter()
        .zip(samples)
        {
            for row in 0..rows {
                let start = offset + row * STRIDE;
                bytes[start..start + width].fill(sample);
            }
        }
    }
}

/// Владелец кадра поверх блока (как `AvFrameHostPlanarOwner` у FFmpeg).
#[derive(Debug)]
struct MemfdOwner {
    block: Arc<MemfdBlock>,
}

impl HostPlanarFrameOwner for MemfdOwner {
    fn visible_row_bytes(
        &self,
        _plane_index: usize,
        plane: &HostPlaneDescriptor,
        row_index: u32,
        visible_row_bytes: usize,
    ) -> anyhow::Result<&[u8]> {
        let start = plane.offset + row_index as usize * plane.stride;
        self.block
            .pages
            .bytes()
            .get(start..start + visible_row_bytes)
            .ok_or_else(|| anyhow::anyhow!("row outside the block"))
    }

    fn visible_plane_block(
        &self,
        _plane_index: usize,
        plane: &HostPlaneDescriptor,
        block_len: usize,
    ) -> anyhow::Result<&[u8]> {
        self.block
            .pages
            .bytes()
            .get(plane.offset..plane.offset + block_len)
            .ok_or_else(|| anyhow::anyhow!("plane outside the block"))
    }

    fn importable_memory(
        &self,
        _planes: &[HostPlaneDescriptor],
    ) -> Option<HostPlanarImportableMemory> {
        let block: Arc<dyn HostImportableMemoryBlock> = self.block.clone();
        Some(HostPlanarImportableMemory::new(
            block,
            PLANE_OFFSETS.to_vec(),
        ))
    }
}

/// Кадр 4:2:0 8 бит с ключом `handle` в блоке `block`.
fn memfd_frame(block: &Arc<MemfdBlock>, handle: u64) -> (DecodedFrame, FrameResourceDescriptor) {
    let planes = [
        (HostPlaneRole::Luma, 16),
        (HostPlaneRole::ChromaU, 8),
        (HostPlaneRole::ChromaV, 8),
    ]
    .into_iter()
    .zip(PLANE_OFFSETS)
    .map(|((role, side), offset)| HostPlaneDescriptor {
        role,
        offset,
        stride: STRIDE,
        visible_width: side,
        visible_height: side,
        bytes_per_sample: 1,
    })
    .collect();
    let owner = Arc::new(MemfdOwner {
        block: Arc::clone(block),
    });
    let mut frame = host_frame(VideoFramePixelLayout::Yuv420Planar8, SIZE, 0, [0; 3]).frame;
    frame.resource_handle = FrameResourceHandle(handle);
    (
        frame,
        FrameResourceDescriptor::HostPlanar(HostPlanarFrameDescriptor::new(owner, planes)),
    )
}

struct Stand {
    context: Gles2Context,
    video: VideoLayerRenderer,
    software_frames: HostBlockZeroCopy,
}

fn stand_or_skip(test_name: &str) -> Option<Stand> {
    let size = SurfaceSize::new(SIZE[0], SIZE[1]).unwrap();
    let context = match Gles2Context::offscreen(size) {
        Ok(context) => context,
        Err(
            error @ (CreateError::EglUnavailable { .. }
            | CreateError::PlatformExtensionMissing { .. }
            | CreateError::DisplayOpen(_)),
        ) => {
            assert!(
                std::env::var_os(REQUIRE_EGL_VARIABLE).is_none(),
                "{test_name}: EGL is required but unavailable: {error}"
            );
            eprintln!("skip {test_name}: no EGL: {error}");
            return None;
        }
        Err(error) => panic!("{test_name}: context creation failed: {error}"),
    };
    if let Err(error) = UdmabufDevice::open() {
        eprintln!("skip {test_name}: /dev/udmabuf is not accessible: {error}");
        return None;
    }
    let video = VideoLayerRenderer::new(&context, VideoLayerOptions::default()).unwrap();
    let driver = HardwareZeroCopy::for_capabilities(context.capabilities());
    Some(Stand {
        context,
        video,
        software_frames: HostBlockZeroCopy::new(driver),
    })
}

/// Нарисовать текущий кадр слоя на всю цель и прочитать пиксели.
fn draw_and_read(
    context: &mut Gles2Context,
    video: &mut VideoLayerRenderer,
    frame: &DecodedFrame,
) -> Vec<u8> {
    let plan = VideoDrawPlan::for_frame(
        &renderable_frame(frame).unwrap(),
        VideoPlacement {
            surface_size: (SIZE[0], SIZE[1]),
            video_viewport: RenderViewport::full_surface(SIZE[0], SIZE[1]),
            exclusions: &[],
        },
        &RenderLiveSettings::default(),
    )
    .unwrap();
    match context.begin_frame().unwrap() {
        FrameStart::Ready(active) => {
            video.draw(&active, &plan.request()).unwrap();
            active.present().unwrap();
        }
        FrameStart::Skipped(reason) => panic!("frame skipped: {reason:?}"),
    }
    let fence = Arc::new(context.insert_fence().unwrap());
    video.retire_submitted_work(Arc::clone(&fence));
    context.read_offscreen_pixels().unwrap().as_rgba().to_vec()
}

/// Эталон: тот же кадр из обычной памяти, показанный копией на отдельном слое.
fn uploaded_reference(stand: &mut Stand, samples: [u8; 3]) -> Vec<u8> {
    let reference = host_frame(VideoFramePixelLayout::Yuv420Planar8, SIZE, 3, samples);
    let mut layer = VideoLayerRenderer::new(&stand.context, VideoLayerOptions::default()).unwrap();
    let upload = crate::host_planar_frame_upload(&reference.frame, &reference.descriptor).unwrap();
    assert!(matches!(
        layer.upload(&stand.context, &upload).unwrap(),
        UploadOutcome::Uploaded(_)
    ));
    let pixels = draw_and_read(&mut stand.context, &mut layer, &reference.frame);
    layer.destroy(&stand.context).unwrap();
    pixels
}

/// Кадр в memfd-блоке показывается без копии (ни одного байта через `glTexSubImage2D`), и
/// картинка та же, что у копии тех же байтов. Один dma-buf на блок.
#[test]
fn a_memfd_frame_is_shown_without_a_copy_and_matches_the_uploaded_picture() {
    let Some(mut stand) = stand_or_skip("a_memfd_frame_is_shown_without_a_copy") else {
        return;
    };
    let samples = [180, 90, 160];
    let mut block = MemfdBlock::new();
    block.write_picture(samples);
    let (frame, descriptor) = memfd_frame(&block, 1);

    let shown =
        stand
            .software_frames
            .try_show(&mut stand.video, &stand.context, &frame, &descriptor);

    assert_eq!(shown, ZeroCopyShow::Shown);
    let stats = stand.software_frames.stats();
    assert!(
        matches!(stats.state, SoftwareZeroCopyState::Enabled(_)),
        "{stats:?}"
    );
    assert_eq!(stats.frames_without_cpu_copy, 1);
    assert_eq!(stats.dma_bufs_created, 1);
    assert_eq!(
        stand.video.stats().copied_bytes,
        0,
        "no byte went through a copy"
    );
    let pixels = draw_and_read(&mut stand.context, &mut stand.video, &frame);
    assert_eq!(pixels, uploaded_reference(&mut stand, samples));
}

/// Декодер переиспользует блок: новый кадр (новый ключ, новое содержимое) показывается без
/// нового dma-buf и без нового импорта, а на экране — новая картинка.
#[test]
fn a_reused_block_shows_its_new_picture_without_a_new_dma_buf_or_import() {
    let Some(mut stand) = stand_or_skip("a_reused_block_shows_its_new_picture") else {
        return;
    };
    let mut block = MemfdBlock::new();
    block.write_picture([60, 128, 128]);
    let (first, first_descriptor) = memfd_frame(&block, 1);
    let first_shown =
        stand
            .software_frames
            .try_show(&mut stand.video, &stand.context, &first, &first_descriptor);
    assert_eq!(first_shown, ZeroCopyShow::Shown);
    let first_pixels = draw_and_read(&mut stand.context, &mut stand.video, &first);
    drop(first_descriptor);

    // Кадр вернулся декодеру, блок переписан следующим кадром.
    let samples = [210, 70, 190];
    block.write_picture(samples);
    let (second, second_descriptor) = memfd_frame(&block, 2);
    let second_shown = stand.software_frames.try_show(
        &mut stand.video,
        &stand.context,
        &second,
        &second_descriptor,
    );

    assert_eq!(second_shown, ZeroCopyShow::Shown);
    let stats = stand.software_frames.stats();
    assert_eq!(stats.frames_without_cpu_copy, 2);
    assert_eq!(
        stats.dma_bufs_created, 1,
        "one dma-buf per block, not per frame"
    );
    let imports = stand.video.planar_dma_buf_import_snapshot();
    assert_eq!(
        imports.imports, 1,
        "the second frame reused the cached import"
    );
    let second_pixels = draw_and_read(&mut stand.context, &mut stand.video, &second);
    assert_ne!(second_pixels, first_pixels);
    assert_eq!(second_pixels, uploaded_reference(&mut stand, samples));
}

/// Политика «сбрасывать кэш»: каждый новый кадр сбрасывается, картинка верна. (На машине, где
/// GPU видит запись CPU и так, ветку включаем напрямую; настоящий Ironlake проверяет приёмка.)
#[test]
fn the_flush_policy_flushes_every_new_frame() {
    let Some(mut stand) = stand_or_skip("the_flush_policy_flushes_every_new_frame") else {
        return;
    };
    stand
        .software_frames
        .enable_for_tests(CpuCacheFlushPolicy::FlushVisibleRows)
        .unwrap();
    let mut block = MemfdBlock::new();
    for (handle, samples) in [(1, [40, 100, 200]), (2, [220, 160, 60])] {
        block.write_picture(samples);
        let (frame, descriptor) = memfd_frame(&block, handle);
        let shown =
            stand
                .software_frames
                .try_show(&mut stand.video, &stand.context, &frame, &descriptor);
        assert_eq!(shown, ZeroCopyShow::Shown);
        let pixels = draw_and_read(&mut stand.context, &mut stand.video, &frame);
        assert_eq!(pixels, uploaded_reference(&mut stand, samples));
    }
    assert_eq!(stand.software_frames.stats().cpu_cache_flushes, 2);
}

/// Кадр не в memfd-блоке (обычная память: wgpu-заказ, AV1) и кадр 10 бит идут копией; путь их
/// даже не проверяет (самопроверка не запускается, состояние не меняется).
#[test]
fn frames_outside_the_path_go_to_the_upload_without_probing() {
    let Some(mut stand) = stand_or_skip("frames_outside_the_path_go_to_the_upload") else {
        return;
    };
    let anonymous = host_frame(VideoFramePixelLayout::Yuv420Planar8, SIZE, 0, [1, 2, 3]);
    let ten_bit = crate::test_support::high_bit_host_frame(
        VideoFramePixelLayout::Yuv420Planar10Le,
        SIZE,
        0,
        [64, 512, 512],
    );
    for test_frame in [anonymous, ten_bit] {
        let shown = stand.software_frames.try_show(
            &mut stand.video,
            &stand.context,
            &test_frame.frame,
            &test_frame.descriptor,
        );
        assert_eq!(shown, ZeroCopyShow::UseUpload);
    }
    let stats = stand.software_frames.stats();
    assert_eq!(stats.state, SoftwareZeroCopyState::NotProbed);
    assert_eq!(stats.frames_without_cpu_copy, 0);
    assert_eq!(stand.video.planar_dma_buf_import_snapshot().imports, 0);
}

/// Драйвер без импорта DMA-BUF: путь выключается с причиной, кадр идёт копией, видеослой не тронут.
#[test]
fn a_driver_without_dma_buf_import_disables_the_path() {
    let Some(mut stand) = stand_or_skip("a_driver_without_dma_buf_import_disables_the_path") else {
        return;
    };
    let mut software_frames = HostBlockZeroCopy::new(HardwareZeroCopy::Unavailable);
    let mut block = MemfdBlock::new();
    block.write_picture([100, 100, 100]);
    let (frame, descriptor) = memfd_frame(&block, 1);

    let shown = software_frames.try_show(&mut stand.video, &stand.context, &frame, &descriptor);

    assert_eq!(shown, ZeroCopyShow::UseUpload);
    assert_eq!(
        software_frames.stats().state,
        SoftwareZeroCopyState::Disabled(SoftwareZeroCopyDisabled::DriverCannotImportDmaBuf)
    );
    assert_eq!(stand.video.planar_dma_buf_import_snapshot().imports, 0);
}

/// dma-buf блоков не больше ёмкости (старые закрываются), смена декодера закрывает все.
#[test]
fn block_dma_bufs_are_bounded_and_closed_on_decoder_change() {
    let Some(mut stand) = stand_or_skip("block_dma_bufs_are_bounded") else {
        return;
    };
    stand
        .software_frames
        .decoder_changed(BlockDmaBufCapacity::matching_surface_pool(2));
    let mut blocks = [MemfdBlock::new(), MemfdBlock::new(), MemfdBlock::new()];
    for (handle, block) in (1..).zip(blocks.iter_mut()) {
        block.write_picture([90, 120, 140]);
        let (frame, descriptor) = memfd_frame(block, handle);
        let shown =
            stand
                .software_frames
                .try_show(&mut stand.video, &stand.context, &frame, &descriptor);
        assert_eq!(shown, ZeroCopyShow::Shown);
    }
    assert_eq!(stand.software_frames.stats().dma_bufs_created, 3);
    assert_eq!(stand.software_frames.dma_bufs.len(), 2);

    stand
        .software_frames
        .decoder_changed(BlockDmaBufCapacity::matching_surface_pool(24));
    assert_eq!(stand.software_frames.dma_bufs.len(), 0);
}

/// Правило «итог самопроверки → политика сброса или причина отказа».
#[test]
fn probe_outcome_maps_to_flush_policy_or_disable_reason() {
    use CpuCacheFlushSupport::{Clflush, Unavailable};
    use CpuWriteVisibility::{CoherentWithoutCacheFlush, NotVisible, VisibleAfterCacheFlush};
    assert_eq!(
        flush_policy_for(CoherentWithoutCacheFlush, Unavailable),
        Ok(CpuCacheFlushPolicy::NotNeeded)
    );
    assert_eq!(
        flush_policy_for(VisibleAfterCacheFlush, Clflush),
        Ok(CpuCacheFlushPolicy::FlushVisibleRows)
    );
    assert_eq!(
        flush_policy_for(VisibleAfterCacheFlush, Unavailable),
        Err(SoftwareZeroCopyDisabled::CacheFlushUnsupported)
    );
    assert_eq!(
        flush_policy_for(NotVisible, Clflush),
        Err(SoftwareZeroCopyDisabled::CpuWritesNotVisible)
    );
}
