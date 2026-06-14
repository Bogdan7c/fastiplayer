use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;

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

    /// Byte offset начала plane-а внутри общего `storage`.
    pub offset: usize,

    /// Byte stride между началом соседних строк.
    pub stride: usize,

    /// Видимая ширина plane-а в samples; padding справа сюда не входит.
    pub visible_width: u32,

    /// Видимая высота plane-а в строках.
    pub visible_height: u32,

    /// Размер одного sample в байтах: 1 для 8-bit, 2 для 10-bit LE storage word.
    pub bytes_per_sample: usize,
}

/// Owned host-planar decoded frame без raw pointers и per-plane owners.
#[derive(Debug, Clone)]
pub struct HostPlanarFrameDescriptor {
    /// Общий immutable byte storage всего host frame-а.
    pub storage: Arc<[u8]>,

    /// Per-plane metadata с offsets в общий `storage`.
    pub planes: Vec<HostPlaneDescriptor>,
}

impl HostPlanarFrameDescriptor {
    /// Дешёвый clone для lookup-а: bytes остаются в том же `Arc<[u8]>`.
    #[must_use]
    fn clone_for_lookup(&self) -> Self {
        Self {
            storage: Arc::clone(&self.storage),
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

    for (plane, expected_plane) in descriptor.planes.iter().zip(expected_planes.iter()) {
        validate_host_plane_metadata(pixel_layout, plane, expected_plane)?;
        validate_host_plane_visible_bounds(plane, descriptor.storage.len())?;
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

fn expected_host_planar_planes(
    pixel_layout: VideoFramePixelLayout,
    coded_width: u32,
    coded_height: u32,
) -> anyhow::Result<Vec<ExpectedHostPlane>> {
    let bytes_per_sample = match pixel_layout {
        VideoFramePixelLayout::Yuv420Planar8 => 1,
        VideoFramePixelLayout::Yuv420Planar10Le => 2,
        _ => bail!("{pixel_layout} is not an accepted host-planar layout"),
    };
    let chroma_width = half_rounded_up(coded_width);
    let chroma_height = half_rounded_up(coded_height);

    Ok(vec![
        ExpectedHostPlane {
            role: HostPlaneRole::Luma,
            visible_width: coded_width,
            visible_height: coded_height,
            bytes_per_sample,
        },
        ExpectedHostPlane {
            role: HostPlaneRole::ChromaU,
            visible_width: chroma_width,
            visible_height: chroma_height,
            bytes_per_sample,
        },
        ExpectedHostPlane {
            role: HostPlaneRole::ChromaV,
            visible_width: chroma_width,
            visible_height: chroma_height,
            bytes_per_sample,
        },
    ])
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

fn validate_host_plane_visible_bounds(
    plane: &HostPlaneDescriptor,
    storage_len: usize,
) -> anyhow::Result<()> {
    ensure!(
        plane.visible_width > 0 && plane.visible_height > 0,
        "host-planar {:?} plane has invalid visible size {}x{}",
        plane.role,
        plane.visible_width,
        plane.visible_height
    );

    let visible_width = usize::try_from(plane.visible_width)
        .context("host-planar visible width does not fit usize")?;
    let visible_height = usize::try_from(plane.visible_height)
        .context("host-planar visible height does not fit usize")?;
    let visible_row_bytes = visible_width
        .checked_mul(plane.bytes_per_sample)
        .context("host-planar visible row byte count overflow")?;

    ensure!(
        plane.stride >= visible_row_bytes,
        "host-planar {:?} stride {} is smaller than visible row bytes {}",
        plane.role,
        plane.stride,
        visible_row_bytes
    );

    let last_row_delta = plane
        .stride
        .checked_mul(visible_height.saturating_sub(1))
        .context("host-planar last-row stride overflow")?;
    let last_row_start = plane
        .offset
        .checked_add(last_row_delta)
        .context("host-planar last-row offset overflow")?;
    let readable_end = last_row_start
        .checked_add(visible_row_bytes)
        .context("host-planar visible readable end overflow")?;

    ensure!(
        readable_end <= storage_len,
        "host-planar {:?} visible area exceeds storage: end {}, storage {}",
        plane.role,
        readable_end,
        storage_len
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::fd::{AsFd, AsRawFd, OwnedFd};

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

    fn sample_host_planar_descriptor(bytes_per_sample: usize) -> HostPlanarFrameDescriptor {
        let width = 4;
        let height = 4;
        let y_stride = width * bytes_per_sample;
        let chroma_stride = (width / 2) * bytes_per_sample;
        let y_size = y_stride * height;
        let u_size = chroma_stride * (height / 2);
        let v_size = u_size;
        let storage = vec![0; y_size + u_size + v_size].into();

        HostPlanarFrameDescriptor {
            storage,
            planes: vec![
                HostPlaneDescriptor {
                    role: HostPlaneRole::Luma,
                    offset: 0,
                    stride: y_stride,
                    visible_width: width as u32,
                    visible_height: height as u32,
                    bytes_per_sample,
                },
                HostPlaneDescriptor {
                    role: HostPlaneRole::ChromaU,
                    offset: y_size,
                    stride: chroma_stride,
                    visible_width: (width / 2) as u32,
                    visible_height: (height / 2) as u32,
                    bytes_per_sample,
                },
                HostPlaneDescriptor {
                    role: HostPlaneRole::ChromaV,
                    offset: y_size + u_size,
                    stride: chroma_stride,
                    visible_width: (width / 2) as u32,
                    visible_height: (height / 2) as u32,
                    bytes_per_sample,
                },
            ],
        }
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
    fn host_planar_accepts_valid_yuv420_8_and_10_descriptors() {
        let planar8 = FrameResourceDescriptor::HostPlanar(sample_host_planar_descriptor(1));
        let planar10 = FrameResourceDescriptor::HostPlanar(sample_host_planar_descriptor(2));

        validate_resource_descriptor_against_contract(
            VideoFrameContract::host_yuv420_planar8(),
            4,
            4,
            &planar8,
        )
        .expect("valid 8-bit host-planar descriptor must pass");
        validate_resource_descriptor_against_contract(
            VideoFrameContract::host_yuv420_planar10le(),
            4,
            4,
            &planar10,
        )
        .expect("valid 10-bit host-planar descriptor must pass");
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
        descriptor.planes[2].offset = descriptor.storage.len();

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
        let descriptor = HostPlanarFrameDescriptor {
            storage: vec![0; 40].into(),
            planes: vec![
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
        };

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
        let descriptor = HostPlanarFrameDescriptor {
            storage: vec![0; 29].into(),
            planes: vec![
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
        };

        validate_resource_descriptor_against_contract(
            VideoFrameContract::host_yuv420_planar8(),
            5,
            3,
            &FrameResourceDescriptor::HostPlanar(descriptor),
        )
        .expect("odd YUV420 dimensions must round chroma plane sizes up");
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

        assert!(Arc::ptr_eq(&original.storage, &cloned.storage));
        assert_eq!(original.planes, cloned.planes);
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
