//! Жизненный цикл блоков под dav1d: перемотки и закрытие без утечек и use-after-free.
//!
//! dav1d держит свои ссылки на картинки (опорные кадры, кадры в работе потоков) и
//! отпускает их через `release_picture_callback` патча — в том числе из своих потоков.
//! Блок обязан вернуться в пул ровно тогда, когда его отпустили и dav1d, и все кадры
//! приложения.

use super::{
    FIXTURE_FRAME_COUNT, decode_all, fixture_bytes, ivf_packets, open_decoder, visible_planes,
};
use crate::codec_adapter::FfmpegDecoderId;
use crate::ffi::codec_context::{CodecContext, SoftwareFrameMemory};
use crate::ffi::error::FfmpegErrorKind;
use crate::ffi::frame::OwnedAvFrame;
use crate::ffi::packet::OwnedAvPacket;
use video_core::{HostFrameMemoryKind, HostImportableMemoryBlock};

/// Сколько перемоток делает «шторм».
const SEEK_ROUNDS: usize = 24;
/// Ключевые кадры фикстуры: `-g 6` в `generate.sh`.
const KEYFRAME_INTERVAL: usize = 6;

/// Отправляет пакеты и забирает все готовые кадры (ссылки на них остаются у вызывающего).
fn decode_packets_keeping_frames(
    context: &mut CodecContext,
    payloads: &[Vec<u8>],
    kept_frames: &mut Vec<OwnedAvFrame>,
) {
    let mut frame = OwnedAvFrame::allocate_for_decode().unwrap_or_else(|error| panic!("{error}"));
    for payload in payloads {
        let packet = OwnedAvPacket::new(payload).unwrap_or_else(|error| panic!("{error}"));
        context
            .send_packet(&packet)
            .unwrap_or_else(|error| panic!("send_packet: {error}"));
        loop {
            match context.receive_frame(&mut frame) {
                Ok(()) => {
                    kept_frames.push(
                        frame
                            .try_clone_ref()
                            .unwrap_or_else(|error| panic!("{error}")),
                    );
                    frame.unref();
                }
                Err(error) if error.status_kind() == Some(FfmpegErrorKind::Again) => break,
                Err(error) => panic!("receive_frame: {error}"),
            }
        }
    }
}

fn checked_out_blocks(context: &CodecContext) -> usize {
    context
        .frame_block_pool_stats()
        .unwrap_or_else(|| unreachable!("ImportableBlocks context has a pool"))
        .checked_out
}

#[test]
fn av1_seek_storm_returns_every_block_to_the_pool() {
    let packets = ivf_packets(&fixture_bytes("av1-176x100-12frames.ivf"));
    let mut context = open_decoder(
        FfmpegDecoderId::Av1,
        SoftwareFrameMemory::ImportableBlocks {
            memory: HostFrameMemoryKind::SealedMemfd,
        },
    );
    // Кадры «в очереди показа»: часть переживает перемотку, как у настоящего плеера.
    let mut kept_frames = Vec::new();

    for round in 0..SEEK_ROUNDS {
        let start = (round % 2) * KEYFRAME_INTERVAL;
        let length = 1 + round % (KEYFRAME_INTERVAL - 1);
        decode_packets_keeping_frames(
            &mut context,
            &packets[start..start + length],
            &mut kept_frames,
        );
        context.flush_buffers();

        // После flush у dav1d нет ни одной картинки: каждый выданный блок держит ровно один
        // кадр приложения (без зерна выходной кадр и опорная картинка — один и тот же блок).
        assert_eq!(
            checked_out_blocks(&context),
            kept_frames.len(),
            "round {round}: blocks still checked out must belong to frames the player holds"
        );
        // Рендер отпускает старые кадры с опозданием — половину за раз.
        let released = kept_frames.len() / 2;
        kept_frames.drain(..released);
    }

    assert!(
        context
            .frame_block_pool_stats()
            .is_some_and(|stats| stats.fallbacks == 0 && stats.reused > 0),
        "AV1 frames went through our reused blocks: {:?}",
        context.frame_block_pool_stats()
    );
    kept_frames.clear();
    assert_eq!(
        checked_out_blocks(&context),
        0,
        "every block returns to the pool once dav1d and the player let go"
    );
}

#[test]
fn av1_frames_keep_their_block_after_decoder_is_closed() {
    let packets = ivf_packets(&fixture_bytes("av1-176x100-12frames.ivf"));
    let mut context = open_decoder(
        FfmpegDecoderId::Av1,
        SoftwareFrameMemory::ImportableBlocks {
            memory: HostFrameMemoryKind::SealedMemfd,
        },
    );
    let mut kept_frames = Vec::new();
    decode_packets_keeping_frames(&mut context, &packets, &mut kept_frames);
    assert!(!kept_frames.is_empty(), "AV1 frames before close");
    let pixels_before_close: Vec<_> = kept_frames.iter().map(visible_planes).collect();

    // Закрытие декодера посреди воспроизведения: dav1d отпускает свои картинки (в том числе
    // из своих потоков), а блоки кадров плеера обязаны остаться живыми.
    drop(context);

    for (index, frame) in kept_frames.iter().enumerate() {
        let block = frame
            .importable_block()
            .unwrap_or_else(|| panic!("frame {index} lost its block"));
        assert!(block.sealed_memfd().is_some(), "frame {index}: memfd block");
        assert_eq!(visible_planes(frame), pixels_before_close[index]);
    }
}

#[test]
fn av1_reference_pictures_return_to_the_pool_on_flush_after_eof() {
    // Все 12 кадров выходят через наши блоки; опорные картинки dav1d — тоже блоки пула.
    let packets = ivf_packets(&fixture_bytes("av1-176x100-12frames.ivf"));
    let mut context = open_decoder(
        FfmpegDecoderId::Av1,
        SoftwareFrameMemory::ImportableBlocks {
            memory: HostFrameMemoryKind::SealedMemfd,
        },
    );

    let decoded = decode_all(&mut context, &packets);

    assert_eq!(decoded.len(), FIXTURE_FRAME_COUNT);
    assert!(
        decoded
            .iter()
            .all(|(_, memory)| memory.has_importable_block)
    );
    // После EOF dav1d ещё держит опорные картинки (кадры приложения уже отпущены):
    // блоки возвращаются при flush (перемотка) или закрытии, а не «висят» навсегда.
    assert!(
        checked_out_blocks(&context) > 0,
        "dav1d keeps its reference pictures after EOF"
    );
    context.flush_buffers();
    assert_eq!(
        checked_out_blocks(&context),
        0,
        "flush releases every reference picture back to the pool"
    );
}
