//! Разбор дескриптора DMA-BUF в кадр NV12 видеослоя: верные смещения, шаги, fd, модификаторы и
//! каждый отказ отдельной причиной.

use std::os::fd::AsRawFd;

use gles2_egui::DrmFourcc;
use gles2_video::{DmaBufBufferIdentity, DmaBufFrameError, DmaBufPlaneRole, VideoFrameKey};
use video_core::{DecodedFrame, DmaBufFrameDescriptor, DmaBufFrameExportLayout};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use super::support::{
    HEIGHT, MODIFIER, PITCH, WIDTH, composed_descriptor, contract, frame, object, p010_descriptor,
    separate_descriptor,
};
use crate::{Nv12DmaBufError, dma_buf_buffer_identity, nv12_dma_buf_frame};

fn passport(descriptor: &DmaBufFrameDescriptor) -> DmaBufBufferIdentity {
    dma_buf_buffer_identity(&descriptor.identity()).unwrap()
}

fn composed_frame() -> DecodedFrame {
    frame(contract(DmaBufImageLayout::ComposedLayers), 41)
}

fn separate_frame() -> DecodedFrame {
    frame(contract(DmaBufImageLayout::SeparateLayers), 42)
}

#[test]
fn composed_descriptor_gives_both_planes_from_one_object() {
    let descriptor = composed_descriptor(1, 10);
    let passport = passport(&descriptor);
    let nv12 = nv12_dma_buf_frame(&composed_frame(), &descriptor, &passport).unwrap();
    assert_eq!(nv12.key(), VideoFrameKey(41));
    assert_eq!(
        (nv12.luma_extent().width, nv12.luma_extent().height),
        (WIDTH, HEIGHT)
    );
    assert_eq!(
        (nv12.chroma_extent().width, nv12.chroma_extent().height),
        (WIDTH / 2, HEIGHT / 2)
    );
    let object_fd = descriptor.objects[0].fd.as_raw_fd();
    assert_eq!(nv12.luma().fd.as_raw_fd(), object_fd);
    assert_eq!(nv12.chroma().fd.as_raw_fd(), object_fd);
    assert_eq!(nv12.luma_offset(), 0);
    assert_eq!(nv12.chroma_offset(), PITCH * HEIGHT);
    assert_eq!((nv12.luma().pitch, nv12.chroma().pitch), (PITCH, PITCH));
    assert_eq!(nv12.luma().drm_format, DrmFourcc::R8.0);
    assert_eq!(nv12.chroma().drm_format, DrmFourcc::GR88.0);
    assert_eq!(nv12.luma().modifier, MODIFIER);
    assert_eq!(nv12.chroma().modifier, MODIFIER);
}

#[test]
fn separate_descriptor_gives_each_plane_its_own_object_offset_and_pitch() {
    let descriptor = separate_descriptor(2, 20);
    let passport = passport(&descriptor);
    let nv12 = nv12_dma_buf_frame(&separate_frame(), &descriptor, &passport).unwrap();
    assert_eq!(
        nv12.luma().fd.as_raw_fd(),
        descriptor.objects[0].fd.as_raw_fd()
    );
    assert_eq!(
        nv12.chroma().fd.as_raw_fd(),
        descriptor.objects[1].fd.as_raw_fd()
    );
    assert_ne!(nv12.luma().fd.as_raw_fd(), nv12.chroma().fd.as_raw_fd());
    assert_eq!((nv12.luma_offset(), nv12.chroma_offset()), (64, 128));
    assert_eq!(
        (nv12.luma().pitch, nv12.chroma().pitch),
        (PITCH, PITCH + 64)
    );
}

#[test]
fn p010_and_other_formats_are_refused_before_the_driver() {
    let descriptor = p010_descriptor(3, 30);
    let passport = passport(&composed_descriptor(3, 30));
    let p010_frame = frame(
        VideoFrameContract::dma_buf_p010(DmaBufImageLayout::ComposedLayers),
        1,
    );
    assert!(matches!(
        nv12_dma_buf_frame(&p010_frame, &descriptor, &passport),
        Err(Nv12DmaBufError::TenBitHardwareNotDeclared { .. })
    ));
    // Контракт NV12, но дескриптор P010: тоже отдельная причина.
    assert!(matches!(
        nv12_dma_buf_frame(&composed_frame(), &descriptor, &passport),
        Err(Nv12DmaBufError::TenBitHardwareNotDeclared { .. })
    ));
    let mut other = composed_descriptor(3, 30);
    other.fourcc = 0x3432_5559;
    assert_eq!(
        nv12_dma_buf_frame(&composed_frame(), &other, &passport).unwrap_err(),
        Nv12DmaBufError::NotNv12 {
            fourcc: 0x3432_5559
        }
    );
}

#[test]
fn export_layout_that_differs_from_the_passport_is_refused() {
    let descriptor = composed_descriptor(4, 40);
    let separate_passport = passport(&separate_descriptor(4, 40));
    assert_eq!(
        nv12_dma_buf_frame(&composed_frame(), &descriptor, &separate_passport).unwrap_err(),
        Nv12DmaBufError::ExportLayoutDiffersFromIdentity {
            descriptor: DmaBufFrameExportLayout::ComposedLayers
        }
    );
}

