//! Кадры декодера для тестов адаптера: настоящий `DecodedFrame` + `HostPlanarFrameDescriptor`
//! поверх своих байтов (как отдаёт FFmpeg: строки со stride, хвост последней строки отрезан).

use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use video_core::{
    DecodedFrame, FrameResourceDescriptor, FrameResourceHandle, HostPlanarFrameDescriptor,
    HostPlaneDescriptor, HostPlaneRole, VideoFrameDiagnostics,
};
use video_frame_contract::{VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath};

/// Кадр программного декодера в памяти теста.
pub(crate) struct TestHostFrame {
    pub(crate) frame: DecodedFrame,
    pub(crate) descriptor: FrameResourceDescriptor,
}

/// Кадр `layout` размером `width × height`; каждая плоскость заполнена своим байтом
/// (`samples` = Y, U, V), к stride добавлено `row_tail` байт.
pub(crate) fn host_frame(
    layout: VideoFramePixelLayout,
    [width, height]: [u32; 2],
    row_tail: usize,
    samples: [u8; 3],
) -> TestHostFrame {
    let (chroma_width, chroma_height) = match layout {
        VideoFramePixelLayout::Yuv420Planar8 => (width.div_ceil(2), height.div_ceil(2)),
        VideoFramePixelLayout::Yuv422Planar8 => (width.div_ceil(2), height),
        _ => (width, height),
    };
    let mut bytes = Vec::new();
    let mut planes = Vec::new();
    for (role, (plane_width, plane_height), sample) in [
        (HostPlaneRole::Luma, (width, height), samples[0]),
        (
            HostPlaneRole::ChromaU,
            (chroma_width, chroma_height),
            samples[1],
        ),
        (
            HostPlaneRole::ChromaV,
            (chroma_width, chroma_height),
            samples[2],
        ),
    ] {
        let stride = plane_width as usize + row_tail;
        let offset = bytes.len();
        for row in 0..plane_height {
            bytes.extend(std::iter::repeat_n(sample, plane_width as usize));
            if row + 1 < plane_height {
                bytes.extend(std::iter::repeat_n(0xEE, row_tail));
            }
        }
        planes.push(HostPlaneDescriptor {
            role,
            offset,
            stride,
            visible_width: plane_width,
            visible_height: plane_height,
            bytes_per_sample: 1,
        });
    }
    TestHostFrame {
        frame: DecodedFrame {
            generation: 1,
            pts: Duration::ZERO,
            frame_contract: VideoFrameContract {
                pixel_layout: layout,
                transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
            },
            width,
            height,
            render_width: width,
            render_height: height,
            display_orientation: VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle: FrameResourceHandle(42),
            diagnostics: VideoFrameDiagnostics::default(),
        },
        descriptor: FrameResourceDescriptor::HostPlanar(
            HostPlanarFrameDescriptor::from_owned_buffer(bytes, planes),
        ),
    }
}
