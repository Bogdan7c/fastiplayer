//! Safe packet input helpers and RAII ownership for `AVPacket`.

#[cfg(feature = "ffmpeg")]
use std::ptr::NonNull;
#[cfg(feature = "ffmpeg")]
use std::slice;

use super::error::{FfiResult, FfmpegError};

/// FFmpeg требует zero padding после compressed input buffer-а.
#[cfg(not(feature = "ffmpeg"))]
pub const INPUT_BUFFER_PADDING_BYTES: usize = 64;

/// FFmpeg требует zero padding после compressed input buffer-а.
#[cfg(feature = "ffmpeg")]
pub const INPUT_BUFFER_PADDING_BYTES: usize =
    ffmpeg_sys_next::AV_INPUT_BUFFER_PADDING_SIZE as usize;

/// Encoded payload вместе с padding, который безопасен для FFmpeg bitstream readers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaddedPacketBytes {
    /// Payload плюс trailing zero padding.
    padded_bytes: Vec<u8>,

    /// Длина настоящего compressed payload-а без padding.
    payload_len: usize,
}

impl PaddedPacketBytes {
    /// Копирует compressed payload и добавляет FFmpeg-required zero padding.
    #[must_use]
    pub fn new(encoded_payload: impl AsRef<[u8]>) -> Self {
        let encoded_payload = encoded_payload.as_ref();
        let payload_len = encoded_payload.len();
        let mut padded_bytes = Vec::with_capacity(payload_len + INPUT_BUFFER_PADDING_BYTES);

        padded_bytes.extend_from_slice(encoded_payload);
        padded_bytes.resize(payload_len + INPUT_BUFFER_PADDING_BYTES, 0);

        Self {
            padded_bytes,
            payload_len,
        }
    }

    /// Возвращает compressed payload без trailing padding.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.padded_bytes[..self.payload_len]
    }

    /// Возвращает payload плюс padding для future packet construction.
    #[must_use]
    pub fn padded_bytes(&self) -> &[u8] {
        &self.padded_bytes
    }

    /// Возвращает длину compressed payload-а без padding.
    #[must_use]
    pub const fn payload_len(&self) -> usize {
        self.payload_len
    }
}

/// RAII owner для caller-owned `AVPacket`.
#[derive(Debug)]
pub struct OwnedAvPacket {
    /// Raw packet живёт только внутри FFI boundary.
    #[cfg(feature = "ffmpeg")]
    raw_packet: NonNull<ffmpeg_sys_next::AVPacket>,

    /// Marker, чтобы type существовал в default build-е без FFmpeg headers/libs.
    #[cfg(not(feature = "ffmpeg"))]
    _feature_disabled: (),
}

/// Timestamp metadata, которую safe layer записывает в `AVPacket`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PacketTimestamps {
    /// Presentation timestamp в FFmpeg stream time base units.
    pub pts: Option<i64>,

    /// Decode timestamp в FFmpeg stream time base units.
    pub dts: Option<i64>,

    /// Packet duration в FFmpeg stream time base units.
    pub duration: Option<i64>,
}

impl OwnedAvPacket {
    /// Allocates an `AVPacket`, copies caller payload and keeps FFmpeg padding.
    pub fn new(encoded_payload: impl AsRef<[u8]>) -> FfiResult<Self> {
        let encoded_payload = encoded_payload.as_ref();

        #[cfg(not(feature = "ffmpeg"))]
        {
            let _encoded_payload = encoded_payload;
            Err(FfmpegError::FeatureDisabled)
        }

        #[cfg(feature = "ffmpeg")]
        {
            Self::allocate_with_payload(encoded_payload)
        }
    }

    /// Возвращает compressed payload без trailing padding.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        #[cfg(not(feature = "ffmpeg"))]
        {
            &[]
        }

