//! NV12 в настоящих буферах ядра: блоки 8×8 разных цветов, раскладки composed и separate, и
//! дескрипторы DRM PRIME в словаре плеера (`video-core`), как их отдал бы VA-API.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "тестовый стенд: громкие падения"
)]

use std::os::fd::AsFd;

use video_core::{
    DmaBufFrameDescriptor, DmaBufFrameExportLayout, DmaBufFrameIdentity, DmaBufLayerDescriptor,
    DmaBufObjectDescriptor, DmaBufObjectIdentity, DmaBufObjectIdentityDescriptor,
};

use super::udmabuf::{KernelObject, Udmabuf};

/// DRM fourcc `NV12`, `R8` и `GR88` (`drm_fourcc.h`).
const FOURCC_NV12: u32 = u32::from_le_bytes(*b"NV12");
const FOURCC_R8: u32 = u32::from_le_bytes(*b"R8  ");
const FOURCC_GR88: u32 = u32::from_le_bytes(*b"GR88");

/// Размер яркости тестового кадра: 4 × 2 блока по 8 пикселей.
pub const LUMA_WIDTH: u32 = 32;
pub const LUMA_HEIGHT: u32 = 16;
/// Шаг строк обеих плоскостей: с запасом, как требует выравнивание AMD.
pub const PITCH: u32 = 256;
/// Цветность composed-буфера начинается сразу за яркостью (`PITCH × LUMA_HEIGHT` — страница).
const COMPOSED_CHROMA_OFFSET: u32 = PITCH * LUMA_HEIGHT;
/// Заполнитель отступов строк: пиксели в него попадать не должны.
const PADDING_BYTE: u8 = 0xEE;
/// `DRM_FORMAT_MOD_LINEAR`: `udmabuf` всегда линейный.
const LINEAR_MODIFIER: u64 = 0;

/// Блок кадра: тройка YUV.
pub type BlockColors = [[u8; 3]; 8];

/// Набор цветов блоков (индекс = `блок_y × 4 + блок_x`).
pub const PALETTE: BlockColors = [
    [81, 90, 240],
    [145, 54, 34],
    [41, 240, 110],
    [170, 166, 16],
    [235, 128, 128],
    [16, 128, 128],
    [107, 102, 220],
    [200, 60, 150],
];

/// Как лежат плоскости.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Nv12Layout {
    /// Яркость и цветность в одном буфере.
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

/// Кадр NV12 в буферах ядра.
pub struct TestNv12 {
    layout: Nv12Layout,
    buffers: Vec<Udmabuf>,
    kernel_objects: Vec<KernelObject>,
}

impl TestNv12 {
    /// Создать буферы с `colors`; `None` — нет `/dev/udmabuf` (причина в stderr).
    pub fn create(test_name: &str, layout: Nv12Layout, colors: &BlockColors) -> Option<Self> {
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
        let kernel_objects = buffers.iter().map(Udmabuf::kernel_object).collect();
        Some(Self {
            layout,
            buffers,
            kernel_objects,
        })
    }

    pub fn layout(&self) -> Nv12Layout {
        self.layout
    }

    fn export_layout(&self) -> DmaBufFrameExportLayout {
        match self.layout {
            Nv12Layout::Composed => DmaBufFrameExportLayout::ComposedLayers,
            Nv12Layout::Separate => DmaBufFrameExportLayout::SeparateLayers,
        }
    }

    fn layers(&self) -> Vec<DmaBufLayerDescriptor> {
        match self.layout {
            Nv12Layout::Composed => vec![DmaBufLayerDescriptor {
                drm_format: FOURCC_NV12,
                num_planes: 2,
                object_index: [0; 4],
                offset: [0, COMPOSED_CHROMA_OFFSET, 0, 0],
                pitch: [PITCH, PITCH, 0, 0],
            }],
            Nv12Layout::Separate => vec![
                DmaBufLayerDescriptor {
                    drm_format: FOURCC_R8,
                    num_planes: 1,
                    object_index: [0; 4],
                    offset: [0; 4],
                    pitch: [PITCH, 0, 0, 0],
                },
                DmaBufLayerDescriptor {
                    drm_format: FOURCC_GR88,
                    num_planes: 1,
                    object_index: [1, 0, 0, 0],
                    offset: [0; 4],
                    pitch: [PITCH, 0, 0, 0],
                },
            ],
        }
    }

    /// Идентичность без dup fd (по `fstat`, сделанному при создании): как `try_resource_identity_lookup`.
    pub fn identity(&self, resource_id: u64) -> DmaBufFrameIdentity {
        DmaBufFrameIdentity {
            resource_id,
            fourcc: FOURCC_NV12,
            export_layout: self.export_layout(),
            width: LUMA_WIDTH,
            height: LUMA_HEIGHT,
            objects: self
                .kernel_objects
                .iter()
                .map(|object| DmaBufObjectIdentityDescriptor {
                    identity: DmaBufObjectIdentity {
                        device: object.device,
                        inode: object.inode,
                        special_device: 0,
                    },
                    size: object.size,
                    drm_format_modifier: LINEAR_MODIFIER,
                })
                .collect(),
            layers: self.layers(),
        }
    }

    /// Дескриптор с ДУБЛИРОВАННЫМИ fd (владеет получатель), как отдаёт провайдер при промахе.
    pub fn descriptor(&self, resource_id: u64) -> DmaBufFrameDescriptor {
        DmaBufFrameDescriptor {
            resource_id,
            fourcc: FOURCC_NV12,
            export_layout: self.export_layout(),
            width: LUMA_WIDTH,
            height: LUMA_HEIGHT,
            objects: self
                .buffers
                .iter()
                .zip(&self.kernel_objects)
                .map(|(buffer, object)| DmaBufObjectDescriptor {
                    fd: buffer.dma_buf.as_fd().try_clone_to_owned().unwrap(),
                    size: object.size,
                    drm_format_modifier: LINEAR_MODIFIER,
                    identity: DmaBufObjectIdentity {
                        device: object.device,
                        inode: object.inode,
                        special_device: 0,
                    },
                })
                .collect(),
            layers: self.layers(),
        }
    }
}
