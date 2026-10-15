//! LRU-кэш с лимитом в байтах.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::sync::Arc;

/// Счётчики кэша.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStatistics {
    /// Запрос нашёл запись.
    pub hits: u64,
    /// Запрос не нашёл запись.
    pub misses: u64,
    /// Записей вытеснено из-за нехватки места.
    pub evictions: u64,
    /// Записей не принято, потому что одна запись больше всего лимита.
    pub rejected_oversized: u64,
    /// Учтённый размер записей сейчас, байты (всегда не больше лимита).
    pub current_bytes: usize,
    /// Число записей сейчас.
    pub entries: usize,
}

/// Запись кэша: значение, его учтённый размер и момент последнего использования.
struct Slot<V> {
    value: Arc<V>,
    bytes: usize,
    last_used: u64,
}

/// Кэш «ключ -> `Arc<V>`», вытесняющий давно не использованное, когда сумма размеров
/// превышает лимит.
///
/// Инварианты:
/// - `current_bytes <= limit_bytes` ВСЕГДА (после любой операции);
/// - запись, которая одна больше лимита, не кэшируется (счётчик `rejected_oversized`):
///   вызывающий всё равно получает своё значение и рисует его, но повторно оно будет
///   растеризовано заново. Это осознанный компромисс: иначе одна огромная реплика вытеснила
///   бы весь кэш и всё равно превысила лимит.
pub(crate) struct ByteLruCache<K, V> {
    limit_bytes: usize,
    slots: HashMap<K, Slot<V>>,
    /// Порядок использования: `last_used` -> ключ; самая давняя запись — первая.
    recency: BTreeMap<u64, K>,
    clock: u64,
    statistics: CacheStatistics,
}

impl<K: Hash + Eq + Clone, V> ByteLruCache<K, V> {
    /// Пустой кэш с лимитом `limit_bytes` (0 — ничего не кэшируется).
    pub(crate) fn new(limit_bytes: usize) -> Self {
        Self {
            limit_bytes,
            slots: HashMap::new(),
            recency: BTreeMap::new(),
            clock: 0,
            statistics: CacheStatistics::default(),
        }
    }

    /// Достаёт запись и помечает её свежей. Считает попадание или промах.
    pub(crate) fn get(&mut self, key: &K) -> Option<Arc<V>> {
        self.clock = self.clock.wrapping_add(1);
        let Some(slot) = self.slots.get_mut(key) else {
            self.statistics.misses += 1;
            return None;
        };
        self.recency.remove(&slot.last_used);
        slot.last_used = self.clock;
        self.recency.insert(self.clock, key.clone());
        self.statistics.hits += 1;
        Some(Arc::clone(&slot.value))
    }

    /// Кладёт запись размером `bytes`, вытесняя давние. Запись больше лимита не принимается
    /// (счётчик `rejected_oversized`).
    ///
    /// Следствие для кэша реплик: реплика, чьи маски больше всего лимита, не кэшируется и при
    /// каждой пересборке кадра рисуется заново с новыми буферами (`OverlayChange::Replaced`).
    /// Это сознательное поведение: лимит памяти важнее скорости для аномально больших реплик.
    /// Их маски живут только в текущем кадре (см. `RasterStatistics::current_frame_bytes`).
    pub(crate) fn insert(&mut self, key: K, value: Arc<V>, bytes: usize) {
        if bytes > self.limit_bytes {
            self.statistics.rejected_oversized += 1;
            return;
        }
        // Повторная вставка под тем же ключом заменяет старую запись.
        self.remove(&key);
        while self.statistics.current_bytes + bytes > self.limit_bytes {
            if !self.evict_oldest() {
                break;
            }
        }
        self.clock = self.clock.wrapping_add(1);
        self.recency.insert(self.clock, key.clone());
        self.slots.insert(
            key,
            Slot {
                value,
                bytes,
                last_used: self.clock,
            },
        );
        self.statistics.current_bytes += bytes;
        self.statistics.entries = self.slots.len();
    }

    /// Забывает все записи (счётчики сохраняются).
    pub(crate) fn clear(&mut self) {
        self.slots.clear();
        self.recency.clear();
        self.statistics.current_bytes = 0;
        self.statistics.entries = 0;
    }

    /// Счётчики кэша.
    pub(crate) fn statistics(&self) -> CacheStatistics {
        self.statistics
    }

    /// Убирает запись по ключу без учёта в вытеснениях.
    fn remove(&mut self, key: &K) {
        if let Some(slot) = self.slots.remove(key) {
            self.recency.remove(&slot.last_used);
            self.statistics.current_bytes -= slot.bytes;
            self.statistics.entries = self.slots.len();
        }
    }

    /// Вытесняет самую давнюю запись; `false` — кэш пуст.
    fn evict_oldest(&mut self) -> bool {
        let Some((_, key)) = self.recency.pop_first() else {
            return false;
        };
        if let Some(slot) = self.slots.remove(&key) {
            self.statistics.current_bytes -= slot.bytes;
            self.statistics.evictions += 1;
        }
        self.statistics.entries = self.slots.len();
        true
    }
}

#[cfg(test)]
mod tests;
