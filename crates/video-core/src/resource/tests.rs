use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use video_frame_contract::{DmaBufImageLayout, VideoFramePixelLayout};

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

/// Поддерживаемый composed multi-planar descriptor использует один object для всех planes.
#[test]
fn dma_buf_import_topology_accepts_multiplanar_single_object() {
    let descriptor = sample_dma_buf_descriptor();

    validate_dma_buf_descriptor_import_topology(&descriptor)
        .expect("single-object composed descriptor must remain supported");
}

/// Separate layers остаются отдельным supported path и могут ссылаться на разные objects.
#[test]
fn dma_buf_import_topology_accepts_separate_layers_with_distinct_objects() {
    let mut descriptor = sample_dma_buf_descriptor();
    descriptor.export_layout = DmaBufFrameExportLayout::SeparateLayers;
    descriptor.objects.push(DmaBufObjectDescriptor {
        fd: open_test_dma_buf_fd(),
        size: 2048,
        drm_format_modifier: 7,
        identity: DmaBufObjectIdentity {
            device: 11,
            inode: 19,
            special_device: 17,
        },
    });
    descriptor.layers = vec![
        DmaBufLayerDescriptor {
            drm_format: 0x2020_3852,
            num_planes: 1,
            object_index: [0, 0, 0, 0],
            offset: [0, 0, 0, 0],
            pitch: [1920, 0, 0, 0],
        },
        DmaBufLayerDescriptor {
            drm_format: 0x3838_5247,
            num_planes: 1,
            object_index: [1, 0, 0, 0],
            offset: [0, 0, 0, 0],
            pitch: [1920, 0, 0, 0],
        },
    ];

    validate_dma_buf_descriptor_import_topology(&descriptor)
        .expect("separate layers may be imported from distinct objects");
}

/// Некорректный object index диагностируется до доступа importer-а к objects vector.
#[test]
fn dma_buf_import_topology_rejects_out_of_bounds_object_index() {
    let mut descriptor = sample_dma_buf_descriptor();
    descriptor.layers[0].object_index[1] = 3;

    assert_eq!(
        validate_dma_buf_descriptor_import_topology(&descriptor),
        Err(DmaBufDescriptorRejection::ObjectIndexOutOfBounds {
            layer_index: 0,
            plane_index: 1,
            object_index: 3,
            object_count: 1,
        })
    );
}

/// Composed image с planes в разных objects — именно unsupported layout, не import error.
#[test]
fn dma_buf_import_topology_rejects_composed_multi_object_layout() {
    let mut descriptor = sample_dma_buf_descriptor();
    descriptor.objects.push(DmaBufObjectDescriptor {
        fd: open_test_dma_buf_fd(),
        size: 2048,
        drm_format_modifier: 7,
        identity: DmaBufObjectIdentity {
            device: 11,
            inode: 23,
            special_device: 17,
        },
    });
    descriptor.layers[0].object_index[1] = 1;

    assert_eq!(
        validate_dma_buf_descriptor_import_topology(&descriptor),
        Err(DmaBufDescriptorRejection::UnsupportedComposedMultiObject {
            first_object_index: 0,
            conflicting_object_index: 1,
        })
    );
}

/// Пустой descriptor сохраняет absent state отдельно от unsupported multi-object.
#[test]
fn dma_buf_import_topology_rejects_absent_objects_and_layers_separately() {
    let mut descriptor_without_objects = sample_dma_buf_descriptor();
    descriptor_without_objects.objects.clear();
    assert_eq!(
        validate_dma_buf_descriptor_import_topology(&descriptor_without_objects),
        Err(DmaBufDescriptorRejection::AbsentObjects)
    );

    let mut descriptor_without_layers = sample_dma_buf_descriptor();
    descriptor_without_layers.layers.clear();
    assert_eq!(
        validate_dma_buf_descriptor_import_topology(&descriptor_without_layers),
        Err(DmaBufDescriptorRejection::AbsentLayers)
    );
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
    let descriptor =
        FrameResourceDescriptor::HostPlanar(sample_host_planar_descriptor_for_case(layout_case));

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

    fn visible_plane_block(
        &self,
        plane_index: usize,
        plane: &HostPlaneDescriptor,
        block_len: usize,
    ) -> anyhow::Result<&[u8]> {
        self.owned_buffer
            .visible_plane_block(plane_index, plane, block_len)
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

#[test]
fn dma_buf_frame_contract_validation_accepts_exact_descriptor_match() {
    let descriptor = sample_dma_buf_descriptor();

    assert_eq!(
        validate_dma_buf_descriptor_against_frame_contract(
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
            1920,
            1080,
            &descriptor,
        ),
        Ok(())
    );
}

#[test]
fn dma_buf_frame_contract_validation_returns_typed_layout_and_size_rejections() {
    let descriptor = sample_dma_buf_descriptor();

    let layout_rejection = validate_dma_buf_descriptor_against_frame_contract(
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        1920,
        1080,
        &descriptor,
    )
    .expect_err("несовпадающий DMA-BUF layout должен быть отклонён");
    assert_eq!(
        layout_rejection,
        DmaBufDescriptorRejection::ImageLayoutMismatch {
            expected: DmaBufImageLayout::SeparateLayers,
            actual: DmaBufImageLayout::ComposedLayers,
        }
    );
    assert!(
        layout_rejection
            .to_string()
            .contains("image layout mismatch")
    );

    let size_rejection = validate_dma_buf_descriptor_against_frame_contract(
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
        1280,
        720,
        &descriptor,
    )
    .expect_err("несовпадающий coded size должен быть отклонён");
    assert_eq!(
        size_rejection,
        DmaBufDescriptorRejection::CodedSizeMismatch {
            expected_width: 1280,
            expected_height: 720,
            actual_width: 1920,
            actual_height: 1080,
        }
    );
    assert!(size_rejection.to_string().contains("1280x720"));
}

#[test]
fn dma_buf_frame_contract_validation_rejects_wrong_transfer_path_and_zero_size() {
    let descriptor = sample_dma_buf_descriptor();

    let transfer_rejection = validate_dma_buf_descriptor_against_frame_contract(
        VideoFrameContract::host_yuv420_planar8(),
        1920,
        1080,
        &descriptor,
    )
    .expect_err("host-upload contract не должен принимать DMA-BUF descriptor");
    assert_eq!(
        transfer_rejection,
        DmaBufDescriptorRejection::FrameContractRequiresHostUpload
    );
    assert!(
        transfer_rejection
            .to_string()
            .contains("requires host upload")
    );

    let size_rejection = validate_dma_buf_descriptor_against_frame_contract(
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
        0,
        1080,
        &descriptor,
    )
    .expect_err("нулевой coded size должен быть отклонён до импорта");
    assert_eq!(
        size_rejection,
        DmaBufDescriptorRejection::InvalidCodedSize {
            coded_width: 0,
            coded_height: 1080,
        }
    );
    assert!(size_rejection.to_string().contains("got 0x1080"));
}

#[test]
fn dma_buf_frame_contract_validation_preserves_topology_rejection() {
    let mut descriptor_without_objects = sample_dma_buf_descriptor();
    descriptor_without_objects.objects.clear();

    assert_eq!(
        validate_dma_buf_descriptor_against_frame_contract(
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
            1920,
            1080,
            &descriptor_without_objects,
        ),
        Err(DmaBufDescriptorRejection::AbsentObjects)
    );
}
