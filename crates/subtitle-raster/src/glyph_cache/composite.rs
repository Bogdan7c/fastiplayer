//! Кэш склеек участков (libass: `composite_cache`) и разделяемые маски его значений.

use std::sync::Arc;

use super::identity::ArcId;
use crate::bitmap::CoverageBitmap;
use crate::cache::{ByteLruCache, CacheStatistics};

/// Служебные расходы одной записи сверх пикселей масок (заголовки, `Arc`, ключ без частей).
const ENTRY_OVERHEAD_BYTES: usize = 256;

/// Неизменяемая маска с разделяемыми пикселями: картинка кадра берёт тот же `Arc`, ничего не
/// копируя.
///
/// Пиксели обрезаны до `(height - 1) * stride + width` байт - ровно столько, сколько требует
/// `SubtitleBitmap` (хвост шага последней строки не нужен).
#[derive(Debug)]
pub(crate) struct SharedMask {
    /// Левый край относительно начала участка (см. [`PlacedMask`]).
    pub(crate) left: i32,
    /// Верхний край относительно начала участка.
    pub(crate) top: i32,
    /// Ширина в пикселях.
    pub(crate) width: i32,
    /// Высота в пикселях.
    pub(crate) height: i32,
    /// Шаг строки в байтах.
    pub(crate) stride: usize,
    /// Пиксели.
    pub(crate) pixels: Arc<[u8]>,
}

impl SharedMask {
    /// Замораживает изменяемый битмап в разделяемую маску (один раз при построении записи).
    pub(crate) fn freeze(bitmap: CoverageBitmap) -> Self {
        let (width, height) = (
            usize::try_from(bitmap.width).unwrap_or(0),
            usize::try_from(bitmap.height).unwrap_or(0),
        );
        let needed = if width == 0 || height == 0 {
            0
        } else {
            (height - 1)
                .saturating_mul(bitmap.stride)
                .saturating_add(width)
        };
        let used = needed.min(bitmap.buffer.len());
        let pixels: Arc<[u8]> = Arc::from(&bitmap.buffer[..used]);
        Self {
            left: bitmap.left,
            top: bitmap.top,
            width: bitmap.width,
            height: bitmap.height,
            stride: bitmap.stride,
            pixels,
        }
    }
}

/// Разделяемая маска и смещение участка в кадре: положение маски в кадре =
/// `(mask.left + offset_x, mask.top + offset_y)`.
#[derive(Debug, Clone)]
pub(crate) struct PlacedMask {
    mask: Arc<SharedMask>,
    offset_x: i32,
    offset_y: i32,
}

impl PlacedMask {
    /// Маска со смещением участка.
    pub(crate) fn new(mask: Arc<SharedMask>, offset: (i32, i32)) -> Self {
        Self {
            mask,
            offset_x: offset.0,
            offset_y: offset.1,
        }
    }

    /// Левый край в кадре.
    pub(crate) fn left(&self) -> i32 {
        self.mask.left.saturating_add(self.offset_x)
    }

    /// Верхний край в кадре.
    pub(crate) fn top(&self) -> i32 {
        self.mask.top.saturating_add(self.offset_y)
    }

    /// Ширина.
    pub(crate) fn width(&self) -> i32 {
        self.mask.width
    }

    /// Высота.
    pub(crate) fn height(&self) -> i32 {
        self.mask.height
    }

    /// Шаг строки.
    pub(crate) fn stride(&self) -> usize {
        self.mask.stride
    }

    /// Пиксели (общие с кэшем).
    pub(crate) fn pixels(&self) -> &Arc<[u8]> {
        &self.mask.pixels
    }
}

