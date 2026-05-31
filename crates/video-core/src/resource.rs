use std::io;
use std::os::fd::{AsFd, OwnedFd};

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
}

impl FrameResourceDescriptor {
    /// Дублирует owned platform handles для renderer-side materializer-а.
    ///
    /// Provider продолжает владеть исходным descriptor-ом и decoder frame-ом;
    /// renderer получает отдельные fd и обязан закрыть их через обычный drop.
    pub fn try_clone_for_lookup(&self) -> io::Result<Self> {
        match self {
            Self::DmaBuf(descriptor) => descriptor.try_clone_for_lookup().map(Self::DmaBuf),
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
        }
    }
}
