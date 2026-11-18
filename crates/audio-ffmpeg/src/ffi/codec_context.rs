//! Открытый AAC-декодер FFmpeg и цикл «пакет → кадры».
//!
//! Звук декодируется синхронно на потоке вызывающего: один пакет AAC даёт
//! один кадр (у HE-AAC — 2048 отсчётов на выходной частоте). Поток декодера
//! и очередь, как у видео, не нужны.

use std::ptr::{self, NonNull};

use crate::decoded_pcm::DecodedAacPcm;
use crate::error::AudioFfmpegError;
use crate::stream_config::AacStreamConfig;

use super::averror::{AVERROR_EAGAIN, AVERROR_EOF, error_from_averror, packet_error_from_averror};
use super::frame_reader::decoded_pcm_from_frame;
use super::owned::{OwnedCodecContext, OwnedFrame, OwnedPacket};

/// Открытый AAC-декодер FFmpeg вместе с переиспользуемыми пакетом и кадром.
pub(crate) struct AacCodecContext {
    context: OwnedCodecContext,
    packet: OwnedPacket,
    frame: OwnedFrame,
}

// SAFETY: все три структуры FFmpeg принадлежат только этому значению, наружу
// указатели не выходят. FFmpeg разрешает переносить контекст между потоками,
// если им не пользуются одновременно; это гарантирует `&mut self` во всех
// методах. Внутренних потоков FFmpeg нет (`thread_count = 1`), поэтому ничто
// не держит ссылку на поток, где контекст создан. Нужно, потому что
// `AudioDecoder` плеера обязан быть `Send`.
unsafe impl Send for AacCodecContext {}

impl AacCodecContext {
    /// Находит встроенный AAC-декодер и открывает его под описание потока.
    pub(crate) fn open(stream_config: &AacStreamConfig) -> Result<Self, AudioFfmpegError> {
        stream_config.validate()?;
        // SAFETY: поиск в реестре по известному идентификатору, без побочных эффектов.
        let raw_codec = unsafe {
            ffmpeg_sys_next::avcodec_find_decoder(ffmpeg_sys_next::AVCodecID::AV_CODEC_ID_AAC)
        };
        let codec = NonNull::new(raw_codec.cast_mut()).ok_or(AudioFfmpegError::DecoderNotFound)?;

        let decoder = Self {
            context: OwnedCodecContext::allocate(codec)?,
            packet: OwnedPacket::allocate()?,
            frame: OwnedFrame::allocate()?,
        };
        decoder.apply_stream_config(stream_config)?;

        // SAFETY: контекст выделен под этот же декодер и ещё не открыт;
        // словарь опций не передаём.
        let status = unsafe {
            ffmpeg_sys_next::avcodec_open2(
                decoder.context.as_ptr(),
                codec.as_ptr(),
                ptr::null_mut(),
            )
        };
        if status < 0 {
            return Err(error_from_averror("avcodec_open2", status));
        }
        Ok(decoder)
    }

    /// Передаёт FFmpeg то, что известно о потоке до первого пакета.
    fn apply_stream_config(&self, stream_config: &AacStreamConfig) -> Result<(), AudioFfmpegError> {
        let raw_context = self.context.as_ptr();
        // SAFETY: контекст валиден и ещё не открыт — до `avcodec_open2` поля пишет вызывающий.
        unsafe { (*raw_context).thread_count = 1 };
        match stream_config {
            AacStreamConfig::AudioSpecificConfig(config_bytes) => {
                self.assign_extradata(config_bytes)
            }
            AacStreamConfig::BareFrames {
                core_sample_rate,
                core_channels,
            } => {
                let sample_rate = i32::try_from(*core_sample_rate).map_err(|_| invalid_rate())?;
                let channel_count = i32::try_from(*core_channels).map_err(|_| invalid_rate())?;
                // SAFETY: контекст валиден и не открыт; `av_channel_layout_default`
                // заполняет встроенный в контекст `ch_layout` раскладкой по числу каналов.
                unsafe {
                    (*raw_context).sample_rate = sample_rate;
                    ffmpeg_sys_next::av_channel_layout_default(
                        ptr::addr_of_mut!((*raw_context).ch_layout),
                        channel_count,
                    );
                }
                Ok(())
            }
        }
    }

    /// Копирует `AudioSpecificConfig` в `extradata` с нулевым хвостом FFmpeg.
    fn assign_extradata(&self, config_bytes: &[u8]) -> Result<(), AudioFfmpegError> {
        let extradata_size = i32::try_from(config_bytes.len()).map_err(|_| {
            AudioFfmpegError::InvalidStreamConfig {
                details: format!(
                    "AudioSpecificConfig is too large: {} bytes",
                    config_bytes.len()
                ),
            }
        })?;
        let allocation_size =
            config_bytes.len() + ffmpeg_sys_next::AV_INPUT_BUFFER_PADDING_SIZE as usize;
        // SAFETY: `av_mallocz` возвращает обнулённый буфер или null; нули в хвосте —
        // обязательное требование читателя битов FFmpeg.
        let buffer = unsafe { ffmpeg_sys_next::av_mallocz(allocation_size) }.cast::<u8>();
        let buffer = NonNull::new(buffer).ok_or(AudioFfmpegError::AllocationFailed {
            operation: "av_mallocz(extradata)",
        })?;
        // SAFETY: буфер длиной не меньше `config_bytes.len()` и не пересекается с
        // источником. Дальше буфером владеет контекст и освобождает его сам
        // в `avcodec_free_context`.
        unsafe {
            ptr::copy_nonoverlapping(config_bytes.as_ptr(), buffer.as_ptr(), config_bytes.len());
            let raw_context = self.context.as_ptr();
            (*raw_context).extradata = buffer.as_ptr();
            (*raw_context).extradata_size = extradata_size;
        }
        Ok(())
    }

