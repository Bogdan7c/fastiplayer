//! Авторские растровые субтитры (PGS): от сжатой реплики до картинки оверлея.
//!
//! Конвейер одной картинки: проверка размеров автора -> распаковка `subtitle_pgs`
//! (RLE, вырезка видимой области, палитра в премультиплицированный RGBA) -> перевод
//! координат автора в пиксели холста ([`geometry`]) -> масштабирование «среднее по площади»
//! только для видимой части ([`scale`]) -> [`SubtitleBitmap`] формата
//! [`crate::BitmapPixelFormat::PremultipliedRgba`].
//!
//! Модуль не знает про кэши и счётчики растеризатора: он возвращает результат или типизированную
//! причину отказа, а учёт ведёт `renderer`. Пользовательский масштаб и стиль к картинкам не
//! применяются.

mod geometry;
mod scale;

use std::sync::Arc;

use subtitle_core::SubtitlePictureCue;
use subtitle_pgs::{PictureDecodeError, PictureDecodeLimits, decode_picture_rgba};

use crate::overlay::{CanvasSize, OverlayPlacementGroup, PixelPoint, SubtitleBitmap};
use geometry::{CanvasSides, VisibleWindow, destination_rect, picture_placement_group};
use scale::{ScaleSizes, box_scale_window};

/// Почему картинку нельзя нарисовать (реплика пропускается, остальные рисуются).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PictureRenderError {
    /// Система координат автора или сама картинка больше лимита `max_canvas_side`.
    #[error(
        "picture of reference size {width}x{height} exceeds the limit of {max_side} pixels per side"
    )]
    OverLimit {
        width: u32,
        height: u32,
        max_side: u32,
    },
    /// Распаковка не удалась (неизвестная кодировка и т. п.).
    #[error("picture decoding failed: {0}")]
    Undecodable(PictureDecodeError),
    /// Распакованное изображение не согласовано со своими размерами (ошибка декодера или
    /// переполнение геометрии); в исправной сборке не бывает.
    #[error("decoded picture is inconsistent with its declared size")]
    InconsistentImage,
}

/// Результат успешной отрисовки.
#[derive(Debug)]
pub(crate) struct PictureRaster {
    /// Картинка оверлея; `None`, если на холсте от неё ничего не видно (целиком за краем
    /// или сжалась в ноль пикселей) - это не ошибка.
    pub(crate) bitmap: Option<SubtitleBitmap>,
    /// Поток RLE был повреждён (обрезан, лишние пиксели), картинка доделана нулями.
    pub(crate) damaged_data: bool,
}

/// Рисует картинку реплики на холсте.
///
/// `max_side` - предел `RasterInputLimits::max_canvas_side`: им же ограничены сторона системы
/// автора, сторона любой картинки и число пикселей распаковки (`max_side^2`), отдельных
/// лимитов нет.
pub(crate) fn rasterize_picture(
    cue: &SubtitlePictureCue,
    canvas: CanvasSize,
    max_side: u32,
) -> Result<PictureRaster, PictureRenderError> {
    let reference = cue.reference_size();
    let picture = cue.picture();
    let largest_side = reference
        .width()
        .get()
        .max(reference.height().get())
        .max(picture.width().get())
        .max(picture.height().get());
    if largest_side > max_side {
        return Err(PictureRenderError::OverLimit {
            width: reference.width().get(),
            height: reference.height().get(),
            max_side,
        });
    }
    let decoded = decode_picture_rgba(
        cue,
        PictureDecodeLimits {
            max_picture_pixels: u64::from(max_side) * u64::from(max_side),
        },
    )
    .map_err(|error| match error {
        PictureDecodeError::TooLarge { .. } => PictureRenderError::OverLimit {
            width: picture.width().get(),
            height: picture.height().get(),
            max_side,
        },
        other => PictureRenderError::Undecodable(other),
    })?;
    let damaged_data = !decoded.report.is_clean();

    let canvas_sides = CanvasSides {
        width: canvas.width(),
        height: canvas.height(),
    };
    let image = &decoded.image;
    let rect = destination_rect(
        cue.origin(),
        image.width(),
        image.height(),
        reference,
        canvas_sides,
    );
    let bitmap = match (
        u32::try_from(rect.width()),
        u32::try_from(rect.height()),
        VisibleWindow::within_canvas(rect, canvas_sides),
    ) {
        (Ok(destination_width), Ok(destination_height), Some(window))
            if destination_width > 0 && destination_height > 0 =>
        {
            let sizes = ScaleSizes {
                source_width: image.width(),
                source_height: image.height(),
                destination_width,
                destination_height,
            };
            Some(build_bitmap(cue, image.pixels(), sizes, window)?)
        }
        _ => None,
    };
    Ok(PictureRaster {
        bitmap,
        damaged_data,
    })
}

/// Масштабирует видимую часть и упаковывает её в картинку оверлея с группой размещения.
fn build_bitmap(
    cue: &SubtitlePictureCue,
    source_pixels: &[u8],
    sizes: ScaleSizes,
    window: VisibleWindow,
) -> Result<SubtitleBitmap, PictureRenderError> {
    let scaled = box_scale_window(source_pixels, sizes, window)
        .ok_or(PictureRenderError::InconsistentImage)?;
    let (Ok(x), Ok(y)) = (
        i32::try_from(window.canvas_x),
        i32::try_from(window.canvas_y),
    ) else {
        return Err(PictureRenderError::InconsistentImage);
    };
    let origin = PixelPoint { x, y };
    let group: OverlayPlacementGroup =
        picture_placement_group(cue.origin().y, cue.reference_size());
    SubtitleBitmap::picture(
        origin,
        window.width,
        window.height,
        Arc::from(scaled),
        cue.forced(),
    )
    .map(|bitmap| bitmap.with_placement_group(group))
    .map_err(|_| PictureRenderError::InconsistentImage)
}
