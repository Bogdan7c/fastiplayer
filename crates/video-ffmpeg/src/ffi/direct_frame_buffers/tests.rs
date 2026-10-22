//! Тесты своего `get_buffer2` на настоящем FFmpeg (cpu-parity 08b).
//!
//! Главное доказательство — побайтное равенство: один и тот же поток декодируется
//! обычным выделением FFmpeg и в наши блоки, видимые plane-ы каждого кадра должны
//! совпасть. Плюс проверка того, ради чего всё делается: кадр лежит в одном блоке,
//! выровненном по странице, со stride, кратным 256, а блоки переиспользуются.

use std::num::NonZeroUsize;
use std::path::PathBuf;

use video_core::{
    HOST_IMPORTABLE_BLOCK_ALIGNMENT, HOST_IMPORTABLE_ROW_PITCH_ALIGNMENT,
    HostImportableMemoryBlock, SoftwareDecodeThreadBudget,
};

use super::*;
use crate::codec_adapter::FfmpegDecoderId;
use crate::ffi::codec_context::{CodecContext, FfmpegCodecContextRequest, SoftwareFrameMemory};
use crate::ffi::error::FfmpegErrorKind;
use crate::ffi::frame::OwnedAvFrame;
use crate::ffi::packet::OwnedAvPacket;
use crate::ffi::pixel_format::SoftwarePixelFormatSet;

/// Размер кадра фикстур (`tests/fixtures/direct-frame-buffers/generate.sh`).
const FIXTURE_WIDTH: usize = 176;
const FIXTURE_HEIGHT: usize = 100;
/// Кадров в каждой фикстуре.
const FIXTURE_FRAME_COUNT: usize = 12;

/// Видимые plane-ы одного кадра 4:2:0 8-bit, построчно без padding-а.
#[derive(Debug, PartialEq, Eq)]
struct VisibleFramePlanes {
    planes: [Vec<u8>; 3],
}

/// Что видно о раскладке кадра в памяти.
#[derive(Debug)]
struct FrameMemoryEvidence {
    /// Кадр лежит в блоке нашего пула.
    has_importable_block: bool,
    /// Все plane-ы внутри блока, начало блока выровнено по странице.
    planes_inside_aligned_block: bool,
    /// Stride всех plane-ов кратен 256.
    strides_fit_gpu_row_pitch: bool,
}

fn fixture_bytes(file_name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/direct-frame-buffers")
        .join(file_name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("read fixture {}: {error}", path.display()))
}

/// Режет IVF (VP9) на пакеты-кадры.
fn ivf_packets(file_bytes: &[u8]) -> Vec<Vec<u8>> {
    assert_eq!(&file_bytes[0..4], b"DKIF", "IVF signature");
    let mut packets = Vec::new();
    let mut offset = 32;
    while offset + 12 <= file_bytes.len() {
        let frame_len = u32::from_le_bytes(
            file_bytes[offset..offset + 4]
                .try_into()
                .unwrap_or_else(|_| unreachable!("4-byte slice")),
        ) as usize;
        let payload_start = offset + 12;
        packets.push(file_bytes[payload_start..payload_start + frame_len].to_vec());
        offset = payload_start + frame_len;
    }
    packets
}

/// Режет Annex B на access unit-ы по AUD (`generate.sh` вставляет AUD перед каждым).
fn annex_b_access_units(
    file_bytes: &[u8],
    is_access_unit_delimiter: fn(u8, u8) -> bool,
) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 5 < file_bytes.len() {
        let four_byte_start = file_bytes[index..index + 4] == [0, 0, 0, 1];
        if four_byte_start && is_access_unit_delimiter(file_bytes[index + 4], file_bytes[index + 5])
        {
            starts.push(index);
        }
        index += 1;
    }
    starts
        .iter()
        .enumerate()
        .map(|(position, &start)| {
            let end = starts
                .get(position + 1)
                .copied()
                .unwrap_or(file_bytes.len());
            file_bytes[start..end].to_vec()
        })
        .collect()
}

