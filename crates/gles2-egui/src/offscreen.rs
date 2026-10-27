//! Offscreen-цель: FBO с текстурой RGBA8 и чтение пикселей.
//!
//! Это стенд пиксельных тестов этой и следующих сессий (egui-художник, видео, субтитры): контекст
//! на EGL surfaceless, кадр рисуется в FBO, `glReadPixels` отдаёт результат. На машине
//! разработчика — настоящий GPU, в CI — llvmpipe.

use crate::context::driver::SurfaceSize;
use crate::context::safety::gl::{CurrentGl, OffscreenGlObjects};
use crate::damage::BufferAge;

/// Сколько задних буферов в кольце offscreen-цели, которая сообщает возраст как EGL.
/// Реальные цепочки показа — 2–4 буфера; больше восьми тестовому стенду не нужно.
pub(crate) const MAX_OFFSCREEN_BACK_BUFFERS: usize = 8;

/// Учёт кольца задних буферов: какой буфер сейчас задний, какой показан последним и сколько
/// кадров назад каждый из них был показан. Чистая логика без GL — её проверяют отдельно.
///
/// Возраст считается так, как требует `EGL_EXT_buffer_age`: у буфера, который ещё не
/// показывали, он 0 (содержимое не определено); после показа кадра у этого буфера возраст 1, а у
/// каждого ранее показанного растёт на 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackBufferRing {
    ages: Vec<u32>,
    current: usize,
    last_presented: usize,
    reports_age: bool,
}

impl BackBufferRing {
    /// Один буфер, возраст не сообщается: поведение обычной offscreen-цели.
    pub(crate) fn single_without_age() -> Self {
        Self::new(1, false)
    }

    /// Кольцо из `count` буферов (не меньше одного), возраст сообщается как у EGL.
    pub(crate) fn with_age_reporting(count: usize) -> Self {
        Self::new(count, true)
    }

    fn new(count: usize, reports_age: bool) -> Self {
        Self {
            ages: vec![0; count.max(1)],
            current: 0,
            last_presented: 0,
            reports_age,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.ages.len()
    }

    /// Индекс заднего буфера, в который рисует кадр.
    pub(crate) fn current(&self) -> usize {
        self.current
    }

    /// Индекс буфера, показанного последним (его читает readback).
    pub(crate) fn last_presented(&self) -> usize {
        self.last_presented
    }

    /// Возраст заднего буфера.
    pub(crate) fn current_age(&self) -> BufferAge {
        if !self.reports_age {
            return BufferAge::Unsupported;
        }
        BufferAge::from_egl_value(self.ages.get(self.current).copied().unwrap_or(0))
    }

    /// Кадр показан: у задних буферов, показанных раньше, возраст +1, у показанного он 1, задним
    /// становится следующий по кругу.
    pub(crate) fn present_current(&mut self) {
        for age in &mut self.ages {
            if *age > 0 {
                *age += 1;
            }
        }
        if let Some(age) = self.ages.get_mut(self.current) {
            *age = 1;
        }
        self.last_presented = self.current;
        self.current = (self.current + 1) % self.ages.len();
    }

    /// Все буферы переоткрыты (resize): содержимое не определено.
    pub(crate) fn invalidate_all(&mut self) {
        self.ages.fill(0);
        self.current = 0;
        self.last_presented = 0;
    }
}

/// Владелец FBO и текстур offscreen-цели. Удаляется явно ([`Self::destroy`]) из `Drop`
/// контекста, пока контекст ещё текущий.
#[derive(Debug)]
pub(crate) struct OffscreenTarget {
    /// По одному FBO на задний буфер кольца.
    buffers: Vec<OffscreenGlObjects>,
    ring: BackBufferRing,
    size: SurfaceSize,
}

impl OffscreenTarget {
    /// Создаёт цель заданного размера с кольцом `ring`.
    pub(crate) fn create(
        gl: CurrentGl<'_>,
        size: SurfaceSize,
        ring: BackBufferRing,
    ) -> Result<Self, String> {
        let mut buffers = Vec::with_capacity(ring.len());
        for _ in 0..ring.len() {
            match gl.create_offscreen_objects(size) {
                Ok(objects) => buffers.push(objects),
                Err(error) => {
                    for created in buffers {
                        gl.delete_offscreen_objects(created);
                    }
                    return Err(error);
                }
            }
        }
        // Создание оставило FBO привязанным — возвращаем буфер по умолчанию, кадр привяжет сам.
        gl.bind_framebuffer(None);
        Ok(Self {
            buffers,
            ring,
            size,
        })
    }