    /// Декодирует один пакет и возвращает весь PCM, который он дал.
    ///
    /// `Ok(None)` — пакет принят, но кадра ещё нет (так бывает у первого
    /// пакета некоторых потоков). Пустой пакет FFmpeg понял бы как «конец
    /// потока», поэтому его не отправляем вовсе.
    pub(crate) fn decode_packet(
        &mut self,
        packet_bytes: &[u8],
    ) -> Result<Option<DecodedAacPcm>, AudioFfmpegError> {
        if packet_bytes.is_empty() {
            return Ok(None);
        }
        self.fill_packet(packet_bytes)?;
        let send_result = self.send_filled_packet();
        // SAFETY: пакет наш; снимаем ссылку на данные на любом исходе отправки,
        // иначе следующий `av_new_packet` потерял бы буфер (утечка).
        unsafe { ffmpeg_sys_next::av_packet_unref(self.packet.as_ptr()) };
        let decoded_before_send = send_result?;
        self.receive_all_frames(decoded_before_send)
    }

    /// Отправляет заполненный пакет; при `EAGAIN` сначала забирает готовые
    /// кадры и повторяет отправку один раз. Возвращает PCM, забранный до отправки.
    fn send_filled_packet(&mut self) -> Result<Option<DecodedAacPcm>, AudioFfmpegError> {
        // SAFETY: контекст открыт, пакет заполнен; FFmpeg берёт свою ссылку на данные.
        let mut send_status = unsafe {
            ffmpeg_sys_next::avcodec_send_packet(self.context.as_ptr(), self.packet.as_ptr())
        };
        let mut decoded_before_send = None;
        if send_status == AVERROR_EAGAIN {
            // Декодер держит невыданный кадр: забираем его и повторяем отправку один раз.
            decoded_before_send = self.receive_all_frames(None)?;
            // SAFETY: те же контекст и пакет; пакет ещё не снят.
            send_status = unsafe {
                ffmpeg_sys_next::avcodec_send_packet(self.context.as_ptr(), self.packet.as_ptr())
            };
        }
        if send_status < 0 {
            return Err(packet_error_from_averror(
                "avcodec_send_packet",
                send_status,
            ));
        }
        Ok(decoded_before_send)
    }

    /// Копирует байты пакета в пакет FFmpeg с обнулённым хвостом.
    fn fill_packet(&mut self, packet_bytes: &[u8]) -> Result<(), AudioFfmpegError> {
        let packet_size =
            i32::try_from(packet_bytes.len()).map_err(|_| AudioFfmpegError::CorruptPacket {
                message: format!("packet is too large: {} bytes", packet_bytes.len()),
            })?;
        // SAFETY: пакет наш и снят после прошлого использования; `av_new_packet`
        // выделяет `size` байт плюс обнулённый хвост `AV_INPUT_BUFFER_PADDING_SIZE`.
        let status = unsafe { ffmpeg_sys_next::av_new_packet(self.packet.as_ptr(), packet_size) };
        if status < 0 {
            return Err(error_from_averror("av_new_packet", status));
        }
        // SAFETY: только что выделено ровно `packet_bytes.len()` байт полезных данных.
        unsafe {
            ptr::copy_nonoverlapping(
                packet_bytes.as_ptr(),
                (*self.packet.as_ptr()).data,
                packet_bytes.len(),
            );
        }
        Ok(())
    }

    /// Забирает все готовые кадры и склеивает их PCM.
    fn receive_all_frames(
        &mut self,
        mut decoded: Option<DecodedAacPcm>,
    ) -> Result<Option<DecodedAacPcm>, AudioFfmpegError> {
        loop {
            // SAFETY: контекст открыт, кадр наш и снят после прошлого чтения.
            let status = unsafe {
                ffmpeg_sys_next::avcodec_receive_frame(self.context.as_ptr(), self.frame.as_ptr())
            };
            if status == AVERROR_EAGAIN || status == AVERROR_EOF {
                return Ok(decoded);
            }
            if status < 0 {
                return Err(packet_error_from_averror("avcodec_receive_frame", status));
            }
            let frame =
                NonNull::new(self.frame.as_ptr()).ok_or(AudioFfmpegError::AllocationFailed {
                    operation: "av_frame_alloc",
                })?;
            // SAFETY: кадр только что успешно получен и до снятия никем не меняется.
            let frame_pcm = unsafe { decoded_pcm_from_frame(frame) };
            // SAFETY: PCM уже скопирован (или получена ошибка) — буферы кадра больше не нужны.
            unsafe { ffmpeg_sys_next::av_frame_unref(self.frame.as_ptr()) };
            let frame_pcm = frame_pcm?;
            match decoded.as_mut() {
                Some(packet_pcm) => packet_pcm.append(frame_pcm)?,
                None => decoded = Some(frame_pcm),
            }
        }
    }

    /// Сбрасывает внутреннее состояние после перемотки (`avcodec_flush_buffers`).
    pub(crate) fn flush(&mut self) {
        // SAFETY: контекст открыт и используется только через `&mut self`.
        unsafe { ffmpeg_sys_next::avcodec_flush_buffers(self.context.as_ptr()) };
    }
}

fn invalid_rate() -> AudioFfmpegError {
    AudioFfmpegError::InvalidStreamConfig {
        details: "core sample rate or channel count does not fit FFmpeg".to_owned(),
    }
}
