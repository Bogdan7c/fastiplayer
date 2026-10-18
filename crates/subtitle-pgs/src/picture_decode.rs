//! Распаковка растровой реплики в готовые к наложению пиксели.
//!
//! Это единственная точка, где сжатая картинка превращается в пиксели: растеризатор
//! вызывает [`decode_picture_rgba`] только для реплик, которые действительно показываются.

use subtitle_core::{PictureEncoding, SubtitlePictureCue};

use crate::rle::{RleReport, decode_hdmv_rle};

/// Ограничения распаковки, задаваемые вызывающим (значения по умолчанию намеренно нет:
/// лимит — решение владельца конфигурации).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PictureDecodeLimits {
    /// Наибольшее число пикселей ПОЛНОЙ картинки объекта (до обрезки).
    pub max_picture_pixels: u64,
}

/// Изображение RGBA8 с ПРЕМУЛЬТИПЛИЦИРОВАННОЙ альфой, строки подряд без выравнивания
/// (`stride = width * 4`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PremultipliedRgbaImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl PremultipliedRgbaImage {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Байты RGBA, построчно сверху вниз.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Пиксель `[r, g, b, a]` в точке (x, y); `None` вне картинки.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = (usize::try_from(y).ok()? * usize::try_from(self.width).ok()?
            + usize::try_from(x).ok()?)
            * 4;
        let raw = self.pixels.get(offset..offset + 4)?;
        Some([raw[0], raw[1], raw[2], raw[3]])
    }
}

/// Результат распаковки: изображение видимой области и отчёт о «мягких» проблемах потока.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPicture {
    pub image: PremultipliedRgbaImage,
    pub report: RleReport,
}

/// Ошибка распаковки (изображения нет).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PictureDecodeError {
    /// Картинка больше лимита вызывающего.
    #[error("picture of {pixels} pixels exceeds the limit of {limit} pixels")]
    TooLarge { pixels: u64, limit: u64 },
    /// Этот crate не умеет данный способ кодирования.
    #[error("picture encoding is not supported by the PGS decoder")]
    UnsupportedEncoding,
}

/// Премультиплицирует канал с округлением: `round(channel * alpha / 255)`.
fn premultiply(channel: u8, alpha: u8) -> u8 {
    // Результат <= 255, потому что channel, alpha <= 255.
    ((u16::from(channel) * u16::from(alpha) + 127) / 255) as u8
}

