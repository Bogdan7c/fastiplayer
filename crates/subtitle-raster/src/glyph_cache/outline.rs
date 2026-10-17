//! Кэш контуров (libass: `outline_cache`): глифы шрифта, рисунки `\p` и обводки.

use std::sync::Arc;

use skrifa::GlyphId;

use super::identity::ArcId;
use crate::cache::{ByteLruCache, CacheStatistics};
use crate::drawing::parse_drawing;
use crate::face_cache::{FaceCache, FaceLoadError};
use crate::fonts::FaceId;
use crate::glyph_render::{GlyphOutline, GlyphVariant, load_styled_glyph_outline};
use crate::glyph_transform::{BorderStrokePlan, STROKER_PRECISION, control_box};
use crate::outline::stroke::{StrokeParameters, stroke_outline};
use crate::outline::{ControlBox, Outline, OutlineError};

/// Размер одной записи в учёте кэша: контуры считаются штуками (как `glyph_max` libass).
const ONE_ITEM: usize = 1;

/// Обводка контура: две границы и их общая рамка (libass: значение `OUTLINE_BORDER`).
#[derive(Debug)]
pub(crate) struct StrokedOutline {
    /// Граница со смещением +1.
    pub(crate) positive: Outline,
    /// Граница со смещением -1.
    pub(crate) negative: Outline,
    /// Рамка обеих границ вместе (нули, если они пусты).
    pub(crate) control_box: ControlBox,
}

/// Разобранный рисунок `\p` (libass: значение `OUTLINE_DRAWING`).
#[derive(Debug)]
pub(crate) struct DrawingOutline {
    /// Контур рисунка с рамкой; может быть пустым (рисунок без отрезков).
    pub(crate) outline: Arc<GlyphOutline>,
    /// Ширина рамки точек в 26.6 (с заворачиванием `i32`, как в libass).
    pub(crate) width: i32,
    /// Высота рамки точек в 26.6.
    pub(crate) height: i32,
}

/// Итог разбора рисунка, как его хранит кэш.
#[derive(Debug, Clone)]
pub(crate) enum DrawingEntry {
    /// Команды отвергнуты (координата вне диапазона): рисунок ничего не рисует и не имеет
    /// метрик.
    Rejected,
    /// Рисунок разобран (контур может быть пустым).
    Parsed(Arc<DrawingOutline>),
}

/// Ключ обводки: исходный контур и параметры, на которых обводчик получает свой результат
/// (libass: `BorderHashKey`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BorderKey {
    source: ArcId<GlyphOutline>,
    scale_order_x: i32,
    scale_order_y: i32,
    border_x: i32,
    border_y: i32,
}

/// Ключ записи кэша контуров (libass: `OutlineHashKey`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum OutlineKey {
    /// Глиф лица и вариант (синтетика, линии).
    Glyph {
        face: FaceId,
        glyph: GlyphId,
        variant: GlyphVariant,
    },
    /// Рисунок: ключ — его текст.
    Drawing(String),
    /// Обводка контура.
    Border(BorderKey),
}

/// Значение записи. Неудача хранится как запись (`None`/`Rejected`): повторять заведомо
/// неудачную загрузку или обводку каждый кадр незачем (libass: `valid = false`).
#[derive(Debug)]
enum OutlineValue {
    Glyph(Option<Arc<GlyphOutline>>),
    Drawing(DrawingEntry),
    Border(Option<Arc<StrokedOutline>>),
}

/// Кэш контуров, считаемый в штуках; вытеснение — [`Self::cut`] в начале кадра.
pub(crate) struct OutlineCache {
    entries: ByteLruCache<OutlineKey, OutlineValue>,
    /// Единичный квадрат плашки `BorderStyle = 3` (libass: `OUTLINE_BOX`); один на кэш.
    unit_box: Option<Arc<GlyphOutline>>,
}

impl OutlineCache {
    /// Пустой кэш на `max_items` записей.
    pub(crate) fn new(max_items: usize) -> Self {
        Self {
            entries: ByteLruCache::new(max_items),
            unit_box: None,
        }
    }

    /// Контур глифа нужного варианта; `None` — контур не загрузился (глиф не рисуется).
    ///
    /// Ошибка лица (`FaceLoadError`) не кэшируется: это сбой подготовки шрифта, а не свойство
    /// глифа.
    pub(crate) fn glyph(
        &mut self,
        faces: &FaceCache,
        face: FaceId,
        glyph: GlyphId,
        variant: GlyphVariant,
    ) -> Result<Option<Arc<GlyphOutline>>, FaceLoadError> {
        let key = OutlineKey::Glyph {
            face,
            glyph,
            variant,
        };
        if let Some(found) = self.entries.get(&key)
            && let OutlineValue::Glyph(known) = &*found
        {
            return Ok(known.clone());
        }
        let entry = faces.get(face)?;
        let font = entry.font_ref()?;
        let loaded = match load_styled_glyph_outline(
            &font,
            entry.scale(),
            entry.style(),
            glyph,
            variant,
        ) {
            Ok(outline) => Some(Arc::new(outline)),
            Err(error) => {
                tracing::debug!(%error, face = face.index(), "glyph outline is unavailable, the glyph is not drawn");
                None
            }
        };
        self.remember(key, OutlineValue::Glyph(loaded.clone()));
        Ok(loaded)
    }