    /// FBO заднего буфера для привязки в начале кадра.
    pub(crate) fn framebuffer(&self) -> Option<glow::NativeFramebuffer> {
        self.buffers
            .get(self.ring.current())
            .map(|objects| objects.framebuffer)
    }

    /// Возраст заднего буфера.
    pub(crate) fn buffer_age(&self) -> BufferAge {
        self.ring.current_age()
    }

    /// Кадр показан: кольцо поворачивается.
    pub(crate) fn present_current(&mut self) {
        self.ring.present_current();
    }

    /// Новый размер (содержимое не сохраняется, возраст всех буферов сбрасывается).
    pub(crate) fn resize(&mut self, gl: CurrentGl<'_>, size: SurfaceSize) {
        if size != self.size {
            for objects in &self.buffers {
                gl.allocate_color_texture(objects.color_texture, size);
            }
            self.ring.invalidate_all();
            self.size = size;
        }
    }

    /// Пиксели последнего ПОКАЗАННОГО кадра (при кольце это буфер, показанный последним, а не
    /// тот, в который рисует следующий кадр).
    pub(crate) fn read_pixels(&self, gl: CurrentGl<'_>) -> OffscreenPixels {
        let presented = self
            .buffers
            .get(self.ring.last_presented())
            .map(|objects| objects.framebuffer);
        gl.bind_framebuffer(presented);
        let bottom_up = gl.read_rgba_bottom_up(self.size);
        OffscreenPixels::from_bottom_up(self.size, &bottom_up)
    }

    /// Удаляет GL-объекты (ровно один раз: `self` поглощается).
    pub(crate) fn destroy(self, gl: CurrentGl<'_>) {
        for objects in self.buffers {
            gl.delete_offscreen_objects(objects);
        }
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
    fn ring_reports_egl_ages_for_one_two_and_three_buffers() {
        // Ожидаемый возраст заднего буфера на каждом из шести кадров подряд.
        for (count, expected) in [
            (1_usize, [0_u32, 1, 1, 1, 1, 1]),
            (2, [0, 0, 2, 2, 2, 2]),
            (3, [0, 0, 0, 3, 3, 3]),
        ] {
            let mut ring = BackBufferRing::with_age_reporting(count);
            let mut seen = Vec::new();
            for _ in 0..expected.len() {
                seen.push(match ring.current_age() {
                    BufferAge::Undefined => 0,
                    BufferAge::Frames(frames) => frames,
                    BufferAge::Unsupported => u32::MAX,
                });
                ring.present_current();
            }
            assert_eq!(seen, expected, "ring of {count}");
        }
    }

    #[test]
    fn ring_without_age_reporting_says_unsupported_and_resize_invalidates() {
        let mut plain = BackBufferRing::single_without_age();
        plain.present_current();
        assert_eq!(plain.current_age(), BufferAge::Unsupported);

        let mut ring = BackBufferRing::with_age_reporting(2);
        ring.present_current();
        ring.present_current();
        assert_eq!(ring.current_age(), BufferAge::Frames(2));
        ring.invalidate_all();
        assert_eq!(ring.current_age(), BufferAge::Undefined);
        assert_eq!((ring.current(), ring.last_presented()), (0, 0));
    }

    #[test]
    fn readback_index_follows_the_presented_buffer() {
        let mut ring = BackBufferRing::with_age_reporting(3);
        ring.present_current();
        assert_eq!((ring.last_presented(), ring.current()), (0, 1));
        ring.present_current();
        assert_eq!((ring.last_presented(), ring.current()), (1, 2));
    }

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