        #[cfg(feature = "ffmpeg")]
        {
            let payload_len = self.payload_len();
            let packet_data = self.packet_data_ptr();

            if packet_data.is_null() || payload_len == 0 {
                return &[];
            }

            // SAFETY: `OwnedAvPacket` владеет `AVPacket` и его refcounted
            // buffer-ом. `av_new_packet` выделил минимум `payload_len` bytes
            // payload-а, а `&self` запрещает concurrent unref/free.
            unsafe { slice::from_raw_parts(packet_data, payload_len) }
        }
    }

    /// Возвращает trailing zero padding, который FFmpeg bitstream readers могут читать.
    #[must_use]
    pub fn padding(&self) -> &[u8] {
        #[cfg(not(feature = "ffmpeg"))]
        {
            &[]
        }

        #[cfg(feature = "ffmpeg")]
        {
            let payload_len = self.payload_len();
            let packet_data = self.packet_data_ptr();

            if packet_data.is_null() {
                return &[];
            }

            // SAFETY: `av_new_packet` выделяет payload + `AV_INPUT_BUFFER_PADDING_SIZE`
            // bytes. Packet остаётся владельцем buffer-а, `&self` не даёт вызвать
            // `unref`/drop параллельно с чтением returned slice.
            unsafe {
                slice::from_raw_parts(packet_data.add(payload_len), INPUT_BUFFER_PADDING_BYTES)
            }
        }
    }

    /// Возвращает длину compressed payload-а без padding.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        #[cfg(not(feature = "ffmpeg"))]
        {
            0
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: raw pointer создан `av_packet_alloc`, хранится NonNull и
            // освобождается только в Drop. Читаем immutable field.
            let packet_size = unsafe { self.raw_packet.as_ref().size };

            packet_size.max(0) as usize
        }
    }

    /// Возвращает размер input buffer-а, который безопасен для FFmpeg readers.
    #[must_use]
    pub fn padded_input_buffer_len(&self) -> usize {
        self.payload_len() + INPUT_BUFFER_PADDING_BYTES
    }

    /// Сбрасывает packet data и side data без освобождения самого `AVPacket`.
    pub fn unref(&mut self) {
        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: wrapper единолично владеет packet pointer-ом. `&mut self`
            // гарантирует, что active borrowed slices через safe API уже закончились.
            unsafe { ffmpeg_sys_next::av_packet_unref(self.raw_packet.as_ptr()) };
        }
    }

    /// Записывает timestamp поля без раскрытия `AVPacket` наружу.
    pub fn set_timestamps(&mut self, timestamps: PacketTimestamps) {
        #[cfg(not(feature = "ffmpeg"))]
        {
            let _timestamps = timestamps;
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: wrapper единолично владеет packet pointer-ом. Поля
            // `pts`/`dts`/`duration` являются plain metadata FFmpeg packet-а.
            let packet = unsafe { self.raw_packet.as_mut() };
            packet.pts = timestamps.pts.unwrap_or(ffmpeg_sys_next::AV_NOPTS_VALUE);
            packet.dts = timestamps.dts.unwrap_or(ffmpeg_sys_next::AV_NOPTS_VALUE);
            packet.duration = timestamps.duration.unwrap_or(0);
        }
    }

    /// Помечает packet как keyframe, если container уже сообщил этот факт.
    pub fn set_keyframe(&mut self, keyframe: bool) {
        #[cfg(not(feature = "ffmpeg"))]
        {
            let _keyframe = keyframe;
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: wrapper единолично владеет packet pointer-ом. FFmpeg
            // flags являются plain bitfield metadata.
            let packet = unsafe { self.raw_packet.as_mut() };

            if keyframe {
                packet.flags |= ffmpeg_sys_next::AV_PKT_FLAG_KEY;
            } else {
                packet.flags &= !ffmpeg_sys_next::AV_PKT_FLAG_KEY;
            }
        }
    }

    #[cfg(feature = "ffmpeg")]
    fn allocate_with_payload(encoded_payload: &[u8]) -> FfiResult<Self> {
        let payload_len = encoded_payload.len();
        let packet_size =
            i32::try_from(payload_len).map_err(|_| FfmpegError::PacketTooLarge { payload_len })?;

        // SAFETY: FFmpeg allocator возвращает owned `AVPacket*` или null.
        // Pointer сразу заворачивается в `NonNull` и освобождается через
        // `av_packet_free` на всех exit path-ах.
        let raw_packet = unsafe { ffmpeg_sys_next::av_packet_alloc() };
        let raw_packet = NonNull::new(raw_packet).ok_or(FfmpegError::AllocationFailed {
            operation: "av_packet_alloc",
        })?;
        let packet_owner = Self { raw_packet };

        // SAFETY: packet pointer valid и принадлежит wrapper-у. FFmpeg выделяет
        // payload buffer размером `packet_size + AV_INPUT_BUFFER_PADDING_SIZE`
        // и zeroes padding по контракту `av_new_packet`.
        let status = unsafe {
            ffmpeg_sys_next::av_new_packet(packet_owner.raw_packet.as_ptr(), packet_size)
        };
        if status < 0 {
            return Err(FfmpegError::from_averror("av_new_packet", status));
        }

        if !encoded_payload.is_empty() {
            // SAFETY: после успешного `av_new_packet` `data` указывает минимум на
            // `encoded_payload.len()` writable bytes. Source slice не пересекается
            // с FFmpeg-owned destination buffer.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    encoded_payload.as_ptr(),
                    (*packet_owner.raw_packet.as_ptr()).data,
                    encoded_payload.len(),
                );
            }
        }

        Ok(packet_owner)
    }

    #[cfg(feature = "ffmpeg")]
    fn packet_data_ptr(&self) -> *const u8 {
        // SAFETY: raw pointer создан `av_packet_alloc`, хранится NonNull и
        // освобождается только в Drop. Читаем immutable field.
        unsafe { self.raw_packet.as_ref().data.cast_const() }
    }

    #[cfg(feature = "ffmpeg")]
    pub(crate) fn as_ptr(&self) -> *const ffmpeg_sys_next::AVPacket {
        self.raw_packet.as_ptr().cast_const()
    }
}

