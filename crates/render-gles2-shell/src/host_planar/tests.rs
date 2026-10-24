use gles2_video::{ChromaSubsampling, PlaneExtent, PlaneSource, VideoFrameKey};
use video_frame_contract::{VideoFramePixelLayout, VideoFrameTransferPath};

use super::*;
use crate::test_support::host_frame;

fn host_rows(source: PlaneSource<'_>) -> HostRows<'_> {
    match source {
        PlaneSource::HostRows(rows) => rows,
        PlaneSource::DmaBuf(_) => panic!("host-planar frame must map to host rows"),
    }
}

#[test]
fn padded_420_frame_maps_to_borrowed_plane_blocks() {
    let test_frame = host_frame(
        VideoFramePixelLayout::Yuv420Planar8,
        [5, 3],
        3,
        [16, 128, 240],
    );
    let upload = host_planar_frame_upload(&test_frame.frame, &test_frame.descriptor)
        .expect("frame maps to planes");

    assert_eq!(upload.key, VideoFrameKey(42));
    assert_eq!(upload.layout.chroma, ChromaSubsampling::Yuv420);
    assert_eq!(
        upload.luma_extent,
        PlaneExtent {
            width: 5,
            height: 3
        }
    );

    let luma = host_rows(upload.luma);
    assert_eq!(
        (luma.stride(), luma.visible_row_bytes(), luma.rows()),
        (8, 5, 3)
    );
    // Блок кончается на видимой части последней строки: 2 × 8 + 5.
    assert_eq!(luma.bytes().len(), 21);
    assert!(luma.bytes()[..5].iter().all(|&sample| sample == 16));

    let chroma_v = host_rows(upload.chroma_v);
    assert_eq!(
        (
            chroma_v.stride(),
            chroma_v.visible_row_bytes(),
            chroma_v.rows()
        ),
        (6, 3, 2)
    );
    assert_eq!(chroma_v.bytes()[0], 240);

    // Срезы заимствуют память дескриптора — копии нет.
    let FrameResourceDescriptor::HostPlanar(descriptor) = &test_frame.descriptor else {
        panic!("host-planar test frame");
    };
    let block = descriptor.visible_plane_block(0).expect("luma block");
    assert_eq!(luma.bytes().as_ptr(), block.bytes.as_ptr());
}

#[test]
fn every_8bit_layout_maps_to_its_chroma_subsampling() {
    for (layout, chroma) in [
        (
            VideoFramePixelLayout::Yuv420Planar8,
            ChromaSubsampling::Yuv420,
        ),
        (
            VideoFramePixelLayout::Yuv422Planar8,
            ChromaSubsampling::Yuv422,
        ),
        (
            VideoFramePixelLayout::Yuv444Planar8,
            ChromaSubsampling::Yuv444,
        ),
    ] {
        let test_frame = host_frame(layout, [6, 4], 0, [16, 128, 128]);
        let upload = host_planar_frame_upload(&test_frame.frame, &test_frame.descriptor)
            .expect("8-bit layouts are supported");
        assert_eq!(upload.layout.chroma, chroma, "{layout:?}");
        assert_eq!(
            host_rows(upload.chroma_u).rows(),
            chroma.chroma_extent(upload.luma_extent).height
        );
    }
}

#[test]
fn high_bit_depth_layout_is_rejected_until_session_06() {
    let mut test_frame = host_frame(
        VideoFramePixelLayout::Yuv420Planar8,
        [4, 4],
        0,
        [16, 128, 128],
    );
    test_frame.frame.frame_contract.pixel_layout = VideoFramePixelLayout::Yuv420Planar10Le;
    assert_eq!(
        host_planar_frame_upload(&test_frame.frame, &test_frame.descriptor).err(),
        Some(HostPlanarFrameError::UnsupportedLayout(
            VideoFramePixelLayout::Yuv420Planar10Le
        ))
    );
}

#[test]
fn hardware_contract_with_host_descriptor_is_rejected() {
    let mut test_frame = host_frame(
        VideoFramePixelLayout::Yuv420Planar8,
        [4, 4],
        0,
        [16, 128, 128],
    );
    test_frame.frame.frame_contract = video_frame_contract::VideoFrameContract::dma_buf_nv12(
        video_frame_contract::DmaBufImageLayout::ComposedLayers,
    );
    assert!(matches!(
        host_planar_frame_upload(&test_frame.frame, &test_frame.descriptor),
        Err(HostPlanarFrameError::NotSoftwareHostUpload(
            VideoFrameTransferPath::HardwareZeroCopy { .. }
        ))
    ));
}

#[test]
fn descriptor_that_disagrees_with_the_contract_is_rejected_by_video_core() {
    // Дескриптор 4:4:4 под контрактом 4:2:0: размеры цветности не те.
    let mut test_frame = host_frame(
        VideoFramePixelLayout::Yuv444Planar8,
        [4, 4],
        0,
        [16, 128, 128],
    );
    test_frame.frame.frame_contract.pixel_layout = VideoFramePixelLayout::Yuv420Planar8;
    assert!(matches!(
        host_planar_frame_upload(&test_frame.frame, &test_frame.descriptor),
        Err(HostPlanarFrameError::ContractRejected { .. })
    ));
}