    /// Разобранный рисунок по тексту команд (libass: `OUTLINE_DRAWING`).
    pub(crate) fn drawing(&mut self, commands: &str) -> DrawingEntry {
        let key = OutlineKey::Drawing(commands.to_owned());
        if let Some(found) = self.entries.get(&key)
            && let OutlineValue::Drawing(known) = &*found
        {
            return known.clone();
        }
        let entry = match parse_drawing(commands) {
            Ok(parsed) => {
                let (width, height) = (parsed.width(), parsed.height());
                DrawingEntry::Parsed(Arc::new(DrawingOutline {
                    outline: Arc::new(GlyphOutline::from_outline(parsed.outline)),
                    width,
                    height,
                }))
            }
            Err(error) => {
                // Текст рисунка в журнал не пишем: реплика может быть приватной.
                tracing::debug!(%error, "ASS drawing is rejected, the drawing is not drawn");
                DrawingEntry::Rejected
            }
        };
        self.remember(key, OutlineValue::Drawing(entry.clone()));
        entry
    }

    /// Обводка `source` с параметрами `plan`; `None` — обводку построить не удалось (контур
    /// слишком велик или сбой обводчика): глиф остаётся без неё. Пустой исходный контур
    /// обводить нельзя — вызывающий проверяет это раньше (libass: `!n_points`).
    pub(crate) fn stroked(
        &mut self,
        source: &Arc<GlyphOutline>,
        plan: &BorderStrokePlan,
    ) -> Option<Arc<StrokedOutline>> {
        let key = OutlineKey::Border(BorderKey {
            source: ArcId::new(source),
            scale_order_x: plan.scale_order_x,
            scale_order_y: plan.scale_order_y,
            border_x: plan.border_x,
            border_y: plan.border_y,
        });
        if let Some(found) = self.entries.get(&key)
            && let OutlineValue::Border(known) = &*found
        {
            return known.clone();
        }
        let built = build_stroked_outline(&source.outline, plan);
        self.remember(key, OutlineValue::Border(built.clone()));
        built
    }

    /// Единичный квадрат плашки `BorderStyle = 3`; создаётся при первом обращении.
    pub(crate) fn unit_box(&mut self) -> Result<Arc<GlyphOutline>, OutlineError> {
        if let Some(known) = &self.unit_box {
            return Ok(Arc::clone(known));
        }
        let mut square = Outline::new();
        square.add_rect(0, 0, 64, 64)?;
        let shared = Arc::new(GlyphOutline::from_outline(square));
        self.unit_box = Some(Arc::clone(&shared));
        Ok(shared)
    }

    /// Вытесняет давние записи сверх лимита (начало кадра).
    pub(crate) fn cut(&mut self) {
        self.entries.cut();
    }

    /// Забывает всё (набор шрифтов изменился).
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.unit_box = None;
    }

    /// Счётчики; «байты» этого кэша — штуки.
    pub(crate) fn statistics(&self) -> CacheStatistics {
        self.entries.statistics()
    }

    fn remember(&mut self, key: OutlineKey, value: OutlineValue) {
        self.entries.insert_deferred(key, Arc::new(value), ONE_ITEM);
    }
}

/// Строит обводку (libass: ветка `OUTLINE_BORDER` в `ass_outline_construct`).
fn build_stroked_outline(source: &Outline, plan: &BorderStrokePlan) -> Option<Arc<StrokedOutline>> {
    let Ok(scaled) = source.scaled_pow2(plan.scale_order_x, plan.scale_order_y) else {
        tracing::debug!("glyph outline is too large to stroke, drawing without border");
        return None;
    };
    let stroked = match stroke_outline(
        &scaled,
        StrokeParameters {
            border_x: plan.border_x * STROKER_PRECISION,
            border_y: plan.border_y * STROKER_PRECISION,
            precision: STROKER_PRECISION,
        },
    ) {
        Ok(stroked) => stroked,
        Err(error) => {
            // libass: «Cannot stroke outline» — глиф остаётся без обводки.
            tracing::debug!(%error, "cannot stroke a glyph outline, drawing without border");
            return None;
        }
    };
    let bounds = control_box(&[&stroked.positive, &stroked.negative]);
    Some(Arc::new(StrokedOutline {
        positive: stroked.positive,
        negative: stroked.negative,
        control_box: bounds,
    }))
}