#[cfg(feature = "ffmpeg")]
impl Drop for OwnedAvPacket {
    fn drop(&mut self) {
        free_packet(self.raw_packet);
    }
}

#[cfg(feature = "ffmpeg")]
fn free_packet(raw_packet: NonNull<ffmpeg_sys_next::AVPacket>) {
    let mut packet_to_free = raw_packet.as_ptr();

    // SAFETY: pointer получен из `av_packet_alloc` и ещё не освобождён.
    // FFmpeg unrefs inner buffer, frees packet struct and writes null into
    // local variable; наружу этот local pointer не отдаётся.
    unsafe { ffmpeg_sys_next::av_packet_free(&mut packet_to_free) };
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "ffmpeg")]
    use static_assertions::assert_not_impl_any;

    // AVPacket создаётся, отправляется и освобождается на decoder owner thread.
    #[cfg(feature = "ffmpeg")]
    assert_not_impl_any!(OwnedAvPacket: Send, Sync);

    #[test]
    fn padded_packet_keeps_payload_and_zero_padding_separate() {
        let packet_bytes = PaddedPacketBytes::new([1_u8, 2, 3]);

        assert_eq!(packet_bytes.payload(), &[1, 2, 3]);
        assert_eq!(packet_bytes.payload_len(), 3);
        assert_eq!(
            packet_bytes.padded_bytes().len(),
            3 + INPUT_BUFFER_PADDING_BYTES
        );
        assert!(
            packet_bytes.padded_bytes()[3..]
                .iter()
                .all(|padding_byte| *padding_byte == 0)
        );
    }

    #[test]
    fn owned_packet_reports_feature_disabled_without_ffmpeg() {
        if cfg!(feature = "ffmpeg") {
            return;
        }

        let error = OwnedAvPacket::new([1_u8, 2, 3]).expect_err("default build has no FFmpeg FFI");

        assert_eq!(error, FfmpegError::FeatureDisabled);
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn owned_packet_allocates_zero_padding_for_ffmpeg_readers() {
        let packet = OwnedAvPacket::new([1_u8, 2, 3]).expect("packet allocation should succeed");

        assert_eq!(packet.payload(), &[1, 2, 3]);
        assert_eq!(packet.payload_len(), 3);
        assert_eq!(
            packet.padded_input_buffer_len(),
            3 + INPUT_BUFFER_PADDING_BYTES
        );
        assert_eq!(packet.padding().len(), INPUT_BUFFER_PADDING_BYTES);
        assert!(
            packet
                .padding()
                .iter()
                .all(|padding_byte| *padding_byte == 0)
        );
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn owned_packet_unref_releases_payload_without_freeing_packet_owner() {
        let mut packet =
            OwnedAvPacket::new([1_u8, 2, 3]).expect("packet allocation should succeed");

        packet.unref();

        assert_eq!(packet.payload_len(), 0);
        assert!(packet.payload().is_empty());
    }
}
