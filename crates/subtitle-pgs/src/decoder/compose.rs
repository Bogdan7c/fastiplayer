//! Построение растровых реплик из PCS и состояния эпохи.

use std::num::NonZeroU32;

use subtitle_core::{PicturePosition, PictureRect, SubtitlePictureCue};

use super::PgsDecodeStatistics;
use super::epoch::EpochState;
use crate::pcs::{CompositionObject, PaletteUpdate, PresentationComposition};
use crate::wds::WindowDefinition;

fn rect_from_u16(x: u16, y: u16, width: u16, height: u16) -> Option<PictureRect> {
    Some(PictureRect::new(
        u32::from(x),
        u32::from(y),
        NonZeroU32::new(u32::from(width))?,
        NonZeroU32::new(u32::from(height))?,
    ))
}

fn window_rect(window: &WindowDefinition) -> Option<PictureRect> {
    rect_from_u16(window.x, window.y, window.width, window.height)
}

/// Строит реплики композиции. `None` — композицию собрать нельзя (нет объекта или
/// палитры); счётчик увеличен, состояние эпохи не тронуто. Вызывающий превращает это в
/// «очистку» экрана.
pub(super) fn compose(
    pcs: &PresentationComposition,
    epoch: &mut EpochState,
    stats: &mut PgsDecodeStatistics,
) -> Option<Vec<SubtitlePictureCue>> {
    // Палитровое обновление наследует список объектов предыдущей композиции.
    let objects: Vec<CompositionObject> = match pcs.palette_update {
        PaletteUpdate::PaletteOnly => epoch.last_composition_objects.clone(),
        PaletteUpdate::FullComposition => pcs.objects.clone(),
    };
    if objects.is_empty() {
        // Пустая композиция — «очистка»; палитра и объекты не нужны.
        epoch.last_composition_objects = objects;
        return Some(Vec::new());
    }
    let Some(palette) = epoch.palettes.get(&pcs.palette_id).cloned() else {
        stats.compositions_skipped_missing_resource += 1;
        return None;
    };
    if objects
        .iter()
        .any(|object| !epoch.objects.contains_key(&object.object_id))
    {
        stats.compositions_skipped_missing_resource += 1;
        return None;
    }
    let mut cues = Vec::with_capacity(objects.len());
    for object in &objects {
        // Наличие объекта проверено выше.
        let Some(picture) = epoch.objects.get(&object.object_id) else {
            continue;
        };
        let window = epoch.windows.get(&object.window_id).map(window_rect);
        match place_object(object, picture.full_rect(), window) {
            Some(placement) => match SubtitlePictureCue::new(
                pcs.reference_size,
                placement.origin,
                placement.visible_region,
                picture.clone(),
                palette.clone(),
                object.forced,
            ) {
                Ok(cue) => cues.push(cue),
                // Недостижимо: область уже вписана в картинку. Но если инвариант нарушен —
                // считаем, а не молчим.
                Err(_) => stats.objects_dropped_no_visible_area += 1,
            },
            None => stats.objects_dropped_no_visible_area += 1,
        }
    }
    epoch.last_composition_objects = objects;
    Some(cues)
}

/// Где и какая часть объекта видна.
struct Placement {
    origin: PicturePosition,
    visible_region: PictureRect,
}

/// Геометрия одного объекта.
///
/// - видимая часть картинки = прямоугольник обрезки (или вся картинка), вписанный в картинку;
/// - на экране она занимает прямоугольник с левым верхним углом в (x, y) объекта;
/// - если окно известно, экранный прямоугольник отсекается окном, а область картинки
///   сдвигается на ту же величину; пусто -> `None` (объект отбрасывается).
///   `window`: `None` — окно неизвестно (отсечения нет); `Some(None)` — окно с нулевым размером.
fn place_object(
    object: &CompositionObject,
    picture_rect: PictureRect,
    window: Option<Option<PictureRect>>,
) -> Option<Placement> {
    let wanted = match object.crop {
        Some(crop) => rect_from_u16(crop.x, crop.y, crop.width, crop.height)?,
        None => picture_rect,
    };
    let region = wanted.intersection(picture_rect)?;
    let screen = PictureRect::new(
        u32::from(object.x),
        u32::from(object.y),
        region.width(),
        region.height(),
    );
    let clipped = match window {
        None => screen,
        Some(window_rect) => screen.intersection(window_rect?)?,
    };
    let shift_x = clipped.x() - screen.x();
    let shift_y = clipped.y() - screen.y();
    Some(Placement {
        origin: PicturePosition {
            x: clipped.x(),
            y: clipped.y(),
        },
        visible_region: PictureRect::new(
            region.x() + shift_x,
            region.y() + shift_y,
            clipped.width(),
            clipped.height(),
        ),
    })
}
