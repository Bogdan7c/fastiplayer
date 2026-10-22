//! Свой `get_buffer2`: FFmpeg декодирует кадры прямо в блоки `HostFrameBlockPool`.
//!
//! Зачем (cpu-parity 08b). Кадр в одном блоке, выровненном по странице, со stride,
//! кратным 256, renderer импортирует в GPU (`VK_EXT_external_memory_host`) и копирует в
//! текстуры без CPU-копии в staging. Так делает и mpv (`--vd-lavc-dr`), только он
//! выделяет память у GPU; мы — у себя, поэтому декодер не зависит от видеокарты.
//!
//! Правила FFmpeg 8.1 (`AVCodecContext.get_buffer2`, `get_buffer.c::update_frame_pool`):
//! - ставится только декодерам с `AV_CODEC_CAP_DR1` (у libdav1d его нет — свой пул);
//! - размеры берутся из `frame`, а не из context-а, и выравниваются
//!   `avcodec_align_dimensions2`;
//! - stride каждой plane-ы кратен и выравниванию декодера, и 256 (GPU), и при этом
//!   нечётен в этих единицах, чтобы строки не конфликтовали в кэше CPU
//!   (`cache_friendly_linesizes`);
//! - каждая plane выровнена «по максимуму для CPU» и имеет хвостовой запас.
//!
//! Любой неподходящий случай (формат вне нашей матрицы, переполнение, пул закрыт,
//! нет памяти) — `DirectFrameBufferFallback`: caller зовёт `avcodec_default_get_buffer2`,
//! и кадр идёт обычным путём через staging.

use std::ffi::c_void;
use std::os::raw::c_int;
use std::ptr::{self, NonNull};
use std::sync::Arc;

use super::host_frame_memory::{HostFrameBlockLayout, HostFrameBlockPool};
use super::pixel_format::SoftwarePixelFormat;
use video_core::HOST_IMPORTABLE_ROW_PITCH_ALIGNMENT;

/// Почему кадр нельзя положить в наш блок; caller уходит на путь FFmpeg по умолчанию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DirectFrameBufferFallback {
    /// Формат вне матрицы host-planar форматов (например, у декодера свой формат).
    UnsupportedPixelFormat,
    /// Неположительные или переполняющие размеры кадра.
    InvalidFrameGeometry,
    /// FFmpeg отказал в расчёте строк/plane-ов.
    ImageLayoutRejected,
    /// Пул закрыт или память не выделилась.
    BlockUnavailable,
    /// `av_buffer_create` не создал буфер.
    BufferWrapFailed,
}