/// Распаковывает реплику: RLE -> индексы -> вырезка `visible_region` -> палитра с
/// премультипликацией. Размер результата равен размеру видимой области.
pub fn decode_picture_rgba(
    cue: &SubtitlePictureCue,
    limits: PictureDecodeLimits,
) -> Result<DecodedPicture, PictureDecodeError> {
    let picture = cue.picture();
    let pixels = u64::from(picture.width().get()) * u64::from(picture.height().get());
    if pixels > limits.max_picture_pixels {
        return Err(PictureDecodeError::TooLarge {
            pixels,
            limit: limits.max_picture_pixels,
        });
    }
    // Лимит проверен: размеры помещаются в usize на целевых платформах (иначе — ошибка).
    let (Ok(width), Ok(height)) = (
        usize::try_from(picture.width().get()),
        usize::try_from(picture.height().get()),
    ) else {
        return Err(PictureDecodeError::TooLarge {
            pixels,
            limit: limits.max_picture_pixels,
        });
    };
    let decoded = match picture.encoding() {
        PictureEncoding::HdmvRle(bytes) => decode_hdmv_rle(bytes, width, height),
        _ => return Err(PictureDecodeError::UnsupportedEncoding),
    };

    let region = cue.visible_region();
    let palette = cue.palette();
    // Область проверена конструктором реплики: лежит внутри картинки, индексы не выйдут за буфер.
    let region_x = region.x() as usize;
    let region_y = region.y() as usize;
    let region_width = region.width().get() as usize;
    let region_height = region.height().get() as usize;
    let mut rgba = Vec::with_capacity(region_width * region_height * 4);
    for row in region_y..region_y + region_height {
        let start = row * width + region_x;
        for &index in &decoded.indices[start..start + region_width] {
            let color = palette.color(index);
            rgba.extend_from_slice(&[
                premultiply(color.red, color.alpha),
                premultiply(color.green, color.alpha),
                premultiply(color.blue, color.alpha),
                color.alpha,
            ]);
        }
    }
    Ok(DecodedPicture {
        image: PremultipliedRgbaImage {
            width: region.width().get(),
            height: region.height().get(),
            pixels: rgba,
        },
        report: decoded.report,
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::sync::Arc;

    use subtitle_core::{
        IndexedPicture, PaletteColor, PictureForcedFlag, PicturePalette, PicturePosition,
        PictureRect, PictureReferenceSize,
    };

    use super::*;

    fn nz(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("non-zero")
    }

    fn cue_with(rle: &[u8], width: u32, height: u32, region: PictureRect) -> SubtitlePictureCue {
        let mut colors = [PaletteColor::TRANSPARENT; PicturePalette::LEN];
        colors[1] = PaletteColor {
            red: 200,
            green: 100,
            blue: 50,
            alpha: 128,
        };
        colors[2] = PaletteColor {
            red: 10,
            green: 20,
            blue: 30,
            alpha: 255,
        };
        SubtitlePictureCue::new(
            PictureReferenceSize::new(nz(1920), nz(1080)),
            PicturePosition { x: 0, y: 0 },
            region,
            Arc::new(IndexedPicture::new(
                nz(width),
                nz(height),
                PictureEncoding::HdmvRle(Arc::from(rle.to_vec())),
            )),
            Arc::new(PicturePalette::new(colors)),
            PictureForcedFlag::Regular,
        )
        .expect("valid cue")
    }

    const LIMITS: PictureDecodeLimits = PictureDecodeLimits {
        max_picture_pixels: 1_000,
    };

    #[test]
    fn decodes_premultiplied_pixels_at_expected_positions() {
        // 3x2: [1 2 0] / [0 0 1]
        let rle = [1, 2, 0, 1, 0, 0, 0, 2, 1, 0, 0];
        let region = PictureRect::new(0, 0, nz(3), nz(2));
        let decoded = decode_picture_rgba(&cue_with(&rle, 3, 2, region), LIMITS).expect("decode");
        assert_eq!((decoded.image.width(), decoded.image.height()), (3, 2));
        // alpha 128: 200*128/255 = 100.39 -> 100, 100 -> 50, 50 -> 25.
        assert_eq!(decoded.image.pixel(0, 0), Some([100, 50, 25, 128]));
        assert_eq!(decoded.image.pixel(1, 0), Some([10, 20, 30, 255]));
        assert_eq!(decoded.image.pixel(2, 0), Some([0, 0, 0, 0]));
        assert_eq!(decoded.image.pixel(2, 1), Some([100, 50, 25, 128]));
        assert_eq!(decoded.image.pixel(3, 0), None);
        assert!(decoded.report.is_clean());
    }

    #[test]
    fn crops_to_visible_region() {
        let rle = [1, 2, 0, 1, 0, 0, 0, 2, 1, 0, 0];
        let region = PictureRect::new(1, 1, nz(2), nz(1));
        let decoded = decode_picture_rgba(&cue_with(&rle, 3, 2, region), LIMITS).expect("decode");
        assert_eq!((decoded.image.width(), decoded.image.height()), (2, 1));
        assert_eq!(decoded.image.pixels().len(), 8);
        assert_eq!(decoded.image.pixel(0, 0), Some([0, 0, 0, 0]));
        assert_eq!(decoded.image.pixel(1, 0), Some([100, 50, 25, 128]));
    }

    #[test]
    fn rejects_pictures_over_the_limit_without_decoding() {
        let region = PictureRect::new(0, 0, nz(100), nz(11));
        let cue = cue_with(&[0, 0], 100, 11, region);
        assert_eq!(
            decode_picture_rgba(&cue, LIMITS),
            Err(PictureDecodeError::TooLarge {
                pixels: 1_100,
                limit: 1_000
            })
        );
    }

    #[test]
    fn broken_data_still_yields_an_image_with_report() {
        let region = PictureRect::new(0, 0, nz(2), nz(2));
        let decoded = decode_picture_rgba(&cue_with(&[2], 2, 2, region), LIMITS).expect("decode");
        assert_eq!(decoded.image.pixel(0, 0), Some([10, 20, 30, 255]));
        assert_eq!(decoded.image.pixel(1, 1), Some([0, 0, 0, 0]));
        assert!(!decoded.report.is_clean());
    }
}
