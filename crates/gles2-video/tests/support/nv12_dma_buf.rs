//! NV12 в настоящих буферах ядра: блоки 8×8 разных цветов, раскладки composed и separate.
//!
//! Блоки 8×8 нужны, чтобы билинейная выборка в центре блока не смешивала соседние цвета
//! (цветность — блоки 4×4 текселя цветности).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "тестовый стенд: громкие падения"
)]

use std::os::fd::AsFd;

use gles2_egui::DrmFourcc;
use gles2_video::{
    DmaBufBufferIdentity, DmaBufFrameFormat, DmaBufNv12Frame, DmaBufPlane, DmaBufPlaneArrangement,
    PlaneExtent, VideoFrameKey,
};

use super::udmabuf::Udmabuf;

/// Размер яркости тестового кадра: 4 × 2 блока по 8 пикселей.
pub const LUMA_WIDTH: u32 = 32;
pub const LUMA_HEIGHT: u32 = 16;
/// Шаг строк обеих плоскостей: с запасом, как требует выравнивание AMD.
pub const PITCH: u32 = 256;
/// Цветность начинается сразу за яркостью (`PITCH × LUMA_HEIGHT` — кратно странице).
pub const COMPOSED_CHROMA_OFFSET: u64 = (PITCH * LUMA_HEIGHT) as u64;
/// Заполнитель отступов строк: пиксели в него попадать не должны.
const PADDING_BYTE: u8 = 0xEE;

/// Блок кадра: тройка YUV.
pub type BlockColors = [[u8; 3]; 8];

/// Один набор цветов блоков (индекс = `блок_y × 4 + блок_x`).
pub const PALETTE_A: BlockColors = [
    [81, 90, 240],
    [145, 54, 34],
    [41, 240, 110],
    [170, 166, 16],
    [235, 128, 128],
    [16, 128, 128],
    [107, 102, 220],
    [200, 60, 150],
];
/// Другой набор: проверка, что перезапись памяти видна.
pub const PALETTE_B: BlockColors = [
    [180, 100, 100],
    [60, 200, 90],
    [120, 128, 128],
    [30, 60, 200],
    [90, 220, 40],
    [210, 40, 210],
    [140, 150, 100],
    [70, 110, 180],
];

pub fn luma_extent() -> PlaneExtent {
    PlaneExtent {
        width: LUMA_WIDTH,
        height: LUMA_HEIGHT,
    }
}

/// Как лежат плоскости.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Nv12Layout {
    /// Яркость и цветность в одном буфере (цветность — за яркостью).
    Composed,
    /// Два отдельных буфера.
    Separate,
}

fn luma_bytes(colors: &BlockColors) -> Vec<u8> {
    let mut bytes = vec![PADDING_BYTE; (PITCH * LUMA_HEIGHT) as usize];
    for y in 0..LUMA_HEIGHT {
        for x in 0..LUMA_WIDTH {
            let block = (y / 8 * 4 + x / 8) as usize;
            bytes[(y * PITCH + x) as usize] = colors[block][0];
        }
    }
    bytes
}

fn chroma_bytes(colors: &BlockColors) -> Vec<u8> {
    let (width, height) = (LUMA_WIDTH / 2, LUMA_HEIGHT / 2);
    let mut bytes = vec![PADDING_BYTE; (PITCH * height) as usize];
    for y in 0..height {
        for x in 0..width {
            let block = (y / 4 * 4 + x / 4) as usize;
            let at = (y * PITCH + x * 2) as usize;
            // GR88: первый байт — R (= U), второй — G (= V).
            bytes[at] = colors[block][1];
            bytes[at + 1] = colors[block][2];
        }
    }
    bytes
}

/// Кадр NV12 в буферах ядра и его паспорт.
pub struct TestNv12 {
    layout: Nv12Layout,
    buffers: Vec<Udmabuf>,
    pub identity: DmaBufBufferIdentity,
}

impl TestNv12 {
    /// Создать буферы с `colors`; `None` — нет `/dev/udmabuf`.
    pub fn create(
        test_name: &str,
        layout: Nv12Layout,
        resource_id: u64,
        colors: &BlockColors,
    ) -> Option<Self> {
        let created = match layout {
            Nv12Layout::Composed => {
                let mut contents = luma_bytes(colors);
                contents.extend(chroma_bytes(colors));
                Udmabuf::create(&contents).map(|buffer| vec![buffer])
            }
            Nv12Layout::Separate => Udmabuf::create(&luma_bytes(colors)).and_then(|luma| {
                Udmabuf::create(&chroma_bytes(colors)).map(|chroma| vec![luma, chroma])
            }),
        };
        let buffers = match created {
            Ok(buffers) => buffers,
            Err(reason) => {
                eprintln!("skip {test_name}: {reason}");
                return None;
            }
        };
        let identity = DmaBufBufferIdentity {
            resource_id,
            objects: buffers
                .iter()
                .map(|buffer| buffer.object_identity(0))
                .collect(),
            format: DmaBufFrameFormat {
                luma_extent: luma_extent(),
                arrangement: match layout {
                    Nv12Layout::Composed => DmaBufPlaneArrangement::ComposedSingleObject,
                    Nv12Layout::Separate => DmaBufPlaneArrangement::SeparatePlanes,
                },
                modifier: 0,
            },
        };
        Some(Self {
            layout,
            buffers,
            identity,
        })
    }

    /// Перезаписать память буферов новыми цветами (как производитель кадров).
    pub fn rewrite(&mut self, colors: &BlockColors) {
        match self.layout {
            Nv12Layout::Composed => {
                let mut contents = luma_bytes(colors);
                contents.extend(chroma_bytes(colors));
                self.buffers[0].overwrite_contents_as_cpu_producer(&contents);
            }
            Nv12Layout::Separate => {
                self.buffers[0].overwrite_contents_as_cpu_producer(&luma_bytes(colors));
                self.buffers[1].overwrite_contents_as_cpu_producer(&chroma_bytes(colors));
            }
        }
    }

    /// Кадр для импорта (дескрипторы заимствованы у `self`).
    pub fn frame(&self, key: u64) -> DmaBufNv12Frame<'_> {
        let luma_fd = self.buffers[0].dma_buf.as_fd();
        let chroma_buffer = self.buffers.last().unwrap();
        let chroma_offset = match self.layout {
            Nv12Layout::Composed => COMPOSED_CHROMA_OFFSET,
            Nv12Layout::Separate => 0,
        };
        let plane = |fd, offset, drm_format: DrmFourcc| DmaBufPlane {
            fd,
            offset,
            pitch: PITCH,
            modifier: self.identity.format.modifier,
            drm_format: drm_format.0,
        };
        DmaBufNv12Frame::new(
            VideoFrameKey(key),
            &self.identity,
            luma_extent(),
            plane(luma_fd, 0, DrmFourcc::R8),
            plane(
                chroma_buffer.dma_buf.as_fd(),
                chroma_offset,
                DrmFourcc::GR88,
            ),
        )
        .expect("test frame is valid")
    }
}
