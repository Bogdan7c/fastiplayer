//! Группы размещения картинок слоя: что показ может двигать независимо.
//!
//! Растеризатор только ПОМЕЧАЕТ картинки группой; решение, двигать ли группу и насколько,
//! принимает показ (app-egui). Группа зависит лишь от реплики, поэтому кэш реплик остаётся
//! корректным: сдвиг группы не требует перерастеризации.

use super::{CanvasRect, SubtitleBitmap, SubtitleOverlay};

/// Группа размещения картинки внутри слоя субтитров.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverlayPlacementGroup {
    /// Основные реплики: SRT/WebVTT и ASS-диалог без `\pos`/`\move` с выравниванием снизу
    /// (`\an1`-`\an3`). Именно их показ поднимает над панелью управления.
    Main,
    /// Реплики, привязанные автором к месту кадра: `\pos`/`\move`, верхнее и среднее
    /// выравнивание (надписи, вывески). Показ их не двигает в режиме «поднять диалог».
    Fixed,
}

impl SubtitleOverlay {
    /// Объединяющий прямоугольник картинок группы `group` в пикселях холста; `None`, если в
    /// группе нет картинок. Правила границ те же, что у [`SubtitleOverlay::content_bounds`].
    pub fn group_bounds(&self, group: OverlayPlacementGroup) -> Option<CanvasRect> {
        union_bounds(
            self.bitmaps
                .iter()
                .filter(|bitmap| bitmap.placement_group() == group),
        )
    }
}

/// Объединяющий прямоугольник заданных картинок; `None` для пустого набора или при выходе
/// размеров за `i32`/`u32` (недостижимо при инварианте «внутри холста»).
pub(super) fn union_bounds<'a>(
    bitmaps: impl Iterator<Item = &'a SubtitleBitmap>,
) -> Option<CanvasRect> {
    let mut bounds: Option<(i64, i64, i64, i64)> = None;
    for bitmap in bitmaps {
        let origin = bitmap.origin();
        let left = i64::from(origin.x);
        let top = i64::from(origin.y);
        let right = left + i64::from(bitmap.width());
        let bottom = top + i64::from(bitmap.height());
        bounds = Some(match bounds {
            None => (left, top, right, bottom),
            Some((l, t, r, b)) => (l.min(left), t.min(top), r.max(right), b.max(bottom)),
        });
    }
    let (left, top, right, bottom) = bounds?;
    Some(CanvasRect {
        x: i32::try_from(left).ok()?,
        y: i32::try_from(top).ok()?,
        width: u32::try_from(right - left).ok()?,
        height: u32::try_from(bottom - top).ok()?,
    })
}
