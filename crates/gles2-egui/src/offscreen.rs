//! Offscreen-цель: FBO с текстурой RGBA8 и чтение пикселей.
//!
//! Это стенд пиксельных тестов этой и следующих сессий (egui-художник, видео, субтитры): контекст
//! на EGL surfaceless, кадр рисуется в FBO, `glReadPixels` отдаёт результат. На машине
//! разработчика — настоящий GPU, в CI — llvmpipe.

use crate::context::driver::SurfaceSize;
use crate::context::safety::gl::{CurrentGl, OffscreenGlObjects};

/// Владелец FBO и текстуры offscreen-цели. Удаляется явно ([`Self::destroy`]) из `Drop`
/// контекста, пока контекст ещё текущий.
#[derive(Debug)]
pub(crate) struct OffscreenTarget {
    objects: OffscreenGlObjects,
    size: SurfaceSize,
}

impl OffscreenTarget {
    /// Создаёт цель заданного размера.
    pub(crate) fn create(gl: CurrentGl<'_>, size: SurfaceSize) -> Result<Self, String> {
        let objects = gl.create_offscreen_objects(size)?;
        // Создание оставило FBO привязанным — возвращаем буфер по умолчанию, кадр привяжет сам.
        gl.bind_framebuffer(None);
        Ok(Self { objects, size })
    }

    /// FBO для привязки в начале кадра.
    pub(crate) fn framebuffer(&self) -> glow::NativeFramebuffer {
        self.objects.framebuffer
    }

    /// Новый размер (содержимое не сохраняется).
    pub(crate) fn resize(&mut self, gl: CurrentGl<'_>, size: SurfaceSize) {
        if size != self.size {
            gl.allocate_color_texture(self.objects.color_texture, size);
            self.size = size;
        }
    }

    /// Пиксели последнего кадра.
    pub(crate) fn read_pixels(&self, gl: CurrentGl<'_>) -> OffscreenPixels {
        gl.bind_framebuffer(Some(self.objects.framebuffer));
        let bottom_up = gl.read_rgba_bottom_up(self.size);
        OffscreenPixels::from_bottom_up(self.size, &bottom_up)
    }

    /// Удаляет GL-объекты (ровно один раз: `self` поглощается).
    pub(crate) fn destroy(self, gl: CurrentGl<'_>) {
        gl.delete_offscreen_objects(self.objects);
    }
}

/// Пиксели RGBA8, строки сверху вниз (как на экране).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OffscreenPixels {
    size: SurfaceSize,
    rgba_top_down: Vec<u8>,
}

impl OffscreenPixels {
    fn from_bottom_up(size: SurfaceSize, bottom_up: &[u8]) -> Self {
        let row_bytes = usize::try_from(size.width())
            .unwrap_or_default()
            .saturating_mul(4);
        let mut rgba_top_down = Vec::with_capacity(bottom_up.len());
        if row_bytes > 0 {
            for row in bottom_up.chunks_exact(row_bytes).rev() {
                rgba_top_down.extend_from_slice(row);
            }
        }
        Self {
            size,
            rgba_top_down,
        }
    }

    /// Размер.
    pub fn size(&self) -> SurfaceSize {
        self.size
    }

    /// Пиксель `[r, g, b, a]` в координатах экрана (0, 0 — левый верхний); `None` вне цели.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.size.width() || y >= self.size.height() {
            return None;
        }
        let width = usize::try_from(self.size.width()).ok()?;
        let index = usize::try_from(y)
            .ok()?
            .checked_mul(width)?
            .checked_add(usize::try_from(x).ok()?)?
            .checked_mul(4)?;
        let bytes = self.rgba_top_down.get(index..index.checked_add(4)?)?;
        bytes.try_into().ok()
    }

    /// Все пиксели RGBA8 сверху вниз.
    pub fn as_rgba(&self) -> &[u8] {
        &self.rgba_top_down
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_flipped_to_screen_order() {
        let Some(size) = SurfaceSize::new(2, 2) else {
            panic!("2×2 is a valid size");
        };
        // GL: нижняя строка первой. Нижняя строка красная, верхняя зелёная.
        let bottom_up = [
            255, 0, 0, 255, 255, 0, 0, 255, //
            0, 255, 0, 255, 0, 255, 0, 255,
        ];
        let pixels = OffscreenPixels::from_bottom_up(size, &bottom_up);
        assert_eq!(pixels.pixel(0, 0), Some([0, 255, 0, 255]));
        assert_eq!(pixels.pixel(1, 1), Some([255, 0, 0, 255]));
        assert_eq!(pixels.pixel(2, 0), None);
    }
}