/// Кладёт `frame` в блок пула: заполняет `data`, `linesize`, `buf[0]`, `extended_data`.
///
/// # Safety
///
/// `context` и `frame` — указатели, которые FFmpeg передал в `get_buffer2`: живые, а
/// `frame.format/width/height` уже заполнены декодером.
pub(super) unsafe fn allocate_frame_in_pool(
    context: *mut ffmpeg_sys_next::AVCodecContext,
    frame: *mut ffmpeg_sys_next::AVFrame,
    pool: &Arc<HostFrameBlockPool>,
) -> Result<(), DirectFrameBufferFallback> {
    // SAFETY: caller гарантирует живой `frame`; читаем поля, которые заполнил декодер.
    let (format_code, width, height) =
        unsafe { ((*frame).format, (*frame).width, (*frame).height) };
    let Some(software_format) = SoftwarePixelFormat::from_av_format_code(format_code) else {
        return Err(DirectFrameBufferFallback::UnsupportedPixelFormat);
    };
    if width <= 0 || height <= 0 {
        return Err(DirectFrameBufferFallback::InvalidFrameGeometry);
    }
    let pixel_format = software_format.av_pixel_format();

    // SAFETY: caller гарантирует живой context.
    let geometry = unsafe { aligned_frame_geometry(context, pixel_format, width, height) }?;
    let plane_sizes = &geometry.plane_sizes[..geometry.plane_count];
    let layout = HostFrameBlockLayout::for_plane_sizes(plane_sizes)
        .ok_or(DirectFrameBufferFallback::InvalidFrameGeometry)?;
    let block = pool
        .checkout(layout.block_len())
        .ok_or(DirectFrameBufferFallback::BlockUnavailable)?;
    let block_address = block.address();

    // Буфер держит пул (а не блок): освобождение вернёт блок в пул по адресу, сам блок
    // до этого держит `checked_out` пула. `Arc::into_raw` уравновешивается в
    // `release_frame_block` ровно одним `Arc::from_raw`.
    let pool_reference = Arc::into_raw(Arc::clone(pool)).cast_mut().cast::<c_void>();
    // SAFETY: адрес и длина — живой блок пула; FFmpeg вызовет `release_frame_block`
    // ровно один раз, когда отпустят последнюю ссылку на буфер.
    let buffer = unsafe {
        ffmpeg_sys_next::av_buffer_create(
            block_address.as_ptr(),
            layout.block_len(),
            Some(release_frame_block),
            pool_reference,
            0,
        )
    };
    if buffer.is_null() {
        // Буфер не создан, callback не будет вызван: возвращаем блок и ссылку на пул сами.
        pool.give_back(block_address);
        // SAFETY: `pool_reference` получен `Arc::into_raw` выше и ещё не возвращён.
        drop(unsafe { Arc::from_raw(pool_reference.cast::<HostFrameBlockPool>()) });
        return Err(DirectFrameBufferFallback::BufferWrapFailed);
    }

    // SAFETY: `frame` жив; offset-ы plane-ов лежат внутри блока (раскладка проверена
    // `HostFrameBlockLayout`), stride-ы рассчитаны FFmpeg для этого формата и ширины.
    unsafe {
        let frame = &mut *frame;
        frame.buf[0] = buffer;
        for plane_index in 0..frame.data.len() {
            if plane_index < geometry.plane_count {
                frame.data[plane_index] = block_address
                    .as_ptr()
                    .add(layout.plane_offsets()[plane_index]);
                frame.linesize[plane_index] = geometry.linesizes[plane_index];
            } else {
                frame.data[plane_index] = ptr::null_mut();
                frame.linesize[plane_index] = 0;
            }
        }
        frame.extended_data = frame.data.as_mut_ptr();
    }
    Ok(())
}

/// Callback освобождения буфера: возвращает блок в пул.
///
/// FFmpeg вызывает его из потока, который отпустил последнюю ссылку на кадр.
unsafe extern "C" fn release_frame_block(opaque: *mut c_void, data: *mut u8) {
    // SAFETY: `opaque` — ровно тот указатель, что `allocate_frame_in_pool` получил через
    // `Arc::into_raw`; FFmpeg вызывает callback один раз на буфер.
    let pool = unsafe { Arc::from_raw(opaque.cast_const().cast::<HostFrameBlockPool>()) };
    if let Some(block_address) = NonNull::new(data) {
        pool.give_back(block_address);
    }
}

/// Строки и размеры plane-ов кадра, выровненные для нашего блока.
#[derive(Debug)]
struct AlignedFrameGeometry {
    /// Stride plane-ов (кратны 256 и выравниванию декодера).
    linesizes: [c_int; 4],
    /// Размеры plane-ов в байтах.
    plane_sizes: [usize; 4],
    /// Сколько plane-ов у формата.
    plane_count: usize,
}

/// Повторяет расчёт `update_frame_pool` FFmpeg, но со stride, пригодным для GPU-копии
/// и без конфликтов строк в кэше CPU (`cache_friendly_linesizes`).
///
/// # Safety
///
/// `context` — живой `AVCodecContext`, переданный FFmpeg в `get_buffer2`.
unsafe fn aligned_frame_geometry(
    context: *mut ffmpeg_sys_next::AVCodecContext,
    pixel_format: ffmpeg_sys_next::AVPixelFormat,
    width: c_int,
    height: c_int,
) -> Result<AlignedFrameGeometry, DirectFrameBufferFallback> {
    let mut aligned_width = width;
    let mut aligned_height = height;
    let mut decoder_linesize_align = [0 as c_int; ffmpeg_sys_next::AV_NUM_DATA_POINTERS as usize];
    // SAFETY: context жив; функция пишет только в переданные width/height и массив
    // из AV_NUM_DATA_POINTERS элементов.
    unsafe {
        ffmpeg_sys_next::avcodec_align_dimensions2(
            context,
            &mut aligned_width,
            &mut aligned_height,
            decoder_linesize_align.as_mut_ptr(),
        );
    }
    let stride_align = combined_stride_alignment(&decoder_linesize_align);

    let mut natural_linesizes = [0 as c_int; 4];
    // SAFETY: формат из нашей матрицы, массив на 4 элемента — как требует FFmpeg.
    let status = unsafe {
        ffmpeg_sys_next::av_image_fill_linesizes(
            natural_linesizes.as_mut_ptr(),
            pixel_format,
            aligned_width,
        )
    };
    if status < 0 {
        return Err(DirectFrameBufferFallback::ImageLayoutRejected);
    }
    let linesizes = cache_friendly_linesizes(&natural_linesizes, &stride_align)
        .ok_or(DirectFrameBufferFallback::InvalidFrameGeometry)?;

    let pointer_linesizes = linesizes.map(|linesize| linesize as isize);
    let mut plane_sizes = [0_usize; 4];
    // SAFETY: массивы на 4 элемента, формат из нашей матрицы, высота положительна.
    let status = unsafe {
        ffmpeg_sys_next::av_image_fill_plane_sizes(
            plane_sizes.as_mut_ptr(),
            pixel_format,
            aligned_height,
            pointer_linesizes.as_ptr(),
        )
    };
    if status < 0 {
        return Err(DirectFrameBufferFallback::ImageLayoutRejected);
    }
    let plane_count = plane_sizes.iter().take_while(|&&size| size > 0).count();
    if plane_count == 0 {
        return Err(DirectFrameBufferFallback::ImageLayoutRejected);
    }

    Ok(AlignedFrameGeometry {
        linesizes,
        plane_sizes,
        plane_count,
    })
}

