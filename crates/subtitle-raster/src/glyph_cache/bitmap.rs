//! Кэш масок глифов (libass: `bitmap_cache`).

use std::sync::Arc;

use super::identity::ArcId;
use super::outline::StrokedOutline;
use crate::bitmap::{BitmapError, CoverageBitmap};
use crate::cache::{ByteLruCache, CacheStatistics};
use crate::glyph_render::GlyphOutline;
use crate::glyph_transform::{QuantizedTransform, construct_bitmap};
use crate::outline::{ControlBox, Outline};
use crate::raster::Rasterizer;

/// Служебные расходы одной записи сверх самих пикселей (ключ, `Arc`, заголовок маски).
const ENTRY_OVERHEAD_BYTES: usize = 160;

/// Из какого контура растеризуется маска (владеет ссылкой, см. [`ArcId`]).
#[derive(Debug, Clone)]
pub(crate) enum BitmapSource {
    /// Контур глифа, рисунка или плашки: заливка.
    Outline(Arc<GlyphOutline>),
    /// Обводка: две границы рисуются вместе.
    Stroked(Arc<StrokedOutline>),
}

impl BitmapSource {
    fn identity(&self) -> SourceIdentity {
        match self {
            Self::Outline(outline) => SourceIdentity::Outline(ArcId::new(outline)),
            Self::Stroked(stroked) => SourceIdentity::Stroked(ArcId::new(stroked)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SourceIdentity {
    Outline(ArcId<GlyphOutline>),
    Stroked(ArcId<StrokedOutline>),
}

/// Ключ маски: контур и квантованное преобразование БЕЗ целой части позиции (libass:
/// `BitmapHashKey`). Целая часть позиции — не свойство маски: один и тот же глиф со сдвигом на
/// целые пиксели даёт ту же маску. Предел стороны входит в ключ, потому что от него зависит
/// отказ «слишком велик».
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BitmapKey {
    source: SourceIdentity,
    offset: [i32; 2],
    matrix_x: [i32; 2],
    matrix_y: [i32; 2],
    matrix_z: [i32; 2],
    side_limit: Option<u32>,
}

/// Итог растеризации, как его хранит кэш. Пустой результат и отказ по размеру тоже запись
/// (libass кэширует и нулевые маски).
#[derive(Debug)]
enum BitmapEntry {
    /// Контур пуст или преобразование вне допустимого диапазона: маски нет.
    Empty,
    /// Готовая маска.
    Mask(Arc<CoverageBitmap>),
    /// Маска шире предела стороны: глиф пропускается вызывающим.
    Oversized { width: i64, height: i64, limit: u32 },
}

/// Кэш масок; вытеснение — [`Self::cut`] в начале кадра.
pub(crate) struct BitmapCache {
    entries: ByteLruCache<BitmapKey, BitmapEntry>,
}

impl BitmapCache {
    /// Пустой кэш на `max_bytes` байт масок.
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            entries: ByteLruCache::new(max_bytes),
        }
    }

    /// Маска контура `source` при преобразовании `transform` (его `position` не учитывается).
    ///
    /// `Ok(None)` — маски нет (контур пуст или вышел из диапазона). Отказ по пределу стороны
    /// возвращается той же ошибкой `ExceedsSideLimit`, что и без кэша. Прочие ошибки
    /// растеризации не кэшируются.
    pub(crate) fn mask(
        &mut self,
        rasterizer: &mut Rasterizer,
        source: &BitmapSource,
        transform: &QuantizedTransform,
    ) -> Result<Option<Arc<CoverageBitmap>>, BitmapError> {
        let key = BitmapKey {
            source: source.identity(),
            offset: transform.offset,
            matrix_x: transform.matrix_x,
            matrix_y: transform.matrix_y,
            matrix_z: transform.matrix_z,
            side_limit: rasterizer.max_bitmap_side(),
        };
        if let Some(found) = self.entries.get(&key) {
            return match &*found {
                BitmapEntry::Empty => Ok(None),
                BitmapEntry::Mask(mask) => Ok(Some(Arc::clone(mask))),
                BitmapEntry::Oversized {
                    width,
                    height,
                    limit,
                } => Err(BitmapError::ExceedsSideLimit {
                    width: *width,
                    height: *height,
                    limit: *limit,
                }),
            };
        }
        let empty = Outline::new();
        let (bounds, outlines): (ControlBox, [&Outline; 2]) = match source {
            BitmapSource::Outline(glyph) => (glyph.control_box, [&glyph.outline, &empty]),
            BitmapSource::Stroked(stroked) => {
                (stroked.control_box, [&stroked.positive, &stroked.negative])
            }
        };
        match construct_bitmap(rasterizer, transform, &bounds, outlines) {
            Ok(Some(bitmap)) => {
                let bytes = bitmap.buffer.len() + ENTRY_OVERHEAD_BYTES;
                let mask = Arc::new(bitmap);
                self.entries.insert_deferred(
                    key,
                    Arc::new(BitmapEntry::Mask(Arc::clone(&mask))),
                    bytes,
                );
                Ok(Some(mask))
            }
            Ok(None) => {
                self.entries.insert_deferred(
                    key,
                    Arc::new(BitmapEntry::Empty),
                    ENTRY_OVERHEAD_BYTES,
                );
                Ok(None)
            }
            Err(BitmapError::ExceedsSideLimit {
                width,
                height,
                limit,
            }) => {
                self.entries.insert_deferred(
                    key,
                    Arc::new(BitmapEntry::Oversized {
                        width,
                        height,
                        limit,
                    }),
                    ENTRY_OVERHEAD_BYTES,
                );
                Err(BitmapError::ExceedsSideLimit {
                    width,
                    height,
                    limit,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Вытесняет давние записи сверх лимита (начало кадра).
    pub(crate) fn cut(&mut self) {
        self.entries.cut();
    }

    /// Забывает всё (набор шрифтов изменился).
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Счётчики кэша.
    pub(crate) fn statistics(&self) -> CacheStatistics {
        self.entries.statistics()
    }
}
