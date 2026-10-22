use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ptr::NonNull;
use std::sync::Arc;

use video_core::{
    HOST_IMPORTABLE_BLOCK_ALIGNMENT, HostImportableMemoryBlock, HostMemoryBlockId,
    HostMemoryRegion, HostPlanarFrameDescriptor, HostPlanarFrameOwner, HostPlanarImportableMemory,
    HostPlaneDescriptor, HostPlaneRole,
};

use super::*;
use crate::video::host_planar_upload::{
    HostPlanarUploadPlaneLayout, HostPlanarUploadTextureFormat,
};

/// Пустой блок памяти, выровненный по странице (содержимое планированию не нужно).
#[derive(Debug)]
struct PageBlock {
    id: HostMemoryBlockId,
    address: NonNull<u8>,
    layout: Layout,
}

// SAFETY: блок владеет allocation-ой эксклюзивно; наружу уходят только id и диапазон.
unsafe impl Send for PageBlock {}
// SAFETY: см. `Send`.
unsafe impl Sync for PageBlock {}

impl PageBlock {
    fn new() -> Arc<Self> {
        let layout = Layout::from_size_align(
            HOST_IMPORTABLE_BLOCK_ALIGNMENT,
            HOST_IMPORTABLE_BLOCK_ALIGNMENT,
        )
        .expect("valid layout");
        // SAFETY: layout ненулевой.
        let address = NonNull::new(unsafe { alloc_zeroed(layout) }).expect("allocation");
        Arc::new(Self {
            id: HostMemoryBlockId::allocate(),
            address,
            layout,
        })
    }
}

impl Drop for PageBlock {
    fn drop(&mut self) {
        // SAFETY: выделено `alloc_zeroed` с этим layout-ом, освобождается один раз.
        unsafe { dealloc(self.address.as_ptr(), self.layout) };
    }
}

// SAFETY: диапазон постоянный, выровнен по странице, память жива до Drop.
unsafe impl HostImportableMemoryBlock for PageBlock {
    fn id(&self) -> HostMemoryBlockId {
        self.id
    }

    fn region(&self) -> HostMemoryRegion {
        HostMemoryRegion::new(self.address, self.layout.size())
    }
}

/// Владелец, который отдаёт блок с заданными offset-ами (строки не читаются).
#[derive(Debug)]
struct ImportableOwner {
    block: Arc<PageBlock>,
    offsets: Vec<usize>,
}

impl HostPlanarFrameOwner for ImportableOwner {
    fn visible_row_bytes(
        &self,
        _plane_index: usize,
        _plane: &HostPlaneDescriptor,
        _row_index: u32,
        _visible_row_bytes: usize,
    ) -> anyhow::Result<&[u8]> {
        anyhow::bail!("planning never reads rows")
    }

    fn visible_plane_block(
        &self,
        _plane_index: usize,
        _plane: &HostPlaneDescriptor,
        _block_len: usize,
    ) -> anyhow::Result<&[u8]> {
        anyhow::bail!("planning never reads plane blocks")
    }

    fn importable_memory(
        &self,
        _planes: &[HostPlaneDescriptor],
    ) -> Option<HostPlanarImportableMemory> {
        let block: Arc<dyn HostImportableMemoryBlock> = Arc::clone(&self.block) as _;
        Some(HostPlanarImportableMemory::new(block, self.offsets.clone()))
    }
}

/// Plane-ы 4×2 YUV 4:2:0 8-bit с шагом `stride`.
fn planes(stride: usize) -> Vec<HostPlaneDescriptor> {
    let plane = |role, width, height| HostPlaneDescriptor {
        role,
        offset: 0,
        stride,
        visible_width: width,
        visible_height: height,
        bytes_per_sample: 1,
    };
    vec![
        plane(HostPlaneRole::Luma, 4, 2),
        plane(HostPlaneRole::ChromaU, 2, 1),
        plane(HostPlaneRole::ChromaV, 2, 1),
    ]
}