/// Выравнивание stride plane-а: общее кратное требования декодера и 256 для GPU.
fn combined_stride_alignment(decoder_linesize_align: &[c_int]) -> [usize; 4] {
    let mut combined = [HOST_IMPORTABLE_ROW_PITCH_ALIGNMENT; 4];
    for (alignment, &decoder_alignment) in combined.iter_mut().zip(decoder_linesize_align) {
        let decoder_alignment = usize::try_from(decoder_alignment)
            .ok()
            .filter(|&value| value > 0)
            .unwrap_or(1);
        *alignment = least_common_multiple(HOST_IMPORTABLE_ROW_PITCH_ALIGNMENT, decoder_alignment);
    }
    combined
}

/// Stride каждой plane-ы: нечётное кратное её выравнивания, не меньше естественного.
///
/// Почему нечётное (замер cpu-parity 08b). Первая версия, как `update_frame_pool` FFmpeg,
/// расширяла ширину, пока все stride не станут кратны 256 вместе: для 4K это 4096 / 2048 —
/// степени двойки. Соседние строки plane-ы тогда попадают в одни и те же наборы кэша CPU
/// (L1: 64 набора по 64 байта = 4 КиБ), и декодер VP9 4K60 стал дороже на ~30 п.п. ядра.
/// Нечётное кратное 256 (3840 = 15 × 256, 2304 = 9 × 256) разносит строки по 16 наборам
/// L1 — как естественный stride FFmpeg 3840 — и остаётся годным для GPU-копии.
///
/// Stride plane-ов выравниваются по отдельности. Предупреждение FFmpeg о
/// `linesize[0] == 2 * linesize[1]` касается MPEG-кодировщиков; наши декодеры (H.264,
/// HEVC, VP9, VP8) читают stride каждой plane-ы отдельно, что подтверждают тесты
/// побайтного равенства. U и V получают одинаковый stride (одинаковая ширина).
/// `None` — отрицательный stride или переполнение.
fn cache_friendly_linesizes(
    natural_linesizes: &[c_int; 4],
    stride_align: &[usize; 4],
) -> Option<[c_int; 4]> {
    let mut linesizes = [0 as c_int; 4];
    for ((linesize, &natural), &alignment) in linesizes
        .iter_mut()
        .zip(natural_linesizes)
        .zip(stride_align)
    {
        let natural = usize::try_from(natural).ok()?;
        if natural == 0 {
            continue;
        }
        let mut multiples = natural.div_ceil(alignment);
        if multiples.is_multiple_of(2) {
            multiples += 1;
        }
        *linesize = c_int::try_from(multiples.checked_mul(alignment)?).ok()?;
    }
    Some(linesizes)
}

/// Наименьшее общее кратное (оба аргумента больше нуля).
fn least_common_multiple(left: usize, right: usize) -> usize {
    left / greatest_common_divisor(left, right) * right
}

/// Наибольший общий делитель по Евклиду.
fn greatest_common_divisor(mut left: usize, mut right: usize) -> usize {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

#[cfg(test)]
mod tests;