/// H.264 AUD: NAL type 9 в младших 5 битах первого байта заголовка.
fn is_h264_aud(first_header_byte: u8, _second_header_byte: u8) -> bool {
    first_header_byte & 0x1f == 9
}

/// HEVC AUD: NAL type 35 в битах 1..6 первого байта заголовка.
fn is_hevc_aud(first_header_byte: u8, _second_header_byte: u8) -> bool {
    (first_header_byte >> 1) & 0x3f == 35
}

fn open_decoder(decoder_id: FfmpegDecoderId, frame_memory: SoftwareFrameMemory) -> CodecContext {
    let request = FfmpegCodecContextRequest::for_decoder_id(
        decoder_id,
        SoftwarePixelFormatSet::v1_host_planar(),
    )
    // Два потока: frame threading декодера выделяет кадры из своих потоков.
    .with_software_decode_thread_budget(SoftwareDecodeThreadBudget::fixed(
        NonZeroUsize::new(2).unwrap_or_else(|| unreachable!("2 > 0")),
    ))
    .with_frame_memory(frame_memory);
    CodecContext::open(&request).unwrap_or_else(|error| panic!("open {decoder_id:?}: {error}"))
}

fn visible_planes(frame: &OwnedAvFrame) -> VisibleFramePlanes {
    let plane_sizes = [
        (FIXTURE_WIDTH, FIXTURE_HEIGHT),
        (FIXTURE_WIDTH.div_ceil(2), FIXTURE_HEIGHT.div_ceil(2)),
        (FIXTURE_WIDTH.div_ceil(2), FIXTURE_HEIGHT.div_ceil(2)),
    ];
    let planes = plane_sizes.map(|_| Vec::new());
    let mut planes = planes;
    for (plane_index, (width, height)) in plane_sizes.into_iter().enumerate() {
        for row in 0..height {
            let row_bytes = frame
                .plane_row_data(plane_index, row, width)
                .unwrap_or_else(|error| panic!("plane {plane_index} row {row}: {error}"))
                .unwrap_or_else(|| panic!("plane {plane_index} has no data"));
            planes[plane_index].extend_from_slice(row_bytes);
        }
    }
    VisibleFramePlanes { planes }
}

fn memory_evidence(frame: &OwnedAvFrame) -> FrameMemoryEvidence {
    let strides_fit_gpu_row_pitch = (0..3).all(|plane_index| {
        frame
            .linesize(plane_index)
            .and_then(|linesize| usize::try_from(linesize).ok())
            .is_some_and(|linesize| linesize.is_multiple_of(HOST_IMPORTABLE_ROW_PITCH_ALIGNMENT))
    });
    let Some(block) = frame.importable_block() else {
        return FrameMemoryEvidence {
            has_importable_block: false,
            planes_inside_aligned_block: false,
            strides_fit_gpu_row_pitch,
        };
    };
    let region = block.region();
    let block_start = region.address().as_ptr() as usize;
    let block_end = block_start + region.len();
    let planes_inside_aligned_block = block_start.is_multiple_of(HOST_IMPORTABLE_BLOCK_ALIGNMENT)
        && (0..3).all(|plane_index| {
            frame
                .plane_data_address(plane_index)
                .is_some_and(|address| address >= block_start && address < block_end)
        });
    FrameMemoryEvidence {
        has_importable_block: true,
        planes_inside_aligned_block,
        strides_fit_gpu_row_pitch,
    }
}

