//! Производные картинки: обрезка до подпрямоугольника и замена маски.
//!
//! Нужны стадии «после кэша» (`crate::clip`): клип превращает картинку реплики в одну или
//! несколько новых, сохраняя всё, что к маске не относится (цвет, вид, группу размещения и
//! владельца-реплику). Поэтому `SubtitleOverlay::retain_cues` и `OverlayPlacementGroup`
//! работают с обрезанными картинками так же, как с исходными.

use std::sync::Arc;

use super::{PixelPoint, SubtitleBitmap};

/// Подпрямоугольник картинки в её собственных координатах (правый и нижний края не входят).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalRect {
    /// Левый край, пиксели от левого края картинки.
    pub(crate) x0: u32,
    /// Верхний край, пиксели от верхнего края картинки.
    pub(crate) y0: u32,
    /// Правый край (не включён).
    pub(crate) x1: u32,
    /// Нижний край (не включён).
    pub(crate) y1: u32,
}

impl SubtitleBitmap {
    /// Часть картинки внутри `region` с той же геометрией маски (шаг строки сохраняется,
    /// как у `my_draw_bitmap` libass, который оставляет шаг исходного буфера).
    ///
    /// `None` — область пуста или выходит за картинку. Маска копируется: обрезок не должен
    /// держать в памяти отброшенные строки целой картинки.
    pub(crate) fn cropped(&self, region: LocalRect) -> Option<Self> {
        if region.x0 >= region.x1
            || region.y0 >= region.y1
            || region.x1 > self.width
            || region.y1 > self.height
        {
            return None;
        }
        let width = region.x1 - region.x0;
        let height = region.y1 - region.y0;
        let start = (region.y0 as usize)
            .checked_mul(self.stride)?
            .checked_add(region.x0 as usize)?;
        // Последняя строка без хвоста шага: нужна ровно ширина куска.
        let end = start
            .checked_add((height as usize - 1).checked_mul(self.stride)?)?
            .checked_add(width as usize)?;
        let visible = self.coverage.get(start..end)?;
        Some(Self {
            origin: PixelPoint {
                x: self.origin.x.checked_add_unsigned(region.x0)?,
                y: self.origin.y.checked_add_unsigned(region.y0)?,
            },
            width,
            height,
            coverage: Arc::from(visible),
            ..self.clone()
        })
    }

    /// Картинка с теми же цветом, видом, группой и владельцем, но другой маской и положением.
    ///
    /// `None`, если раскладка маски нарушает инвариант картинки (пустая площадь, короткий буфер).
    pub(crate) fn with_mask(
        &self,
        origin: PixelPoint,
        width: u32,
        height: u32,
        stride: usize,
        coverage: Arc<[u8]>,
    ) -> Option<Self> {
        let rebuilt = Self::new(
            origin, width, height, stride, coverage, self.color, self.kind,
        )
        .ok()?;
        Some(Self {
            placement_group: self.placement_group,
            owner: self.owner,
            ..rebuilt
        })
    }
}