#[test]
fn composed_layer_problems_are_reported_one_by_one() {
    let frame = composed_frame();
    let build = |edit: &dyn Fn(&mut DmaBufFrameDescriptor)| {
        let mut descriptor = composed_descriptor(5, 50);
        edit(&mut descriptor);
        let passport = passport(&composed_descriptor(5, 50));
        nv12_dma_buf_frame(&frame, &descriptor, &passport).unwrap_err()
    };
    assert_eq!(
        build(&|descriptor| descriptor.layers.push(descriptor.layers[0].clone())),
        Nv12DmaBufError::LayerCount {
            expected: 1,
            actual: 2
        }
    );
    assert_eq!(
        build(&|descriptor| descriptor.layers[0].drm_format = DrmFourcc::R8.0),
        Nv12DmaBufError::LayerFormat {
            layer_index: 0,
            expected: 0x3231_564e,
            actual: DrmFourcc::R8.0
        }
    );
    assert_eq!(
        build(&|descriptor| descriptor.layers[0].num_planes = 1),
        Nv12DmaBufError::LayerPlaneCount {
            layer_index: 0,
            expected: 2,
            actual: 1
        }
    );
    assert_eq!(
        build(&|descriptor| descriptor.layers[0].object_index[1] = 5),
        Nv12DmaBufError::ObjectIndexOutOfRange {
            layer_index: 0,
            plane_index: 1,
            object_index: 5,
            object_count: 1
        }
    );
    // Две плоскости в разных объектах: это не один composed-образ.
    assert_eq!(
        build(&|descriptor| {
            descriptor.objects.push(object(51, MODIFIER));
            descriptor.layers[0].object_index[1] = 1;
        }),
        Nv12DmaBufError::ComposedPlanesInDifferentObjects {
            luma_object_index: 0,
            chroma_object_index: 1
        }
    );
}

#[test]
fn separate_layer_problems_are_reported_one_by_one() {
    let frame = separate_frame();
    let build = |edit: &dyn Fn(&mut DmaBufFrameDescriptor)| {
        let mut descriptor = separate_descriptor(6, 60);
        edit(&mut descriptor);
        let passport = passport(&separate_descriptor(6, 60));
        nv12_dma_buf_frame(&frame, &descriptor, &passport).unwrap_err()
    };
    assert_eq!(
        build(&|descriptor| {
            descriptor.layers.pop();
        }),
        Nv12DmaBufError::LayerCount {
            expected: 2,
            actual: 1
        }
    );
    // Слои в обратном порядке: яркость там, где должна быть цветность.
    assert_eq!(
        build(&|descriptor| descriptor.layers.swap(0, 1)),
        Nv12DmaBufError::LayerFormat {
            layer_index: 0,
            expected: DrmFourcc::R8.0,
            actual: DrmFourcc::GR88.0
        }
    );
    assert_eq!(
        build(&|descriptor| descriptor.layers[1].num_planes = 2),
        Nv12DmaBufError::LayerPlaneCount {
            layer_index: 1,
            expected: 1,
            actual: 2
        }
    );
    assert_eq!(
        build(&|descriptor| descriptor.layers[1].object_index[0] = 9),
        Nv12DmaBufError::ObjectIndexOutOfRange {
            layer_index: 1,
            plane_index: 0,
            object_index: 9,
            object_count: 2
        }
    );
}

/// Размер и модификатор кадра сверяются с паспортом: кэш не должен отдать чужую текстуру.
#[test]
fn frame_that_contradicts_its_passport_is_refused() {
    let mut wider = composed_descriptor(7, 70);
    wider.width = WIDTH * 2;
    let passport = passport(&composed_descriptor(7, 70));
    assert!(matches!(
        nv12_dma_buf_frame(&composed_frame(), &wider, &passport).unwrap_err(),
        Nv12DmaBufError::Frame(DmaBufFrameError::LumaExtentDiffersFromIdentity { .. })
    ));
    let mut other_modifier = composed_descriptor(7, 70);
    other_modifier.objects[0].drm_format_modifier = 0x99;
    assert!(matches!(
        nv12_dma_buf_frame(&composed_frame(), &other_modifier, &passport).unwrap_err(),
        Nv12DmaBufError::Frame(DmaBufFrameError::PlaneModifierDiffersFromIdentity {
            plane: DmaBufPlaneRole::Luma,
            ..
        })
    ));
    let mut short_pitch = composed_descriptor(7, 70);
    short_pitch.layers[0].pitch[0] = WIDTH - 1;
    assert!(matches!(
        nv12_dma_buf_frame(&composed_frame(), &short_pitch, &passport).unwrap_err(),
        Nv12DmaBufError::Frame(DmaBufFrameError::PitchShorterThanRow {
            plane: DmaBufPlaneRole::Luma,
            ..
        })
    ));
}

/// Контракт кадра, который не NV12 (планарный YUV), отвергается, не дойдя до слоёв.
#[test]
fn non_nv12_frame_contract_is_refused() {
    let descriptor = composed_descriptor(8, 80);
    let passport = passport(&descriptor);
    let host_frame = frame(
        VideoFrameContract {
            pixel_layout: video_frame_contract::VideoFramePixelLayout::Yuv420Planar8,
            transfer_path: video_frame_contract::VideoFrameTransferPath::SoftwareHostUpload,
        },
        1,
    );
    assert!(matches!(
        nv12_dma_buf_frame(&host_frame, &descriptor, &passport).unwrap_err(),
        Nv12DmaBufError::FrameContractNotNv12 { .. }
    ));
}