/// Декодирует все пакеты до EOF, собирая видимые plane-ы и раскладку памяти кадров.
fn decode_all(
    context: &mut CodecContext,
    packets: &[Vec<u8>],
) -> Vec<(VisibleFramePlanes, FrameMemoryEvidence)> {
    let mut frame = OwnedAvFrame::allocate_for_decode().unwrap_or_else(|error| panic!("{error}"));
    let mut decoded = Vec::new();
    let mut receive_ready = |context: &mut CodecContext, decoded: &mut Vec<_>| loop {
        match context.receive_frame(&mut frame) {
            Ok(()) => {
                decoded.push((visible_planes(&frame), memory_evidence(&frame)));
                frame.unref();
            }
            Err(error)
                if matches!(
                    error.status_kind(),
                    Some(FfmpegErrorKind::Again | FfmpegErrorKind::EndOfFile)
                ) =>
            {
                return;
            }
            Err(error) => panic!("receive_frame: {error}"),
        }
    };
    for payload in packets {
        let packet = OwnedAvPacket::new(payload).unwrap_or_else(|error| panic!("{error}"));
        loop {
            match context.send_packet(&packet) {
                Ok(()) => break,
                Err(error) if error.status_kind() == Some(FfmpegErrorKind::Again) => {
                    receive_ready(context, &mut decoded);
                }
                Err(error) => panic!("send_packet: {error}"),
            }
        }
        receive_ready(context, &mut decoded);
    }
    context
        .send_flush_packet()
        .unwrap_or_else(|error| panic!("send_flush_packet: {error}"));
    receive_ready(context, &mut decoded);
    decoded
}

/// Сравнивает декод в наши блоки с обычным выделением FFmpeg на одном потоке.
fn assert_direct_buffers_match_default(decoder_id: FfmpegDecoderId, packets: &[Vec<u8>]) {
    let mut default_context = open_decoder(decoder_id, SoftwareFrameMemory::DecoderDefault);
    let default_frames = decode_all(&mut default_context, packets);
    let mut direct_context = open_decoder(decoder_id, SoftwareFrameMemory::ImportableBlocks);
    let direct_frames = decode_all(&mut direct_context, packets);

    assert_eq!(
        default_frames.len(),
        FIXTURE_FRAME_COUNT,
        "{decoder_id:?} default frame count"
    );
    assert_eq!(
        direct_frames.len(),
        FIXTURE_FRAME_COUNT,
        "{decoder_id:?} direct frame count"
    );
    for (index, ((default_planes, default_memory), (direct_planes, direct_memory))) in
        default_frames.iter().zip(&direct_frames).enumerate()
    {
        assert!(
            default_planes == direct_planes,
            "{decoder_id:?} frame {index}: pixels decoded into our blocks differ from FFmpeg default"
        );
        assert!(
            !default_memory.has_importable_block,
            "default frame {index} must not claim a block"
        );
        assert!(
            direct_memory.has_importable_block,
            "{decoder_id:?} frame {index} has no block"
        );
        assert!(
            direct_memory.planes_inside_aligned_block,
            "{decoder_id:?} frame {index}: {direct_memory:?}"
        );
        assert!(
            direct_memory.strides_fit_gpu_row_pitch,
            "{decoder_id:?} frame {index}: {direct_memory:?}"
        );
    }

    let stats = direct_context
        .frame_block_pool_stats()
        .unwrap_or_else(|| unreachable!("ImportableBlocks context has a pool"));
    assert_eq!(
        stats.fallbacks, 0,
        "{decoder_id:?}: no frame may fall back to FFmpeg allocation"
    );
    assert!(
        stats.reused > 0,
        "{decoder_id:?}: blocks must be reused, stats {stats:?}"
    );
    assert!(
        (stats.allocated as usize) < FIXTURE_FRAME_COUNT,
        "{decoder_id:?}: pool must not allocate a block per frame, stats {stats:?}"
    );
}

#[test]
fn vp9_frames_decoded_into_importable_blocks_match_ffmpeg_default() {
    let packets = ivf_packets(&fixture_bytes("vp9-176x100-12frames.ivf"));

    assert_direct_buffers_match_default(FfmpegDecoderId::Vp9, &packets);
}

#[test]
fn h264_frames_decoded_into_importable_blocks_match_ffmpeg_default() {
    let packets = annex_b_access_units(&fixture_bytes("h264-176x100-12frames.h264"), is_h264_aud);
    assert_eq!(
        packets.len(),
        FIXTURE_FRAME_COUNT,
        "one access unit per frame"
    );

    assert_direct_buffers_match_default(FfmpegDecoderId::H264, &packets);
}

