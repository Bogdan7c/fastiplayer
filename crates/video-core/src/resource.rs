use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::{fmt, io};

use anyhow::{Context, bail, ensure};
use video_frame_contract::{
    DmaBufImageLayout, HardwareFrameHandle, VideoFrameContract, VideoFramePixelLayout,
    VideoFrameTransferPath,
};

/// Opaque handle decoded frame resource-а.
///
/// Handle не раскрывает backend storage: decoder/provider владеет таблицей
/// resources, а playback/render слои используют это значение только как ключ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameResourceHandle(pub u64);

/// Renderer-neutral описание platform resource-а decoded frame-а.
///
/// Контракт намеренно хранит только переносимые для boundary данные. WGPU,
/// Vulkan, VA-API, GBM и cros-codecs типы остаются в concrete crates.
#[derive(Debug)]
pub enum FrameResourceDescriptor {
    /// Linux DMA-BUF/DRM PRIME descriptor для zero-copy materialization.
    DmaBuf(DmaBufFrameDescriptor),

    /// CPU-visible planar frame, готовый для будущего host-upload path-а.
    HostPlanar(HostPlanarFrameDescriptor),
}

impl FrameResourceDescriptor {
    /// Дублирует owned platform handles для renderer-side materializer-а.
    ///
    /// Provider продолжает владеть исходным descriptor-ом и decoder frame-ом;
    /// renderer получает отдельные fd и обязан закрыть их через обычный drop.
    pub fn try_clone_for_lookup(&self) -> io::Result<Self> {
        match self {
            Self::DmaBuf(descriptor) => descriptor.try_clone_for_lookup().map(Self::DmaBuf),
            Self::HostPlanar(descriptor) => Ok(Self::HostPlanar(descriptor.clone_for_lookup())),
        }
    }
}

/// Роль plane внутри owned host-planar frame-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPlaneRole {
    /// Luma/Y plane.
    Luma,

    /// Chroma U/Cb plane.
    ChromaU,

    /// Chroma V/Cr plane.
    ChromaV,
}

/// Описание одной visible plane внутри общего host storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPlaneDescriptor {
    /// Смысл plane-а; layout остаётся в `VideoFrameContract`, не в descriptor-е.
    pub role: HostPlaneRole,

    /// Byte offset начала plane-а внутри backing-а, которым владеет `HostPlanarFrameOwner`.
    ///
    /// Для simple owned-buffer owner-а это offset от начала общего byte slice-а.
    /// Provider-owned owner может интерпретировать offset относительно своей
    /// plane storage, не раскрывая raw pointers за пределы provider-а.
    pub offset: usize,

    /// Byte stride между началом соседних строк.
    pub stride: usize,

    /// Видимая ширина plane-а в samples; padding справа сюда не входит.
    pub visible_width: u32,

    /// Видимая высота plane-а в строках.
    pub visible_height: u32,

    /// Размер одного sample в байтах: 1 для 8-bit, 2 для 10/12-bit LE storage word.
    pub bytes_per_sample: usize,
}

/// Владелец CPU-visible backing-а для host-planar frame-а.
///
/// `video-core` знает только этот neutral trait. Concrete provider может держать
/// внутри `Arc<[u8]>`, AVFrame или другой resource, но наружу отдаёт только
/// безопасный slice видимых bytes конкретной строки plane-а.
pub trait HostPlanarFrameOwner: fmt::Debug + Send + Sync {
    /// Возвращает только видимые bytes строки, без правого padding-а.
    ///
    /// `plane_index` и metadata принадлежат descriptor-у; owner использует их,
    /// чтобы найти строку в своём backing-е и не раскрывать storage details.
    fn visible_row_bytes(
        &self,
        plane_index: usize,
        plane: &HostPlaneDescriptor,
        row_index: u32,
        visible_row_bytes: usize,
    ) -> anyhow::Result<&[u8]>;
}

/// Простая owned-buffer реализация owner-а для тестов и будущих CPU-owned paths.
#[derive(Debug, Clone)]
pub struct HostPlanarOwnedBuffer {
    /// Общий immutable byte storage всего host frame-а.
    bytes: Arc<[u8]>,
}

impl HostPlanarOwnedBuffer {
    /// Создаёт owner поверх immutable byte storage без дополнительной копии.
    #[must_use]
    pub fn new(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            bytes: bytes.into(),
        }
    }
}

impl HostPlanarFrameOwner for HostPlanarOwnedBuffer {
    fn visible_row_bytes(
        &self,
        _plane_index: usize,
        plane: &HostPlaneDescriptor,
        row_index: u32,
        visible_row_bytes: usize,
    ) -> anyhow::Result<&[u8]> {
        let row_index =
            usize::try_from(row_index).context("host-planar row index does not fit usize")?;
        let row_delta = plane
            .stride
            .checked_mul(row_index)
            .context("host-planar row stride overflow")?;
        let row_start = plane
            .offset
            .checked_add(row_delta)
            .context("host-planar row offset overflow")?;
        let row_end = row_start
            .checked_add(visible_row_bytes)
            .context("host-planar visible row end overflow")?;

        ensure!(
            row_end <= self.bytes.len(),
            "host-planar {:?} visible area exceeds storage: end {}, storage {}",
            plane.role,
            row_end,
            self.bytes.len()
        );

        Ok(&self.bytes[row_start..row_end])
    }
}

