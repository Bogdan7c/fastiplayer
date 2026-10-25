//! Картинка оверлея для unit-тестов: свой буфер пикселей в `Arc`, тождество — тот же буфер.

use std::sync::Arc;

use super::{
    OverlayBitmap, OverlayBitmapRect, OverlayPixelFormat, OverlayPixels, OverlayPlacementGroup,
    OverlayTint,
};

#[derive(Debug, Clone)]
pub(crate) struct FakeBitmap {
    rect: OverlayBitmapRect,
    format: OverlayPixelFormat,
    pixels: Arc<[u8]>,
}

impl FakeBitmap {
    fn new(format: OverlayPixelFormat, x: i32, y: i32, width: u32, height: u32) -> Self {
        let bytes = width as usize * height as usize * format.bytes_per_pixel();
        Self {
            rect: OverlayBitmapRect {
                x,
                y,
                width,
                height,
            },
            format,
            pixels: vec![0xFF; bytes].into(),
        }
    }

    pub(crate) fn coverage(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self::new(OverlayPixelFormat::Coverage, x, y, width, height)
    }

    pub(crate) fn picture(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self::new(OverlayPixelFormat::PremultipliedRgba, x, y, width, height)
    }
}

impl OverlayBitmap for FakeBitmap {
    fn rect(&self) -> OverlayBitmapRect {
        self.rect
    }

    fn pixels(&self) -> OverlayPixels<'_> {
        OverlayPixels {
            format: self.format,
            bytes: &self.pixels,
            stride: self.rect.width as usize * self.format.bytes_per_pixel(),
        }
    }

    fn tint(&self) -> OverlayTint {
        OverlayTint {
            red: 255,
            green: 255,
            blue: 255,
            alpha: 255,
        }
    }

    fn placement_group(&self) -> OverlayPlacementGroup {
        OverlayPlacementGroup::Main
    }

    fn shows_same_pixels_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.pixels, &other.pixels) && self.format == other.format
    }
}
