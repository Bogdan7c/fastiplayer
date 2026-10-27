//! Программный планарный кадр в настоящем буфере ядра: три плоскости Y, U, V в одном `udmabuf`,
//! с шагом строк больше ширины и мусором в хвостах (шаг обязан соблюдаться, как у блока FFmpeg).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "тестовый стенд: громкие падения"
)]

use std::os::fd::AsFd;

use gles2_egui::{Gles2Context, OffscreenPixels};
use gles2_video::{
    ChromaSubsampling, ColorAdjustment, DmaBufPlanarFrame, DmaBufPlanarIdentity,
    DmaBufPlaneLocation, PlanarDmaBufImportOutcome, PlanarLayout, PlaneExtent, SampleDepth,
    VideoDrawOutcome, VideoFrameKey, VideoLayerRenderer, YuvColorEncoding,
};

use super::udmabuf::Udmabuf;
use super::{FramePattern, OwnedFrame, draw_frame, extent, fill_request, size};

pub const ENCODING: YuvColorEncoding = YuvColorEncoding::SDR_BT709_LIMITED;
/// Кадр 32×16 растянут на цель 64×32.
pub const TARGET_WIDTH: u32 = 64;
pub const TARGET_HEIGHT: u32 = 32;

/// Шаг строк всех плоскостей: с запасом над шириной, кратен 256 (выравнивание AMD).
pub const PITCH: u32 = 256;
/// Плоскости начинаются с границы страницы.
const PLANE_ALIGNMENT: u32 = 4096;
/// Заполнитель отступов строк: пиксели в него попадать не должны.
const PADDING_BYTE: u8 = 0xEE;

/// Куда кладётся каждая плоскость и сколько всего байт.
fn plane_locations(
    luma_extent: PlaneExtent,
    chroma: ChromaSubsampling,
) -> ([DmaBufPlaneLocation; 3], u32) {
    let chroma_extent = chroma.chroma_extent(luma_extent);
    let mut offset = 0;
    let mut locations = [DmaBufPlaneLocation {
        offset: 0,
        pitch: PITCH,
    }; 3];
    for (location, extent) in locations
        .iter_mut()
        .zip([luma_extent, chroma_extent, chroma_extent])
    {
        location.offset = offset;
        offset += (PITCH * extent.height).div_ceil(PLANE_ALIGNMENT) * PLANE_ALIGNMENT;
    }
    (locations, offset)
}

/// Содержимое буфера для кадра `frame`: видимые байты по своим местам, отступы — мусор.
fn buffer_contents(
    frame: &OwnedFrame,
    locations: &[DmaBufPlaneLocation; 3],
    total: u32,
) -> Vec<u8> {
    let mut bytes = vec![PADDING_BYTE; total as usize];
    for (location, plane) in locations
        .iter()
        .zip([&frame.luma, &frame.chroma_u, &frame.chroma_v])
    {
        for row in 0..plane.rows as usize {
            let source =
                &plane.bytes[row * plane.stride..row * plane.stride + plane.visible_row_bytes];
            let at = location.offset as usize + row * location.pitch as usize;
            bytes[at..at + plane.visible_row_bytes].copy_from_slice(source);
        }
    }
    bytes
}

/// Кадр в буфере ядра и его паспорт.
pub struct TestPlanar {
    buffer: Udmabuf,
    locations: [DmaBufPlaneLocation; 3],
    total: u32,
    pub identity: DmaBufPlanarIdentity,
}

impl TestPlanar {
    /// Создать буфер с содержимым `frame`; `None` — нет `/dev/udmabuf`.
    pub fn create(test_name: &str, resource_id: u64, frame: &OwnedFrame) -> Option<Self> {
        let (locations, total) = plane_locations(frame.luma_extent, frame.chroma);
        let buffer = match Udmabuf::create(&buffer_contents(frame, &locations, total)) {
            Ok(buffer) => buffer,
            Err(reason) => {
                eprintln!("skip {test_name}: {reason}");
                return None;
            }
        };
        let identity = DmaBufPlanarIdentity {
            resource_id,
            object: buffer.object_identity(0),
            layout: PlanarLayout {
                chroma: frame.chroma,
                depth: SampleDepth::Bits8,
            },
            luma_extent: frame.luma_extent,
            planes: locations,
        };
        Some(Self {
            buffer,
            locations,
            total,
            identity,
        })
    }

    /// Перезаписать память кадром `frame` (как производитель: с окном CPU-доступа и сбросом кэша).
    pub fn rewrite(&mut self, frame: &OwnedFrame) {
        let contents = buffer_contents(frame, &self.locations, self.total);
        self.buffer.overwrite_contents_as_cpu_producer(&contents);
    }

    /// Кадр для импорта (дескриптор заимствован у `self`).
    pub fn frame(&self, key: u64) -> DmaBufPlanarFrame<'_> {
        DmaBufPlanarFrame::new(
            VideoFrameKey(key),
            &self.identity,
            self.buffer.dma_buf.as_fd(),
        )
        .expect("test frame is valid")
    }
}

/// Кадр с разными значениями в каждой плоскости и по координатам: перепутанные плоскости,
/// неверный шаг строки или сдвиг дают другие пиксели.
pub fn patterned_frame(
    chroma: ChromaSubsampling,
    width: u32,
    height: u32,
    seed: u32,
) -> OwnedFrame {
    OwnedFrame::from_pattern(
        chroma,
        extent(width, height),
        7,
        &FramePattern {
            luma: &|x, y| u8::try_from(16 + (x * 5 + y * 11 + seed * 17) % 200).unwrap(),
            chroma_u: &|x, y| u8::try_from(40 + (x * 13 + y * 3 + seed * 29) % 180).unwrap(),
            chroma_v: &|x, y| u8::try_from(200 - (x * 7 + y * 19 + seed * 5) % 170).unwrap(),
        },
    )
}

pub fn full_draw(
    context: &mut Gles2Context,
    video: &mut VideoLayerRenderer,
) -> (VideoDrawOutcome, OffscreenPixels) {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let rects = [super::full_rect(target)];
    let request = fill_request(target, &rects, ENCODING, ColorAdjustment::IDENTITY);
    draw_frame(context, video, &request)
}

pub fn drawn(outcome: VideoDrawOutcome) {
    assert!(
        matches!(outcome, VideoDrawOutcome::Drawn { .. }),
        "expected a drawn frame, got {outcome:?}"
    );
}

pub fn imported(outcome: PlanarDmaBufImportOutcome) {
    assert!(
        matches!(outcome, PlanarDmaBufImportOutcome::Imported(_)),
        "expected a fresh import, got {outcome:?}"
    );
}
