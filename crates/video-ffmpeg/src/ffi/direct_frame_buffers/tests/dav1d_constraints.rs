//! Проверка контракта dav1d внутри патча FFmpeg (fail-closed).
//!
//! Патч `ffmpeg-vendor` зовёт `get_buffer2()` приложения из выделителя картинок dav1d
//! и обязан проверить ответ: адреса plane-ов кратны 64 (`DAV1D_PICTURE_ALIGNMENT`).
//! Здесь приложение нарочно отдаёт память со сдвигом на 16 байт. Ожидание: dav1d её
//! не получает (иначе SIMD-код читал бы мимо выравнивания), картинка берётся из
//! внутреннего пула обёртки, а кадры декодируются те же, что без колбэка.
//!
//! Тест работает на сыром FFmpeg API: `CodecContext` ставит только наш настоящий
//! колбэк, а нужен заведомо «плохой».

use std::ffi::CString;
use std::os::raw::c_int;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{
    FIXTURE_FRAME_COUNT, VisibleFramePlanes, decode_all, fixture_bytes, ivf_packets, open_decoder,
    visible_planes,
};
use crate::codec_adapter::FfmpegDecoderId;
use crate::ffi::codec_context::SoftwareFrameMemory;
use crate::ffi::error::FfmpegErrorKind;
use crate::ffi::frame::OwnedAvFrame;
use crate::ffi::packet::OwnedAvPacket;

/// Выравнивание, которое dav1d требует от адресов plane-ов (`DAV1D_PICTURE_ALIGNMENT`).
const DAV1D_PICTURE_ALIGNMENT: usize = 64;
/// Сдвиг, ломающий это выравнивание у памяти «плохого» приложения.
const MISALIGNMENT_BYTES: usize = 16;

/// Сколько раз обёртка спросила память у «плохого» приложения.
static MISALIGNED_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

/// `get_buffer2`, который выделяет как FFmpeg, но сдвигает plane-ы на 16 байт.
///
/// # Safety
/// Зовётся только FFmpeg с валидными `context`/`frame` по контракту `get_buffer2`.
unsafe extern "C" fn misaligned_get_buffer2(
    context: *mut ffmpeg_sys_next::AVCodecContext,
    frame: *mut ffmpeg_sys_next::AVFrame,
    flags: c_int,
) -> c_int {
    MISALIGNED_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    // SAFETY: аргументы пришли от FFmpeg по контракту get_buffer2.
    let status = unsafe { ffmpeg_sys_next::avcodec_default_get_buffer2(context, frame, flags) };
    if status < 0 {
        return status;
    }
    for plane_index in 0..3 {
        // SAFETY: frame валиден; сдвиг остаётся внутри буфера plane-ы (у буферов FFmpeg
        // есть хвостовой запас), а сами байты по сдвинутому адресу никто не читает:
        // обёртка обязана отвергнуть такой кадр до передачи dav1d.
        unsafe {
            let plane = (*frame).data[plane_index];
            if !plane.is_null() {
                (*frame).data[plane_index] = plane.add(MISALIGNMENT_BYTES);
            }
        }
    }
    status
}

/// Кадры, декодированные libdav1d с «плохим» `get_buffer2`, и адреса их Y-plane-ов.
fn decode_with_misaligned_caller_buffers(packets: &[Vec<u8>]) -> Vec<(VisibleFramePlanes, usize)> {
    let decoder_name = CString::new("libdav1d").unwrap_or_else(|_| unreachable!("no NUL"));
    // SAFETY: последовательность сырого API FFmpeg: найти декодер, создать context,
    // поставить колбэк до open, затем send/receive и освобождение context-а в конце.
    unsafe {
        let decoder = ffmpeg_sys_next::avcodec_find_decoder_by_name(decoder_name.as_ptr());
        assert!(!decoder.is_null(), "vendored FFmpeg has libdav1d");
        let mut context = ffmpeg_sys_next::avcodec_alloc_context3(decoder);
        assert!(!context.is_null());
        (*context).get_buffer2 = Some(misaligned_get_buffer2);
        (*context).thread_count = 2;
        let open_status = ffmpeg_sys_next::avcodec_open2(context, decoder, std::ptr::null_mut());
        assert_eq!(open_status, 0, "avcodec_open2(libdav1d)");

        let mut frame =
            OwnedAvFrame::allocate_for_decode().unwrap_or_else(|error| panic!("{error}"));
        let mut decoded = Vec::new();
        let mut receive_ready = |decoded: &mut Vec<_>| loop {
            let status = ffmpeg_sys_next::avcodec_receive_frame(context, frame.as_mut_ptr());
            if status < 0 {
                let kind = crate::ffi::error::FfmpegError::from_averror("receive", status);
                assert!(
                    matches!(
                        kind.status_kind(),
                        Some(FfmpegErrorKind::Again | FfmpegErrorKind::EndOfFile)
                    ),
                    "receive_frame: {kind}"
                );
                return;
            }
            let luma_address = frame.plane_data_address(0).unwrap_or(0);
            decoded.push((visible_planes(&frame), luma_address));
            frame.unref();
        };
        for payload in packets {
            let packet = OwnedAvPacket::new(payload).unwrap_or_else(|error| panic!("{error}"));
            let status = ffmpeg_sys_next::avcodec_send_packet(context, packet.as_ptr());
            assert_eq!(status, 0, "send_packet");
            receive_ready(&mut decoded);
        }
        assert_eq!(
            ffmpeg_sys_next::avcodec_send_packet(context, std::ptr::null()),
            0,
            "flush packet"
        );
        receive_ready(&mut decoded);
        ffmpeg_sys_next::avcodec_free_context(&mut context);
        decoded
    }
}

#[test]
fn dav1d_rejects_misaligned_caller_buffers_and_falls_back_to_its_pool() {
    let packets = ivf_packets(&fixture_bytes("av1-176x100-12frames.ivf"));
    let mut reference_context =
        open_decoder(FfmpegDecoderId::Av1, SoftwareFrameMemory::DecoderDefault);
    let reference_frames = decode_all(&mut reference_context, &packets);

    let decoded = decode_with_misaligned_caller_buffers(&packets);

    // Обёртка действительно спрашивала приложение (DR1 работает)…
    assert!(
        MISALIGNED_ALLOCATIONS.load(Ordering::Relaxed) > 0,
        "patched libdav1d must call the caller's get_buffer2"
    );
    // …но ни один кадр не лежит в сдвинутой памяти: всё из выровненного пула обёртки.
    assert_eq!(decoded.len(), FIXTURE_FRAME_COUNT);
    for (index, ((planes, luma_address), (reference_planes, _))) in
        decoded.iter().zip(&reference_frames).enumerate()
    {
        assert_eq!(
            luma_address % DAV1D_PICTURE_ALIGNMENT,
            0,
            "frame {index} must not use misaligned caller memory"
        );
        assert!(planes == reference_planes, "frame {index} pixels differ");
    }
}
