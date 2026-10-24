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
    size: [u32; 2],
    row_tail: usize,
    samples: [u8; 3],
) -> TestHostFrame {
    build_host_frame(layout, size, row_tail, samples.map(|sample| vec![sample]))
}

/// Кадр 10/12 бит: каждая плоскость заполнена своим кодом (`codes` = Y, U, V) 16-битными словами
/// LE, к stride добавлено `row_tail_samples` отсчётов.
pub(crate) fn high_bit_host_frame(
    layout: VideoFramePixelLayout,
    size: [u32; 2],
    row_tail_samples: usize,
    codes: [u16; 3],
) -> TestHostFrame {
    build_host_frame(
        layout,
        size,
        row_tail_samples,
        codes.map(|code| code.to_le_bytes().to_vec()),
    )
}

/// Общая сборка: `sample_bytes` — байты одного отсчёта каждой плоскости (их длина — байт на
/// отсчёт).
fn build_host_frame(
    layout: VideoFramePixelLayout,
    [width, height]: [u32; 2],
    row_tail_samples: usize,
    sample_bytes: [Vec<u8>; 3],
) -> TestHostFrame {
    let (chroma_width, chroma_height) = match layout {
        VideoFramePixelLayout::Yuv420Planar8
        | VideoFramePixelLayout::Yuv420Planar10Le
        | VideoFramePixelLayout::Yuv420Planar12Le => (width.div_ceil(2), height.div_ceil(2)),
        VideoFramePixelLayout::Yuv422Planar8
        | VideoFramePixelLayout::Yuv422Planar10Le
        | VideoFramePixelLayout::Yuv422Planar12Le => (width.div_ceil(2), height),
        _ => (width, height),
    };
    let bytes_per_sample = sample_bytes[0].len();
    let mut bytes = Vec::new();
    let mut planes = Vec::new();
    for (role, (plane_width, plane_height), sample) in [
        (HostPlaneRole::Luma, (width, height), &sample_bytes[0]),
        (
            HostPlaneRole::ChromaU,
            (chroma_width, chroma_height),
            &sample_bytes[1],
        ),
        (
            HostPlaneRole::ChromaV,
            (chroma_width, chroma_height),
            &sample_bytes[2],
        ),
    ] {
        let row_bytes = plane_width as usize * bytes_per_sample;
        let tail_bytes = row_tail_samples * bytes_per_sample;
        let offset = bytes.len();
        for row in 0..plane_height {
            for _ in 0..plane_width {
                bytes.extend_from_slice(sample);
            }
            if row + 1 < plane_height {
                bytes.extend(std::iter::repeat_n(0xEE, tail_bytes));
            }
        }
        planes.push(HostPlaneDescriptor {
            role,
            offset,
            stride: row_bytes + tail_bytes,
            visible_width: plane_width,
            visible_height: plane_height,
            bytes_per_sample,
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
