use vp9_parser::Vp9FrameInfo;

use crate::dpb::DecodedPictureBuffer;
use crate::raw_video::*;

/// Структура с базовыми данными VP9 для декодирования.
///
/// Не содержит self-referential полей. Вызывающий код должен использовать
/// эти данные для создания `VideoDecodeInfoKHR` непосредственно перед вызовом
/// `vkCmdDecodeVideoKHR`, когда все временные объекты живут в одном scope.
pub struct Vp9PictureInfo {
    /// Стандартная информация о VP9 кадре.
    pub std_picture_info: StdVideoDecodeVP9PictureInfo,

    /// VP9-специфичное расширение (без p_std_picture_info — устанавливается вызывающим).
    pub vp9_picture_info: VideoDecodeVP9PictureInfoKHR,

    /// Индексы опорных слотов (-1 = не используется).
    pub ref_name_slot_indices: [i8; 8],

    /// Индекс слота назначения в DPB.
    pub dst_slot_index: i32,

    /// Ширина кадра.
    pub width: u32,

    /// Высота кадра.
    pub height: u32,
}

/// Строит базовые структуры VP9 decode из распарсенного заголовка.
///
/// Возвращает `Vp9PictureInfo`. Вызывающий код должен:
/// 1. Установить `vp9_picture_info.p_std_picture_info`
/// 2. Создать `VideoDecodeInfoKHR` с правильными lifetimes
/// 3. Вызвать `vkCmdDecodeVideoKHR`
pub fn build_vp9_picture_info(
    frame_info: &Vp9FrameInfo,
    dpb: &DecodedPictureBuffer,
    dst_slot_index: i32,
) -> Vp9PictureInfo {
    // 1. Создаём стандартную структуру информации о VP9 кадре.
    let std_picture_info = StdVideoDecodeVP9PictureInfo {
        frame_width_minus_1: (frame_info.width.saturating_sub(1)) as u16,
        frame_height_minus_1: (frame_info.height.saturating_sub(1)) as u16,
        render_width_minus_1: (frame_info.render_width.saturating_sub(1)) as u16,
        render_height_minus_1: (frame_info.render_height.saturating_sub(1)) as u16,
        refresh_frame_flags: frame_info.refresh_frame_flags,
        frame_type: if frame_info.keyframe { 0 } else { 1 },
        _reserved1: 0,
        _reserved2: 0,
    };

    // 2. Отображаем опорные кадры VP9 в слоты DPB.
    let mut ref_name_slot_indices = [-1i8; 8];
    if !frame_info.keyframe {
        for i in 0..3 {
            if let Some(slot_idx) = dpb.map_reference(frame_info.ref_frame_idx[i]) {
                ref_name_slot_indices[i] = slot_idx as i8;
            }
        }
    }

    // 3. Создаём VP9-специфичную структуру расширения.
    // p_std_picture_info будет установлен вызывающим кодом.
    let vp9_picture_info = VideoDecodeVP9PictureInfoKHR {
        p_std_picture_info: std::ptr::null(),
        reference_name_slot_indices: ref_name_slot_indices,
        ..Default::default()
    };

    Vp9PictureInfo {
        std_picture_info,
        vp9_picture_info,
        ref_name_slot_indices,
        dst_slot_index,
        width: frame_info.width,
        height: frame_info.height,
    }
}