/// Owned host-planar decoded frame без raw pointers и layout второго порядка.
#[derive(Debug, Clone)]
pub struct HostPlanarFrameDescriptor {
    /// Owner удерживает backing живым, пока существует descriptor или его clone.
    owner: Arc<dyn HostPlanarFrameOwner>,

    /// Per-plane metadata; pixel layout и frame dimensions остаются в contract-е.
    pub planes: Vec<HostPlaneDescriptor>,
}

impl HostPlanarFrameDescriptor {
    /// Создаёт descriptor поверх provider-owned backing-а.
    #[must_use]
    pub fn new(owner: Arc<dyn HostPlanarFrameOwner>, planes: Vec<HostPlaneDescriptor>) -> Self {
        Self { owner, planes }
    }

    /// Создаёт descriptor поверх simple owned byte buffer-а.
    #[must_use]
    pub fn from_owned_buffer(
        bytes: impl Into<Arc<[u8]>>,
        planes: Vec<HostPlaneDescriptor>,
    ) -> Self {
        Self::new(Arc::new(HostPlanarOwnedBuffer::new(bytes)), planes)
    }

    /// Возвращает видимые bytes одной строки plane-а без padding-а.
    pub fn visible_plane_row_bytes(
        &self,
        plane_index: usize,
        row_index: u32,
    ) -> anyhow::Result<&[u8]> {
        let plane = self
            .planes
            .get(plane_index)
            .with_context(|| format!("host-planar plane index {plane_index} is out of bounds"))?;
        ensure!(
            row_index < plane.visible_height,
            "host-planar {:?} row {} is outside visible height {}",
            plane.role,
            row_index,
            plane.visible_height
        );

        let visible_row_bytes = host_plane_visible_row_bytes(plane)?;
        ensure!(
            plane.stride >= visible_row_bytes,
            "host-planar {:?} stride {} is smaller than visible row bytes {}",
            plane.role,
            plane.stride,
            visible_row_bytes
        );

        let row_bytes =
            self.owner
                .visible_row_bytes(plane_index, plane, row_index, visible_row_bytes)?;
        ensure!(
            row_bytes.len() == visible_row_bytes,
            "host-planar {:?} owner returned {} bytes for visible row, expected {}",
            plane.role,
            row_bytes.len(),
            visible_row_bytes
        );

        Ok(row_bytes)
    }

    /// Дешёвый clone для lookup-а: owner/refcount клонируется без копирования bytes.
    #[must_use]
    fn clone_for_lookup(&self) -> Self {
        Self {
            owner: Arc::clone(&self.owner),
            planes: self.planes.clone(),
        }
    }
}

/// Layout exported DMA-BUF frame-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaBufFrameExportLayout {
    /// Один DRM layer описывает весь multi-planar image.
    ComposedLayers,

    /// Отдельные DRM layers описывают luma/chroma planes.
    SeparateLayers,
}

/// Kernel identity exported DMA-BUF object-а без transient fd number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmaBufObjectIdentity {
    /// Device id из `fstat`.
    pub device: u64,

    /// Inode exported object-а из `fstat`.
    pub inode: u64,

    /// Special-device id из `fstat`, если kernel его заполняет.
    pub special_device: u64,
}

/// Один owned DMA-BUF object внутри DRM PRIME descriptor-а.
#[derive(Debug)]
pub struct DmaBufObjectDescriptor {
    /// Owned fd, который живёт вместе с descriptor-ом.
    pub fd: OwnedFd,

    /// Размер object-а в байтах.
    pub size: u32,

    /// DRM format modifier backing memory layout-а.
    pub drm_format_modifier: u64,

    /// Stable identity object-а для cache/reuse проверок.
    pub identity: DmaBufObjectIdentity,
}

impl DmaBufObjectDescriptor {
    /// Дублирует fd, сохраняя immutable metadata.
    fn try_clone_for_lookup(&self) -> io::Result<Self> {
        Ok(Self {
            fd: self.fd.as_fd().try_clone_to_owned()?,
            size: self.size,
            drm_format_modifier: self.drm_format_modifier,
            identity: self.identity,
        })
    }
}

/// Один DRM PRIME layer decoded frame-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmaBufLayerDescriptor {
    /// DRM fourcc этого layer-а.
    pub drm_format: u32,

    /// Количество planes, описанных layer-ом.
    pub num_planes: u32,

    /// Индекс DMA-BUF object-а для каждой plane.
    pub object_index: [u8; 4],

    /// Byte offsets planes внутри objects.
    pub offset: [u32; 4],

    /// Row pitches planes в байтах.
    pub pitch: [u32; 4],
}

/// Нейтральный DMA-BUF/DRM PRIME descriptor decoded frame-а.
#[derive(Debug)]
pub struct DmaBufFrameDescriptor {
    /// Backend-stable decoded resource id; для VA-API это numeric surface id.
    pub resource_id: u64,

    /// Top-level DRM fourcc exported surface-а.
    pub fourcc: u32,

    /// Layout export-а.
    pub export_layout: DmaBufFrameExportLayout,

    /// Coded width imported image-а.
    pub width: u32,

    /// Coded height imported image-а.
    pub height: u32,

    /// Owned DMA-BUF objects.
    pub objects: Vec<DmaBufObjectDescriptor>,

    /// DRM PRIME layers.
    pub layers: Vec<DmaBufLayerDescriptor>,
}

