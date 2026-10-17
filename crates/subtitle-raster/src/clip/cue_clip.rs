//! Клип реплики целиком: прямоугольный и/или векторный, и их применение к списку картинок.

use super::paint::ImagePaint;
use super::rect::ScreenRectClip;
use super::vector::{VectorClipEffect, VectorClipMask};
use super::vector_build::{
    IgnoredVectorClip, VectorClipOutcome, VectorClipPlan, build_vector_clip_mask,
};
use crate::bitmap::BitmapError;
use crate::overlay::{CanvasSize, LocalRect, SubtitleBitmap, SubtitleBitmapKind};

/// Геометрия клипа реплики, не зависящая от времени.
///
/// Ключа кэша реплик она не касается: кэш хранит картинки БЕЗ клипа, клип накладывается при
/// сборке кадра ([`Self::apply`]). Так же устроен анимированный `\t(\clip(...))`: стадия
/// принимает прямоугольник на момент кадра ([`Self::apply_with_rect`]), а не вшивает его в
/// кэшированные маски. Караоке встанет сюда же.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CueClip {
    rect: Option<ScreenRectClip>,
    vector: Option<VectorClipMask>,
}

/// Клип реплики и сведения о том, что из заказанного не применяется.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BuiltCueClip {
    /// Готовый клип.
    pub(crate) clip: CueClip,
    /// Почему векторный клип не применяется (если автор его задал, но он отброшен).
    pub(crate) ignored_vector: Option<IgnoredVectorClip>,
}

impl CueClip {
    /// Реплика без клипа.
    pub(crate) const NONE: Self = Self {
        rect: None,
        vector: None,
    };

    /// Клип из прямоугольника и/или векторной маски (libass допускает оба сразу).
    pub(crate) fn new(rect: Option<ScreenRectClip>, vector: Option<VectorClipMask>) -> Self {
        Self { rect, vector }
    }

    /// Собирает клип реплики: прямоугольник берётся как есть, векторная фигура растеризуется
    /// (растеризуется только часть внутри `canvas`). Ошибка — сбой растеризатора, не «клип не нужен».
    pub(crate) fn build(
        rect: Option<ScreenRectClip>,
        vector: Option<&VectorClipPlan>,
        canvas: CanvasSize,
    ) -> Result<BuiltCueClip, BitmapError> {
        let (mask, ignored_vector) = match vector.map(|plan| build_vector_clip_mask(plan, canvas)) {
            None => (None, None),
            Some(Err(error)) => return Err(error),
            Some(Ok(VectorClipOutcome::Ready(mask))) => (Some(mask), None),
            Some(Ok(VectorClipOutcome::Ignored(reason))) => (None, Some(reason)),
        };
        Ok(BuiltCueClip {
            clip: Self::new(rect, mask),
            ignored_vector,
        })
    }

    /// Неизменный прямоугольник клипа (без покадровой анимации).
    pub(crate) fn static_rect(&self) -> Option<&ScreenRectClip> {
        self.rect.as_ref()
    }

    /// Клипа нет: картинки идут в кадр без изменений и без копирования.
    pub(crate) fn is_none(&self) -> bool {
        self.rect.is_none() && self.vector.is_none()
    }

    /// Память под маску векторного клипа (входит в размер записи кэша реплик).
    pub(crate) fn mask_bytes(&self) -> usize {
        self.vector.as_ref().map_or(0, VectorClipMask::mask_bytes)
    }

    /// Применяет клип к картинкам реплики; порядок — как у libass (`render_text`).
    ///
    /// Порядок такой же, как в `render_glyph` -> `blend_vector_clip`: сначала каждая картинка
    /// режется прямоугольником (инверсный даёт до четырёх кусков подряд), затем каждый кусок
    /// проходит векторную маску. Подложка (`SubtitleBitmapKind::Background`: `BorderStyle = 4` и
    /// плашка пользовательского стиля) клипом не затрагивается — в libass `add_background`
    /// добавляет её уже после клипов. Владелец, цвет и группа размещения куски наследуют от картинки.
    pub(crate) fn apply(&self, bitmaps: &[SubtitleBitmap]) -> Vec<SubtitleBitmap> {
        self.apply_with_rect(self.rect.as_ref(), bitmaps, None)
    }

    /// То же, что [`Self::apply`], но с другим прямоугольником (векторная маска остаётся) и
    /// раскраской караоке: так накладывается покадровый клип `\t(\clip(...))` и заливка
    /// `\kf`. `paints[i]` относится к `bitmaps[i]`; без `paints` (или для картинки за концом
    /// списка) картинки остаются как в кэше.
    pub(crate) fn apply_with_rect(
        &self,
        rect: Option<&ScreenRectClip>,
        bitmaps: &[SubtitleBitmap],
        paints: Option<&[ImagePaint]>,
    ) -> Vec<SubtitleBitmap> {
        let mut result = Vec::with_capacity(bitmaps.len());
        let mut parts: Vec<SubtitleBitmap> = Vec::new();
        for (index, image) in bitmaps.iter().enumerate() {
            if image.kind() == SubtitleBitmapKind::Background {
                result.push(image.clone());
                continue;
            }
            let paint = paints
                .and_then(|paints| paints.get(index))
                .copied()
                .unwrap_or(ImagePaint::AsStored);
            parts.clear();
            match rect {
                Some(rect) => rect.push_visible_parts(image, paint, &mut parts),
                None => push_whole(image, paint, &mut parts),
            }
            for part in parts.drain(..) {
                match &self.vector {
                    None => result.push(part),
                    Some(vector) => match vector.apply(&part) {
                        VectorClipEffect::Unchanged => result.push(part),
                        VectorClipEffect::Removed => {}
                        VectorClipEffect::Replaced(clipped) => result.push(clipped),
                    },
                }
            }
        }
        result
    }
}

/// Картинка целиком (без прямоугольного клипа) с раскраской `paint`.
fn push_whole(image: &SubtitleBitmap, paint: ImagePaint, out: &mut Vec<SubtitleBitmap>) {
    if paint.is_as_stored() {
        out.push(image.clone());
        return;
    }
    let whole = LocalRect {
        x0: 0,
        y0: 0,
        x1: image.width(),
        y1: image.height(),
    };
    paint.push_region(image, whole, out);
}