/// Описание фильтров в ключе склейки (libass: `FilterDesc`). Числа с плавающей точкой лежат
/// битами: два ключа равны, только если дисперсии размытия совпали бит в бит.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FilterKey {
    /// Флаги слоёв участка.
    pub(crate) flags: u8,
    /// Биты дисперсий гауссова размытия по X и Y.
    pub(crate) gaussian_bits: [u64; 2],
    /// Число проходов `\be`.
    pub(crate) box_passes: u32,
    /// Смещение тени в 26.6.
    pub(crate) shadow_offset: (i32, i32),
    /// Предел стороны размытого слоя. Результат размытия зависит от него (слишком большой
    /// слой остаётся неразмытым), поэтому он часть ключа - так же, как `side_limit` в ключе
    /// маски (`glyph_cache::bitmap`).
    pub(crate) blur_side_limit: u32,
}

/// Одна часть участка в ключе: маски глифа и их позиции ОТНОСИТЕЛЬНО начала участка
/// (libass: `BitmapRef`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PartKey {
    /// Маска заливки.
    pub(crate) fill: Option<ArcId<CoverageBitmap>>,
    /// Маска обводки или плашки.
    pub(crate) outline: Option<ArcId<CoverageBitmap>>,
    /// Позиция заливки.
    pub(crate) fill_position: (i32, i32),
    /// Позиция обводки.
    pub(crate) outline_position: (i32, i32),
}

/// Ключ склейки. Цвета, клип и караоке в него не входят: они применяются после кэша.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CompositeKey {
    /// Фильтры.
    pub(crate) filter: FilterKey,
    /// Части участка по порядку.
    pub(crate) parts: Vec<PartKey>,
}

/// Результат склейки участка в координатах начала участка (libass: `CompositeHashValue`).
#[derive(Debug, Default)]
pub(crate) struct CompositeEntry {
    /// Заливка.
    pub(crate) fill: Option<Arc<SharedMask>>,
    /// Обводка или плашка.
    pub(crate) outline: Option<Arc<SharedMask>>,
    /// Тень.
    pub(crate) shadow: Option<Arc<SharedMask>>,
    /// Слоёв, оставшихся неразмытыми по пределу размера (учитывается и при попадании в кэш,
    /// чтобы счётчики не зависели от прогретости кэша).
    pub(crate) unblurred_oversized_layers: u32,
    /// Слоёв, оставшихся неразмытыми из-за нарушенного инварианта.
    pub(crate) failed_blur_layers: u32,
}

impl CompositeEntry {
    /// Учётный размер записи: пиксели трёх масок и обе копии ключа.
    fn accounted_bytes(&self, key: &CompositeKey) -> usize {
        let masks: usize = [&self.fill, &self.outline, &self.shadow]
            .into_iter()
            .flatten()
            .map(|mask| mask.pixels.len())
            .sum();
        // Ключ лежит в кэше двумя копиями (индекс и ячейка), каждая со своим `Vec` частей.
        const KEY_COPIES: usize = 2;
        masks
            .saturating_add(
                key.parts
                    .len()
                    .saturating_mul(size_of::<PartKey>())
                    .saturating_mul(KEY_COPIES),
            )
            .saturating_add(ENTRY_OVERHEAD_BYTES)
    }
}

/// Кэш склеек; вытеснение — [`Self::cut`] в начале кадра.
pub(crate) struct CompositeCache {
    entries: ByteLruCache<CompositeKey, CompositeEntry>,
}

impl CompositeCache {
    /// Пустой кэш на `max_bytes` байт масок.
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            entries: ByteLruCache::new(max_bytes),
        }
    }

    /// Готовая склейка по ключу (считает попадание или промах).
    pub(crate) fn get(&mut self, key: &CompositeKey) -> Option<Arc<CompositeEntry>> {
        self.entries.get(key)
    }

    /// Запоминает склейку и возвращает её общую копию.
    pub(crate) fn insert(
        &mut self,
        key: CompositeKey,
        entry: CompositeEntry,
    ) -> Arc<CompositeEntry> {
        let bytes = entry.accounted_bytes(&key);
        let shared = Arc::new(entry);
        self.entries
            .insert_deferred(key, Arc::clone(&shared), bytes);
        shared
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