impl DmaBufFrameDescriptor {
    /// Дублирует все object fd для materializer lookup-а.
    pub fn try_clone_for_lookup(&self) -> io::Result<Self> {
        let objects = self
            .objects
            .iter()
            .map(DmaBufObjectDescriptor::try_clone_for_lookup)
            .collect::<io::Result<Vec<_>>>()?;

        Ok(Self {
            resource_id: self.resource_id,
            fourcc: self.fourcc,
            export_layout: self.export_layout,
            width: self.width,
            height: self.height,
            objects,
            layers: self.layers.clone(),
        })
    }
}

/// Проверяет runtime resource descriptor против expected decoded frame contract-а.
///
/// Размеры передаются явно, чтобы descriptor не становился вторым источником
/// frame-level coded/render geometry для host-planar path-а.
pub fn validate_resource_descriptor_against_contract(
    contract: VideoFrameContract,
    coded_width: u32,
    coded_height: u32,
    descriptor: &FrameResourceDescriptor,
) -> anyhow::Result<()> {
    contract
        .validate()
        .with_context(|| format!("invalid video frame contract: {contract}"))?;
    ensure!(
        coded_width > 0 && coded_height > 0,
        "resource validation requires positive coded size, got {coded_width}x{coded_height}"
    );

    match (contract.transfer_path, descriptor) {
        (
            VideoFrameTransferPath::HardwareZeroCopy {
                handle:
                    HardwareFrameHandle::DmaBuf {
                        image_layout: expected_layout,
                    },
            },
            FrameResourceDescriptor::DmaBuf(dma_buf_descriptor),
        ) => validate_dma_buf_descriptor_against_contract(
            expected_layout,
            coded_width,
            coded_height,
            dma_buf_descriptor,
        ),
        (
            VideoFrameTransferPath::HardwareZeroCopy { .. },
            FrameResourceDescriptor::HostPlanar(_),
        ) => bail!("hardware zero-copy contract requires DMA-BUF descriptor, got host-planar"),
        (VideoFrameTransferPath::SoftwareHostUpload, FrameResourceDescriptor::HostPlanar(host)) => {
            validate_host_planar_descriptor_against_contract(
                contract.pixel_layout,
                coded_width,
                coded_height,
                host,
            )
        }
        (VideoFrameTransferPath::SoftwareHostUpload, FrameResourceDescriptor::DmaBuf(_)) => {
            bail!("software host-upload contract requires host-planar descriptor, got DMA-BUF")
        }
    }
}

fn validate_dma_buf_descriptor_against_contract(
    expected_layout: DmaBufImageLayout,
    coded_width: u32,
    coded_height: u32,
    descriptor: &DmaBufFrameDescriptor,
) -> anyhow::Result<()> {
    let actual_layout = dma_buf_export_layout_to_image_layout(descriptor.export_layout);
    ensure!(
        actual_layout == expected_layout,
        "DMA-BUF image layout mismatch: expected {expected_layout}, got {actual_layout}"
    );
    ensure!(
        descriptor.width == coded_width && descriptor.height == coded_height,
        "DMA-BUF coded size mismatch: expected {coded_width}x{coded_height}, got {}x{}",
        descriptor.width,
        descriptor.height
    );
    Ok(())
}

const fn dma_buf_export_layout_to_image_layout(
    export_layout: DmaBufFrameExportLayout,
) -> DmaBufImageLayout {
    match export_layout {
        DmaBufFrameExportLayout::ComposedLayers => DmaBufImageLayout::ComposedLayers,
        DmaBufFrameExportLayout::SeparateLayers => DmaBufImageLayout::SeparateLayers,
    }
}

