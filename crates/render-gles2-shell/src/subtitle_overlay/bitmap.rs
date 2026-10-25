//! Картинка растеризатора субтитров плеера в словаре слоя оверлеев `gles2-video`.

use gles2_video::{
    OverlayBitmap, OverlayBitmapRect, OverlayPixelFormat, OverlayPixels, OverlayPlacementGroup,
    OverlayTint,
};
use subtitle_raster::{BitmapPixelFormat, SubtitleBitmap};

/// `SubtitleBitmap` для слоя оверлеев. Клон дешёвый: пиксели — окно в общем `Arc`-буфере
/// растеризатора, клон держит этот буфер живым (на этом стоит сравнение «те же пиксели»).
#[derive(Debug, Clone)]
pub(super) struct SubtitleOverlayBitmap(pub(super) SubtitleBitmap);

impl OverlayBitmap for SubtitleOverlayBitmap {
    fn rect(&self) -> OverlayBitmapRect {
        let origin = self.0.origin();
        OverlayBitmapRect {
            x: origin.x,
            y: origin.y,
            width: self.0.width(),
            height: self.0.height(),
        }
    }

    fn pixels(&self) -> OverlayPixels<'_> {
        let (format, bytes) = match self.0.pixel_format() {
            BitmapPixelFormat::Coverage => (OverlayPixelFormat::Coverage, self.0.coverage()),
            // `rgba_pixels` — `Some` ровно для этого формата; пустой срез на случай нарушения
            // инварианта растеризатора слой отвергнет проверкой размера (`MalformedBitmap`).
            BitmapPixelFormat::PremultipliedRgba => (
                OverlayPixelFormat::PremultipliedRgba,
                self.0.rgba_pixels().unwrap_or(&[]),
            ),
        };
        OverlayPixels {
            format,
            bytes,
            stride: self.0.stride(),
        }
    }

    fn tint(&self) -> OverlayTint {
        let color = self.0.color();
        OverlayTint {
            red: color.red,
            green: color.green,
            blue: color.blue,
            alpha: color.alpha,
        }
    }

    fn placement_group(&self) -> OverlayPlacementGroup {
        match self.0.placement_group() {
            subtitle_raster::OverlayPlacementGroup::Main => OverlayPlacementGroup::Main,
            subtitle_raster::OverlayPlacementGroup::Fixed => OverlayPlacementGroup::Fixed,
        }
    }

    fn shows_same_pixels_as(&self, other: &Self) -> bool {
        self.0.shows_same_pixels_as(&other.0)
    }
}