/// Раскладка слота под тот же кадр 4×2.
fn slot_layout() -> HostPlanarUploadLayout {
    let plane = |role, width, height| HostPlanarUploadPlaneLayout {
        role,
        width,
        height,
        bytes_per_sample: 1,
        texture_format: HostPlanarUploadTextureFormat::R8Unorm,
    };
    HostPlanarUploadLayout {
        planes: [
            plane(HostPlaneRole::Luma, 4, 2),
            plane(HostPlaneRole::ChromaU, 2, 1),
            plane(HostPlaneRole::ChromaV, 2, 1),
        ],
    }
}

fn importable_descriptor(stride: usize, offsets: Vec<usize>) -> HostPlanarFrameDescriptor {
    let owner = ImportableOwner {
        block: PageBlock::new(),
        offsets,
    };
    HostPlanarFrameDescriptor::new(Arc::new(owner), planes(stride))
}

#[test]
fn importable_frame_with_gpu_row_pitch_is_planned() {
    let descriptor = importable_descriptor(256, vec![0, 512, 768]);

    let DirectUploadPlanning::Planned(upload) = plan_direct_upload(&descriptor, slot_layout())
    else {
        panic!("frame in a page block with 256-byte rows must be planned");
    };

    assert_eq!(
        [upload.plane(0), upload.plane(1), upload.plane(2)],
        [
            Some(DirectPlaneCopy {
                offset: 0,
                bytes_per_row: 256
            }),
            Some(DirectPlaneCopy {
                offset: 512,
                bytes_per_row: 256
            }),
            Some(DirectPlaneCopy {
                offset: 768,
                bytes_per_row: 256
            }),
        ]
    );
    assert_eq!(upload.plane(3), None);
}

#[test]
fn planned_upload_keeps_the_frame_alive() {
    let descriptor = importable_descriptor(256, vec![0, 512, 768]);
    let DirectUploadPlanning::Planned(upload) = plan_direct_upload(&descriptor, slot_layout())
    else {
        panic!("frame must be planned");
    };

    let block_id = upload.memory().block().id();
    drop(descriptor);

    // План держит свой дескриптор кадра: его GPU-копия переживёт отпускание исходного.
    let still_planned = plan_direct_upload(upload.frame(), slot_layout());
    assert!(matches!(still_planned, DirectUploadPlanning::Planned(_)));
    assert_eq!(upload.memory().block().id(), block_id);
}

#[test]
fn ordinary_frame_is_not_importable() {
    let descriptor = HostPlanarFrameDescriptor::from_owned_buffer(vec![0_u8; 4096], planes(256));

    assert!(matches!(
        plan_direct_upload(&descriptor, slot_layout()),
        DirectUploadPlanning::NotImportable
    ));
}

#[test]
fn row_pitch_not_multiple_of_256_is_unsuitable() {
    // Обычный stride FFmpeg (64): GPU-копия буфер → текстура в wgpu так не умеет.
    let descriptor = importable_descriptor(64, vec![0, 128, 192]);

    let DirectUploadPlanning::Unsuitable(reason) = plan_direct_upload(&descriptor, slot_layout())
    else {
        panic!("64-byte rows must be unsuitable for the direct copy");
    };
    assert!(
        reason.to_string().contains("not a multiple of 256"),
        "{reason}"
    );
}

#[test]
fn plane_that_does_not_match_slot_is_unsuitable() {
    let descriptor = importable_descriptor(256, vec![0, 512, 768]);
    let mut wider_slot = slot_layout();
    wider_slot.planes[0].width = 8;

    let DirectUploadPlanning::Unsuitable(reason) = plan_direct_upload(&descriptor, wider_slot)
    else {
        panic!("size mismatch must be unsuitable");
    };
    assert!(
        reason.to_string().contains("does not match upload layout"),
        "{reason}"
    );
}

#[test]
fn inconsistent_block_is_unsuitable() {
    // Последняя plane выходит за блок (4096 байт): descriptor отвергает блок целиком.
    let descriptor = importable_descriptor(256, vec![0, 512, 4096 - 1]);

    assert!(matches!(
        plan_direct_upload(&descriptor, slot_layout()),
        DirectUploadPlanning::Unsuitable(_)
    ));
}