#[test]
fn hevc_frames_decoded_into_importable_blocks_match_ffmpeg_default() {
    let packets = annex_b_access_units(&fixture_bytes("hevc-176x100-12frames.hevc"), is_hevc_aud);
    assert_eq!(
        packets.len(),
        FIXTURE_FRAME_COUNT,
        "one access unit per frame"
    );

    assert_direct_buffers_match_default(FfmpegDecoderId::Hevc, &packets);
}

#[test]
fn frames_keep_their_block_after_decoder_is_closed() {
    let packets = ivf_packets(&fixture_bytes("vp9-176x100-12frames.ivf"));
    let mut context = open_decoder(FfmpegDecoderId::Vp9, SoftwareFrameMemory::ImportableBlocks);
    let mut frame = OwnedAvFrame::allocate_for_decode().unwrap_or_else(|error| panic!("{error}"));
    let packet = OwnedAvPacket::new(&packets[0]).unwrap_or_else(|error| panic!("{error}"));
    context
        .send_packet(&packet)
        .unwrap_or_else(|error| panic!("{error}"));
    context
        .send_flush_packet()
        .unwrap_or_else(|error| panic!("{error}"));
    context
        .receive_frame(&mut frame)
        .unwrap_or_else(|error| panic!("{error}"));
    let pixels_before_close = visible_planes(&frame);
    let kept_frame = frame
        .try_clone_ref()
        .unwrap_or_else(|error| panic!("{error}"));
    frame.unref();

    drop(context);

    // Ссылка на кадр пережила декодер и пул: блок жив, пиксели те же.
    assert!(
        kept_frame.importable_block().is_some(),
        "clone_ref must keep the block"
    );
    assert_eq!(visible_planes(&kept_frame), pixels_before_close);
}

#[test]
fn decoder_without_dr1_keeps_ffmpeg_allocation() {
    // libdav1d (AV1) не объявляет `AV_CODEC_CAP_DR1`: наш get_buffer2 ему не ставится,
    // и запрос ImportableBlocks молча остаётся на выделении FFmpeg (без пула).
    let context = open_decoder(FfmpegDecoderId::Av1, SoftwareFrameMemory::ImportableBlocks);

    assert!(context.frame_block_pool_stats().is_none());
}

#[test]
fn stride_alignment_is_at_least_gpu_row_pitch() {
    let alignment = combined_stride_alignment(&[32, 16, 16, 0, 0, 0, 0, 0]);

    assert_eq!(alignment, [256; 4]);
}

#[test]
fn stride_alignment_keeps_unusual_decoder_requirement() {
    let alignment = combined_stride_alignment(&[96, 0, 0, 0]);

    assert_eq!(alignment[0], 768, "lcm(256, 96)");
    assert_eq!(alignment[1], 256);
}

#[test]
fn linesizes_are_odd_multiples_of_gpu_row_pitch() {
    // 4K 4:2:0: Y 3840 = 15 × 256 уже нечётное; chroma 1920 → 2304 (9 × 256), а не 2048.
    assert_eq!(
        cache_friendly_linesizes(&[3840, 1920, 1920, 0], &[256; 4]),
        Some([3840, 2304, 2304, 0])
    );
    // 1080p: 1920 → 2304, 960 → 1280 (5 × 256); степени двойки 2048 / 1024 обходятся.
    assert_eq!(
        cache_friendly_linesizes(&[1920, 960, 960, 0], &[256; 4]),
        Some([2304, 1280, 1280, 0])
    );
    // Требование декодера 768 = lcm(256, 96): 1000 → 2 × 768 чётно → 3 × 768.
    assert_eq!(
        cache_friendly_linesizes(&[1000, 0, 0, 0], &[768, 256, 256, 256]),
        Some([2304, 0, 0, 0])
    );
}

#[test]
fn negative_linesize_is_rejected() {
    assert_eq!(
        cache_friendly_linesizes(&[-3840, 1920, 1920, 0], &[256; 4]),
        None
    );
}