fn validate_host_planar_descriptor_against_contract(
    pixel_layout: VideoFramePixelLayout,
    coded_width: u32,
    coded_height: u32,
    descriptor: &HostPlanarFrameDescriptor,
) -> anyhow::Result<()> {
    let expected_planes = expected_host_planar_planes(pixel_layout, coded_width, coded_height)?;
    ensure!(
        descriptor.planes.len() == expected_planes.len(),
        "host-planar plane count mismatch for {pixel_layout}: expected {}, got {}",
        expected_planes.len(),
        descriptor.planes.len()
    );

    for (plane_index, (plane, expected_plane)) in descriptor
        .planes
        .iter()
        .zip(expected_planes.iter())
        .enumerate()
    {
        validate_host_plane_metadata(pixel_layout, plane, expected_plane)?;
        validate_host_plane_visible_bounds(descriptor, plane_index, plane)?;
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct ExpectedHostPlane {
    role: HostPlaneRole,
    visible_width: u32,
    visible_height: u32,
    bytes_per_sample: usize,
}

#[derive(Debug, Clone, Copy)]
struct ExpectedHostPlanarLayout {
    chroma_width: u32,
    chroma_height: u32,
    bytes_per_sample: usize,
}

fn expected_host_planar_planes(
    pixel_layout: VideoFramePixelLayout,
    coded_width: u32,
    coded_height: u32,
) -> anyhow::Result<Vec<ExpectedHostPlane>> {
    let layout = expected_host_planar_layout(pixel_layout, coded_width, coded_height)?;

    Ok(vec![
        ExpectedHostPlane {
            role: HostPlaneRole::Luma,
            visible_width: coded_width,
            visible_height: coded_height,
            bytes_per_sample: layout.bytes_per_sample,
        },
        ExpectedHostPlane {
            role: HostPlaneRole::ChromaU,
            visible_width: layout.chroma_width,
            visible_height: layout.chroma_height,
            bytes_per_sample: layout.bytes_per_sample,
        },
        ExpectedHostPlane {
            role: HostPlaneRole::ChromaV,
            visible_width: layout.chroma_width,
            visible_height: layout.chroma_height,
            bytes_per_sample: layout.bytes_per_sample,
        },
    ])
}

fn expected_host_planar_layout(
    pixel_layout: VideoFramePixelLayout,
    coded_width: u32,
    coded_height: u32,
) -> anyhow::Result<ExpectedHostPlanarLayout> {
    match pixel_layout {
        VideoFramePixelLayout::Yuv420Planar8 => Ok(ExpectedHostPlanarLayout {
            chroma_width: half_rounded_up(coded_width),
            chroma_height: half_rounded_up(coded_height),
            bytes_per_sample: 1,
        }),
        VideoFramePixelLayout::Yuv420Planar10Le | VideoFramePixelLayout::Yuv420Planar12Le => {
            Ok(ExpectedHostPlanarLayout {
                chroma_width: half_rounded_up(coded_width),
                chroma_height: half_rounded_up(coded_height),
                bytes_per_sample: 2,
            })
        }
        VideoFramePixelLayout::Yuv422Planar8 => Ok(ExpectedHostPlanarLayout {
            chroma_width: half_rounded_up(coded_width),
            chroma_height: coded_height,
            bytes_per_sample: 1,
        }),
        VideoFramePixelLayout::Yuv422Planar10Le | VideoFramePixelLayout::Yuv422Planar12Le => {
            Ok(ExpectedHostPlanarLayout {
                chroma_width: half_rounded_up(coded_width),
                chroma_height: coded_height,
                bytes_per_sample: 2,
            })
        }
        VideoFramePixelLayout::Yuv444Planar8 => Ok(ExpectedHostPlanarLayout {
            chroma_width: coded_width,
            chroma_height: coded_height,
            bytes_per_sample: 1,
        }),
        VideoFramePixelLayout::Yuv444Planar10Le => Ok(ExpectedHostPlanarLayout {
            chroma_width: coded_width,
            chroma_height: coded_height,
            bytes_per_sample: 2,
        }),
        _ => bail!("{pixel_layout} is not an accepted host-planar layout"),
    }
}

const fn half_rounded_up(dimension: u32) -> u32 {
    (dimension / 2) + (dimension % 2)
}

fn validate_host_plane_metadata(
    pixel_layout: VideoFramePixelLayout,
    plane: &HostPlaneDescriptor,
    expected_plane: &ExpectedHostPlane,
) -> anyhow::Result<()> {
    ensure!(
        plane.role == expected_plane.role,
        "host-planar role mismatch for {pixel_layout}: expected {:?}, got {:?}",
        expected_plane.role,
        plane.role
    );
    ensure!(
        plane.visible_width == expected_plane.visible_width
            && plane.visible_height == expected_plane.visible_height,
        "host-planar {:?} visible size mismatch for {pixel_layout}: expected {}x{}, got {}x{}",
        plane.role,
        expected_plane.visible_width,
        expected_plane.visible_height,
        plane.visible_width,
        plane.visible_height
    );
    ensure!(
        plane.bytes_per_sample == expected_plane.bytes_per_sample,
        "host-planar {:?} bytes-per-sample mismatch for {pixel_layout}: expected {}, got {}",
        plane.role,
        expected_plane.bytes_per_sample,
        plane.bytes_per_sample
    );
    Ok(())
}

fn host_plane_visible_row_bytes(plane: &HostPlaneDescriptor) -> anyhow::Result<usize> {
    ensure!(
        plane.visible_width > 0 && plane.visible_height > 0,
        "host-planar {:?} plane has invalid visible size {}x{}",
        plane.role,
        plane.visible_width,
        plane.visible_height
    );

    let visible_width = usize::try_from(plane.visible_width)
        .context("host-planar visible width does not fit usize")?;
    let visible_row_bytes = visible_width
        .checked_mul(plane.bytes_per_sample)
        .context("host-planar visible row byte count overflow")?;

    Ok(visible_row_bytes)
}

fn validate_host_plane_visible_bounds(
    descriptor: &HostPlanarFrameDescriptor,
    plane_index: usize,
    plane: &HostPlaneDescriptor,
) -> anyhow::Result<()> {
    let visible_row_bytes = host_plane_visible_row_bytes(plane)?;
    ensure!(
        plane.stride >= visible_row_bytes,
        "host-planar {:?} stride {} is smaller than visible row bytes {}",
        plane.role,
        plane.stride,
        visible_row_bytes
    );

    let last_visible_row = plane.visible_height.saturating_sub(1);
    descriptor.visible_plane_row_bytes(plane_index, last_visible_row)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::fd::{AsFd, AsRawFd, OwnedFd};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Создаёт owned fd, пригодный для проверки dup/drop semantics.
    fn open_test_dma_buf_fd() -> OwnedFd {
        let file = File::open("/dev/null").expect("test fd source must be readable");
        file.into()
    }

    /// Создаёт минимальный neutral DMA-BUF descriptor без backend-specific типов.
    fn sample_dma_buf_descriptor() -> DmaBufFrameDescriptor {
        DmaBufFrameDescriptor {
            resource_id: 42,
            fourcc: 0x3231_564e,
            export_layout: DmaBufFrameExportLayout::ComposedLayers,
            width: 1920,
            height: 1080,
            objects: vec![DmaBufObjectDescriptor {
                fd: open_test_dma_buf_fd(),
                size: 4096,
                drm_format_modifier: 7,
                identity: DmaBufObjectIdentity {
                    device: 11,
                    inode: 13,
                    special_device: 17,
                },
            }],
            layers: vec![DmaBufLayerDescriptor {
                drm_format: 0x3231_564e,
                num_planes: 2,
                object_index: [0, 0, 0, 0],
                offset: [0, 2048, 0, 0],
                pitch: [1920, 1920, 0, 0],
            }],
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct HostPlanarLayoutCase {
        pixel_layout: VideoFramePixelLayout,
        coded_width: u32,
        coded_height: u32,
        chroma_width: u32,
        chroma_height: u32,
        bytes_per_sample: usize,
    }

    fn host_planar_contract(pixel_layout: VideoFramePixelLayout) -> VideoFrameContract {
        VideoFrameContract {
            pixel_layout,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        }
    }

    fn explicit_host_planar_layout_cases() -> [HostPlanarLayoutCase; 8] {
        [
            HostPlanarLayoutCase {
                pixel_layout: VideoFramePixelLayout::Yuv420Planar8,
                coded_width: 4,
                coded_height: 4,
                chroma_width: 2,
                chroma_height: 2,
                bytes_per_sample: 1,
            },
            HostPlanarLayoutCase {
                pixel_layout: VideoFramePixelLayout::Yuv420Planar10Le,
                coded_width: 4,
                coded_height: 4,
                chroma_width: 2,
                chroma_height: 2,
                bytes_per_sample: 2,
            },
            HostPlanarLayoutCase {
                pixel_layout: VideoFramePixelLayout::Yuv420Planar12Le,
                coded_width: 4,
                coded_height: 4,
                chroma_width: 2,
                chroma_height: 2,
                bytes_per_sample: 2,
            },
            HostPlanarLayoutCase {
                pixel_layout: VideoFramePixelLayout::Yuv422Planar8,
                coded_width: 4,
                coded_height: 4,
                chroma_width: 2,
                chroma_height: 4,
                bytes_per_sample: 1,
            },
            HostPlanarLayoutCase {
                pixel_layout: VideoFramePixelLayout::Yuv422Planar10Le,
                coded_width: 4,
                coded_height: 4,
                chroma_width: 2,
                chroma_height: 4,
                bytes_per_sample: 2,
            },
            HostPlanarLayoutCase {
                pixel_layout: VideoFramePixelLayout::Yuv422Planar12Le,
                coded_width: 4,
                coded_height: 4,
                chroma_width: 2,
                chroma_height: 4,
                bytes_per_sample: 2,
            },
            HostPlanarLayoutCase {
                pixel_layout: VideoFramePixelLayout::Yuv444Planar8,
                coded_width: 4,
                coded_height: 4,
                chroma_width: 4,
                chroma_height: 4,
                bytes_per_sample: 1,
            },
            HostPlanarLayoutCase {
                pixel_layout: VideoFramePixelLayout::Yuv444Planar10Le,
                coded_width: 4,
                coded_height: 4,
                chroma_width: 4,
                chroma_height: 4,
                bytes_per_sample: 2,
            },
        ]
    }

    fn sample_host_planar_descriptor(bytes_per_sample: usize) -> HostPlanarFrameDescriptor {
        sample_host_planar_descriptor_for_case(HostPlanarLayoutCase {
            pixel_layout: VideoFramePixelLayout::Yuv420Planar8,
            coded_width: 4,
            coded_height: 4,
            chroma_width: 2,
            chroma_height: 2,
            bytes_per_sample,
        })
    }

    fn sample_host_planar_descriptor_for_case(
        layout_case: HostPlanarLayoutCase,
    ) -> HostPlanarFrameDescriptor {
        let plane_geometries = [
            (
                HostPlaneRole::Luma,
                layout_case.coded_width,
                layout_case.coded_height,
            ),
            (
                HostPlaneRole::ChromaU,
                layout_case.chroma_width,
                layout_case.chroma_height,
            ),
            (
                HostPlaneRole::ChromaV,
                layout_case.chroma_width,
                layout_case.chroma_height,
            ),
        ];

        let mut next_offset = 0usize;
        let mut planes = Vec::with_capacity(plane_geometries.len());
        for (role, visible_width, visible_height) in plane_geometries {
            let visible_width_usize =
                usize::try_from(visible_width).expect("test visible width must fit usize");
            let visible_height_usize =
                usize::try_from(visible_height).expect("test visible height must fit usize");
            let stride = visible_width_usize
                .checked_mul(layout_case.bytes_per_sample)
                .expect("test stride must not overflow");
            let plane_size = stride
                .checked_mul(visible_height_usize)
                .expect("test plane size must not overflow");

            planes.push(HostPlaneDescriptor {
                role,
                offset: next_offset,
                stride,
                visible_width,
                visible_height,
                bytes_per_sample: layout_case.bytes_per_sample,
            });

            next_offset = next_offset
                .checked_add(plane_size)
                .expect("test total byte length must not overflow");
        }

        HostPlanarFrameDescriptor::from_owned_buffer(vec![0; next_offset], planes)
    }

    /// Проверяет, что cloned descriptor получает другой fd и сохраняет metadata.
    #[test]
    fn dma_buf_descriptor_clone_duplicates_owned_fds_and_preserves_metadata() {
        let descriptor = sample_dma_buf_descriptor();
        let original_fd = descriptor.objects[0].fd.as_raw_fd();

        let cloned_descriptor = descriptor
            .try_clone_for_lookup()
            .expect("descriptor fd duplication must succeed");
        let cloned_fd = cloned_descriptor.objects[0].fd.as_raw_fd();

        assert_ne!(original_fd, cloned_fd);
        assert_eq!(cloned_descriptor.resource_id, descriptor.resource_id);
        assert_eq!(cloned_descriptor.fourcc, descriptor.fourcc);
        assert_eq!(cloned_descriptor.export_layout, descriptor.export_layout);
        assert_eq!(cloned_descriptor.width, descriptor.width);
        assert_eq!(cloned_descriptor.height, descriptor.height);
        assert_eq!(
            cloned_descriptor.objects[0].size,
            descriptor.objects[0].size
        );
        assert_eq!(
            cloned_descriptor.objects[0].drm_format_modifier,
            descriptor.objects[0].drm_format_modifier
        );
        assert_eq!(
            cloned_descriptor.objects[0].identity,
            descriptor.objects[0].identity
        );
        assert_eq!(cloned_descriptor.layers, descriptor.layers);
    }

    /// Проверяет, что drop cloned descriptor-а не закрывает provider-owned fd.
    #[test]
    fn dropping_cloned_descriptor_keeps_original_fd_owned_by_provider() {
        let descriptor = sample_dma_buf_descriptor();
        let cloned_descriptor = descriptor
            .try_clone_for_lookup()
            .expect("descriptor fd duplication must succeed");

        drop(cloned_descriptor);

        descriptor.objects[0]
            .fd
            .as_fd()
            .try_clone_to_owned()
            .expect("provider-owned fd must remain valid after cloned descriptor drop");
    }

    /// Проверяет, что drop provider descriptor-а не закрывает renderer-owned duplicated fd.
    #[test]
    fn dropping_original_descriptor_keeps_renderer_duplicate_fd_alive() {
        let descriptor = sample_dma_buf_descriptor();
        let cloned_descriptor = descriptor
            .try_clone_for_lookup()
            .expect("descriptor fd duplication must succeed");

        drop(descriptor);

        cloned_descriptor.objects[0]
            .fd
            .as_fd()
            .try_clone_to_owned()
            .expect("renderer-owned duplicate fd must remain valid after original drop");
    }

    /// Проверяет enum-level clone path, который вызывают provider lookup методы.
    #[test]
    fn frame_resource_descriptor_clone_preserves_dma_buf_variant() {
        let descriptor = FrameResourceDescriptor::DmaBuf(sample_dma_buf_descriptor());

        let cloned_descriptor = descriptor
            .try_clone_for_lookup()
            .expect("frame resource descriptor clone must succeed");

        match cloned_descriptor {
            FrameResourceDescriptor::DmaBuf(cloned_dma_buf) => {
                assert_eq!(cloned_dma_buf.resource_id, 42);
                assert_eq!(cloned_dma_buf.objects.len(), 1);
                assert_eq!(cloned_dma_buf.layers.len(), 1);
            }
            FrameResourceDescriptor::HostPlanar(_) => panic!("expected DMA-BUF clone"),
        }
    }

    #[test]
    fn host_planar_accepts_valid_descriptors_for_explicit_layout_matrix() {
        for layout_case in explicit_host_planar_layout_cases() {
            let descriptor = FrameResourceDescriptor::HostPlanar(
                sample_host_planar_descriptor_for_case(layout_case),
            );

            validate_resource_descriptor_against_contract(
                host_planar_contract(layout_case.pixel_layout),
                layout_case.coded_width,
                layout_case.coded_height,
                &descriptor,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "valid {} host-planar descriptor must pass: {error:#}",
                    layout_case.pixel_layout
                )
            });
        }
    }

    #[test]
    fn host_planar_rejects_invalid_plane_count() {
        let mut descriptor = sample_host_planar_descriptor(1);
        descriptor.planes.pop();

        let error = validate_resource_descriptor_against_contract(
            VideoFrameContract::host_yuv420_planar8(),
            4,
            4,
            &FrameResourceDescriptor::HostPlanar(descriptor),
        )
        .expect_err("missing V plane must be rejected");

        assert!(error.to_string().contains("plane count mismatch"));
    }

    #[test]
    fn host_planar_rejects_invalid_role_order() {
        let mut descriptor = sample_host_planar_descriptor(1);
        descriptor.planes.swap(1, 2);

        let error = validate_resource_descriptor_against_contract(
            VideoFrameContract::host_yuv420_planar8(),
            4,
            4,
            &FrameResourceDescriptor::HostPlanar(descriptor),
        )
        .expect_err("U/V role order mismatch must be rejected");

        assert!(error.to_string().contains("role mismatch"));
    }

    #[test]
    fn host_planar_rejects_invalid_stride() {
        let mut descriptor = sample_host_planar_descriptor(1);
        descriptor.planes[0].stride = 3;

        let error = validate_resource_descriptor_against_contract(
            VideoFrameContract::host_yuv420_planar8(),
            4,
            4,
            &FrameResourceDescriptor::HostPlanar(descriptor),
        )
        .expect_err("visible row wider than stride must be rejected");

        assert!(error.to_string().contains("stride"));
    }

    #[test]
    fn host_planar_rejects_out_of_bounds_offset() {
        let mut descriptor = sample_host_planar_descriptor(1);
        descriptor.planes[2].offset = 24;

        let error = validate_resource_descriptor_against_contract(
            VideoFrameContract::host_yuv420_planar8(),
            4,
            4,
            &FrameResourceDescriptor::HostPlanar(descriptor),
        )
        .expect_err("visible V plane outside storage must be rejected");

        assert!(error.to_string().contains("exceeds storage"));
    }

    #[test]
    fn host_planar_bounds_check_reads_visible_bytes_not_trailing_padding() {
        let descriptor = HostPlanarFrameDescriptor::from_owned_buffer(
            vec![0; 40],
            vec![
                HostPlaneDescriptor {
                    role: HostPlaneRole::Luma,
                    offset: 0,
                    stride: 8,
                    visible_width: 4,
                    visible_height: 4,
                    bytes_per_sample: 1,
                },
                HostPlaneDescriptor {
                    role: HostPlaneRole::ChromaU,
                    offset: 28,
                    stride: 4,
                    visible_width: 2,
                    visible_height: 2,
                    bytes_per_sample: 1,
                },
                HostPlaneDescriptor {
                    role: HostPlaneRole::ChromaV,
                    offset: 34,
                    stride: 4,
                    visible_width: 2,
                    visible_height: 2,
                    bytes_per_sample: 1,
                },
            ],
        );

        validate_resource_descriptor_against_contract(
            VideoFrameContract::host_yuv420_planar8(),
            4,
            4,
            &FrameResourceDescriptor::HostPlanar(descriptor),
        )
        .expect("last-row padding after visible bytes must not be required");
    }

    #[test]
    fn host_planar_uses_rounded_chroma_sizes_for_odd_yuv420_dimensions() {
        let descriptor = HostPlanarFrameDescriptor::from_owned_buffer(
            vec![0; 29],
            vec![
                HostPlaneDescriptor {
                    role: HostPlaneRole::Luma,
                    offset: 0,
                    stride: 5,
                    visible_width: 5,
                    visible_height: 3,
                    bytes_per_sample: 1,
                },
                HostPlaneDescriptor {
                    role: HostPlaneRole::ChromaU,
                    offset: 15,
                    stride: 3,
                    visible_width: 3,
                    visible_height: 2,
                    bytes_per_sample: 1,
                },
                HostPlaneDescriptor {
                    role: HostPlaneRole::ChromaV,
                    offset: 23,
                    stride: 3,
                    visible_width: 3,
                    visible_height: 2,
                    bytes_per_sample: 1,
                },
            ],
        );

        validate_resource_descriptor_against_contract(
            VideoFrameContract::host_yuv420_planar8(),
            5,
            3,
            &FrameResourceDescriptor::HostPlanar(descriptor),
        )
        .expect("odd YUV420 dimensions must round chroma plane sizes up");
    }

    #[test]
    fn host_planar_uses_rounded_chroma_width_for_odd_yuv422_dimensions() {
        let layout_case = HostPlanarLayoutCase {
            pixel_layout: VideoFramePixelLayout::Yuv422Planar10Le,
            coded_width: 5,
            coded_height: 3,
            chroma_width: 3,
            chroma_height: 3,
            bytes_per_sample: 2,
        };
        let descriptor = FrameResourceDescriptor::HostPlanar(
            sample_host_planar_descriptor_for_case(layout_case),
        );

        validate_resource_descriptor_against_contract(
            host_planar_contract(layout_case.pixel_layout),
            layout_case.coded_width,
            layout_case.coded_height,
            &descriptor,
        )
        .expect("odd YUV422 width must round chroma width up without halving height");
    }

    #[test]
    fn host_planar_rejects_mismatched_visible_plane_size() {
        let layout_case = HostPlanarLayoutCase {
            pixel_layout: VideoFramePixelLayout::Yuv422Planar8,
            coded_width: 4,
            coded_height: 4,
            chroma_width: 2,
            chroma_height: 4,
            bytes_per_sample: 1,
        };
        let mut descriptor = sample_host_planar_descriptor_for_case(layout_case);
        descriptor.planes[1].visible_height = 2;

        let error = validate_resource_descriptor_against_contract(
            host_planar_contract(layout_case.pixel_layout),
            layout_case.coded_width,
            layout_case.coded_height,
            &FrameResourceDescriptor::HostPlanar(descriptor),
        )
        .expect_err("wrong YUV422 chroma height must be rejected");

        assert!(error.to_string().contains("visible size mismatch"));
    }

    #[test]
    fn host_planar_rejects_mismatched_layout_bit_depth() {
        let descriptor = sample_host_planar_descriptor(2);

        let error = validate_resource_descriptor_against_contract(
            VideoFrameContract::host_yuv420_planar8(),
            4,
            4,
            &FrameResourceDescriptor::HostPlanar(descriptor),
        )
        .expect_err("8-bit contract must reject 16-bit host samples");

        assert!(error.to_string().contains("bytes-per-sample mismatch"));
    }

    #[test]
    fn host_planar_clone_for_lookup_preserves_shared_storage_without_copying() {
        let descriptor = FrameResourceDescriptor::HostPlanar(sample_host_planar_descriptor(1));
        let cloned_descriptor = descriptor
            .try_clone_for_lookup()
            .expect("host-planar clone must be infallible");

        let (
            FrameResourceDescriptor::HostPlanar(original),
            FrameResourceDescriptor::HostPlanar(cloned),
        ) = (&descriptor, &cloned_descriptor)
        else {
            panic!("expected host-planar descriptors");
        };

        assert!(std::sync::Arc::ptr_eq(&original.owner, &cloned.owner));
        assert_eq!(std::sync::Arc::strong_count(&original.owner), 2);
        assert_eq!(original.planes, cloned.planes);
    }

    #[test]
    fn host_planar_visible_row_access_returns_only_visible_bytes() {
        let descriptor = HostPlanarFrameDescriptor::from_owned_buffer(
            vec![1, 2, 91, 92, 3, 4, 93, 94],
            vec![HostPlaneDescriptor {
                role: HostPlaneRole::Luma,
                offset: 0,
                stride: 4,
                visible_width: 2,
                visible_height: 2,
                bytes_per_sample: 1,
            }],
        );

        assert_eq!(
            descriptor
                .visible_plane_row_bytes(0, 0)
                .expect("first row must be readable"),
            &[1, 2]
        );
        assert_eq!(
            descriptor
                .visible_plane_row_bytes(0, 1)
                .expect("second row must be readable"),
            &[3, 4]
        );

        let error = descriptor
            .visible_plane_row_bytes(0, 2)
            .expect_err("row outside visible height must be rejected");

        assert!(error.to_string().contains("outside visible height"));
    }

    #[derive(Debug)]
    struct DropCountingHostPlanarOwner {
        owned_buffer: HostPlanarOwnedBuffer,
        drop_count: std::sync::Arc<AtomicUsize>,
    }

    impl HostPlanarFrameOwner for DropCountingHostPlanarOwner {
        fn visible_row_bytes(
            &self,
            plane_index: usize,
            plane: &HostPlaneDescriptor,
            row_index: u32,
            visible_row_bytes: usize,
        ) -> anyhow::Result<&[u8]> {
            self.owned_buffer
                .visible_row_bytes(plane_index, plane, row_index, visible_row_bytes)
        }
    }

    impl Drop for DropCountingHostPlanarOwner {
        fn drop(&mut self) {
            self.drop_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn host_planar_descriptor_clone_keeps_owner_alive_until_last_drop() {
        let drop_count = std::sync::Arc::new(AtomicUsize::new(0));
        let descriptor = HostPlanarFrameDescriptor::new(
            std::sync::Arc::new(DropCountingHostPlanarOwner {
                owned_buffer: HostPlanarOwnedBuffer::new(vec![11, 12, 13, 14]),
                drop_count: std::sync::Arc::clone(&drop_count),
            }),
            vec![HostPlaneDescriptor {
                role: HostPlaneRole::Luma,
                offset: 0,
                stride: 2,
                visible_width: 2,
                visible_height: 2,
                bytes_per_sample: 1,
            }],
        );

        let cloned = descriptor.clone_for_lookup();
        drop(descriptor);

        assert_eq!(drop_count.load(Ordering::SeqCst), 0);
        assert_eq!(
            cloned
                .visible_plane_row_bytes(0, 0)
                .expect("clone must keep owner readable"),
            &[11, 12]
        );

        drop(cloned);

        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn descriptor_validation_accepts_dma_buf_and_host_planar_matching_contracts() {
        validate_resource_descriptor_against_contract(
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
            1920,
            1080,
            &FrameResourceDescriptor::DmaBuf(sample_dma_buf_descriptor()),
        )
        .expect("matching DMA-BUF descriptor must pass");

        validate_resource_descriptor_against_contract(
            VideoFrameContract::host_yuv420_planar8(),
            4,
            4,
            &FrameResourceDescriptor::HostPlanar(sample_host_planar_descriptor(1)),
        )
        .expect("matching host-planar descriptor must pass");
    }

    #[test]
    fn descriptor_validation_rejects_mismatched_descriptor_contract_pairs() {
        let host_error = validate_resource_descriptor_against_contract(
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
            4,
            4,
            &FrameResourceDescriptor::HostPlanar(sample_host_planar_descriptor(1)),
        )
        .expect_err("hardware contract must reject host-planar descriptor");
        assert!(host_error.to_string().contains("requires DMA-BUF"));

        let dma_buf_error = validate_resource_descriptor_against_contract(
            VideoFrameContract::host_yuv420_planar8(),
            1920,
            1080,
            &FrameResourceDescriptor::DmaBuf(sample_dma_buf_descriptor()),
        )
        .expect_err("software contract must reject DMA-BUF descriptor");
        assert!(dma_buf_error.to_string().contains("requires host-planar"));
    }

    #[test]
    fn descriptor_validation_rejects_dma_buf_image_layout_mismatch() {
        let error = validate_resource_descriptor_against_contract(
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
            1920,
            1080,
            &FrameResourceDescriptor::DmaBuf(sample_dma_buf_descriptor()),
        )
        .expect_err("composed DMA-BUF descriptor must not satisfy separate-layer contract");

        assert!(error.to_string().contains("image layout mismatch"));
    }
}
