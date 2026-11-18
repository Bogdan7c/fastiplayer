//! Чтение декодированного `AVFrame` в нейтральный PCM.
//!
//! Все поля кадра проверяются до того, как по ним читается память: формат
//! отсчётов, частота, число каналов и указатели плоскостей. Перечисления FFmpeg
//! (`AVChannelOrder`) читаются как числа, а не как Rust-`enum` биндингов.

use std::ptr::{self, NonNull};

use audio_core::AudioOutputSpec;

use crate::channel_mapping::{FfmpegChannelOrder, map_ffmpeg_channels};
use crate::decoded_pcm::{DecodedAacPcm, interleave_planes};
use crate::error::AudioFfmpegError;

/// `AV_SAMPLE_FMT_FLTP`: формат встроенного AAC-декодера FFmpeg (float planar).
const FLOAT_PLANAR_SAMPLE_FORMAT: i32 = ffmpeg_sys_next::AVSampleFormat::AV_SAMPLE_FMT_FLTP as i32;
/// `AV_CHANNEL_ORDER_UNSPEC` и `AV_CHANNEL_ORDER_NATIVE` как числа.
const CHANNEL_ORDER_UNSPECIFIED: i32 =
    ffmpeg_sys_next::AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC as i32;
const CHANNEL_ORDER_NATIVE: i32 = ffmpeg_sys_next::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32;
/// Верхняя граница каналов AAC (до 48 по спецификации) с запасом.
const MAX_FRAME_CHANNELS: usize = 64;

/// Переводит заполненный декодером кадр в interleaved PCM.
///
/// # Safety
/// `frame` — кадр, только что успешно полученный `avcodec_receive_frame`,
/// и не изменяемый никем до конца вызова.
pub(super) unsafe fn decoded_pcm_from_frame(
    frame: NonNull<ffmpeg_sys_next::AVFrame>,
) -> Result<DecodedAacPcm, AudioFfmpegError> {
    let raw_frame = frame.as_ptr();
    // SAFETY: по контракту функции кадр валиден; читаем простые числовые поля.
    let (sample_format, sample_rate, frame_samples) = unsafe {
        (
            (*raw_frame).format,
            (*raw_frame).sample_rate,
            (*raw_frame).nb_samples,
        )
    };
    if sample_format != FLOAT_PLANAR_SAMPLE_FORMAT {
        return Err(AudioFfmpegError::UnsupportedSampleFormat {
            format: sample_format,
        });
    }
    let sample_rate = u32::try_from(sample_rate)
        .ok()
        .filter(|rate| *rate > 0)
        .ok_or_else(|| AudioFfmpegError::UnsupportedChannelLayout {
            details: format!("frame has invalid sample rate {sample_rate}"),
        })?;
    let frame_samples =
        usize::try_from(frame_samples).map_err(|_| AudioFfmpegError::UnsupportedChannelLayout {
            details: format!("frame has negative sample count {frame_samples}"),
        })?;

    // SAFETY: `ch_layout` — поле валидного кадра; порядок читается как i32,
    // чтобы неожиданное значение не стало невалидным Rust-`enum`.
    let (channel_order, channel_count) = unsafe { read_channel_order(raw_frame) };
    let channel_count = usize::try_from(channel_count)
        .ok()
        .filter(|count| (1..=MAX_FRAME_CHANNELS).contains(count))
        .ok_or_else(|| AudioFfmpegError::UnsupportedChannelLayout {
            details: format!("frame has {channel_count} channels"),
        })?;
    let mapping = map_ffmpeg_channels(channel_order, channel_count)?;

    // SAFETY: формат FLTP проверен, число каналов ограничено; FFmpeg гарантирует
    // `channel_count` плоскостей по `frame_samples` отсчётов f32 в `extended_data`.
    let planes = unsafe { float_planes(raw_frame, channel_count, frame_samples)? };
    let interleaved_samples = interleave_planes(&planes, &mapping)?;
    Ok(DecodedAacPcm {
        output_spec: AudioOutputSpec::new(sample_rate, mapping.layout),
        interleaved_samples,
    })
}

/// Читает порядок каналов и маску кадра как числа.
///
/// # Safety
/// `raw_frame` указывает на валидный `AVFrame`.
unsafe fn read_channel_order(
    raw_frame: *const ffmpeg_sys_next::AVFrame,
) -> (FfmpegChannelOrder, i32) {
    // SAFETY: адрес поля валидного кадра; `AVChannelOrder` — C-enum размером int.
    let raw_order = unsafe { ptr::read(ptr::addr_of!((*raw_frame).ch_layout.order).cast::<i32>()) };
    // SAFETY: простое числовое поле валидного кадра.
    let channel_count = unsafe { (*raw_frame).ch_layout.nb_channels };
    let order = match raw_order {
        CHANNEL_ORDER_NATIVE => {
            // SAFETY: для NATIVE FFmpeg требует, чтобы активным членом union была `mask`.
            let mask = unsafe { (*raw_frame).ch_layout.u.mask };
            FfmpegChannelOrder::Native { mask }
        }
        CHANNEL_ORDER_UNSPECIFIED => FfmpegChannelOrder::Unspecified,
        raw_order => FfmpegChannelOrder::Other { raw_order },
    };
    (order, channel_count)
}

/// Срезы плоскостей float кадра.
///
/// # Safety
/// Кадр в формате FLTP с `channel_count` плоскостями по `frame_samples` отсчётов.
unsafe fn float_planes<'frame>(
    raw_frame: *const ffmpeg_sys_next::AVFrame,
    channel_count: usize,
    frame_samples: usize,
) -> Result<Vec<&'frame [f32]>, AudioFfmpegError> {
    // SAFETY: поле валидного кадра; у planar-звука это массив из `channel_count` указателей.
    let plane_pointers = unsafe { (*raw_frame).extended_data };
    if plane_pointers.is_null() {
        return Err(AudioFfmpegError::UnsupportedChannelLayout {
            details: "frame has no data planes".to_owned(),
        });
    }
    (0..channel_count)
        .map(|plane_index| {
            // SAFETY: индекс меньше числа каналов, массив указателей той же длины.
            let plane_pointer = unsafe { *plane_pointers.add(plane_index) }.cast::<f32>();
            if plane_pointer.is_null() {
                return Err(AudioFfmpegError::UnsupportedChannelLayout {
                    details: format!("frame plane {plane_index} is null"),
                });
            }
            // SAFETY: плоскость FLTP содержит `frame_samples` выровненных f32 и живёт,
            // пока кадр не снят `av_frame_unref` (после копирования в interleaved).
            Ok(unsafe { std::slice::from_raw_parts(plane_pointer, frame_samples) })
        })
        .collect()
}
