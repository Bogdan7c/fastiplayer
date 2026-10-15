//! Ограниченный кэш «(основное лицо, символ) -> результат поиска глифа».

use std::collections::{HashMap, VecDeque};

use skrifa::GlyphId;

use super::face::FaceId;

/// Лимит кэша поиска символов. Значение задаёт вызывающий (из конфига), `Default` нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontLookupCacheLimits {
    /// Максимум запомненных пар (лицо, символ); 0 отключает кэш.
    pub max_cached_chars: usize,
}

/// Запомненный результат поиска.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CachedLookup {
    /// Глиф найден в `face`.
    Found { face: FaceId, glyph: GlyphId },
    /// Нигде нет (отрицательный результат тоже кэшируется: полный перебор системных шрифтов дорог).
    Missing,
}

/// Кэш с вытеснением самых старых записей (FIFO): без дорогой учётной информации на hit.
#[derive(Debug)]
pub(super) struct LookupCache {
    limits: FontLookupCacheLimits,
    entries: HashMap<(FaceId, char), CachedLookup>,
    insertion_order: VecDeque<(FaceId, char)>,
}

impl LookupCache {
    pub(super) fn new(limits: FontLookupCacheLimits) -> Self {
        Self {
            limits,
            entries: HashMap::new(),
            insertion_order: VecDeque::new(),
        }
    }

    pub(super) fn get(&self, primary: FaceId, ch: char) -> Option<CachedLookup> {
        self.entries.get(&(primary, ch)).copied()
    }

    /// Запоминает результат; при переполнении вытесняет самые старые записи.
    pub(super) fn insert(&mut self, primary: FaceId, ch: char, lookup: CachedLookup) {
        if self.limits.max_cached_chars == 0 {
            return;
        }
        let key = (primary, ch);
        if self.entries.insert(key, lookup).is_none() {
            self.insertion_order.push_back(key);
        }
        while self.entries.len() > self.limits.max_cached_chars {
            let Some(oldest) = self.insertion_order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    /// Забывает все записи: набор шрифтов изменился, и прежние «нигде нет» могли устареть.
    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.insertion_order.clear();
    }

    /// Забывает записи, указывающие на `face` (лицо признано непригодным): следующий поиск
    /// символа найдёт другое лицо или `.notdef`. Остальные записи остаются.
    pub(super) fn forget_face(&mut self, face: FaceId) {
        self.entries.retain(|_, lookup| {
            !matches!(lookup, CachedLookup::Found { face: found, .. } if *found == face)
        });
        let entries = &self.entries;
        self.insertion_order.retain(|key| entries.contains_key(key));
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(face: u32) -> CachedLookup {
        CachedLookup::Found {
            face: FaceId(face),
            glyph: GlyphId::new(7),
        }
    }

    #[test]
    fn eviction_keeps_size_within_limit_and_drops_oldest_first() {
        let mut cache = LookupCache::new(FontLookupCacheLimits {
            max_cached_chars: 2,
        });
        cache.insert(FaceId(0), 'a', found(0));
        cache.insert(FaceId(0), 'b', found(1));
        cache.insert(FaceId(0), 'c', found(2));
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(FaceId(0), 'a'), None);
        assert_eq!(cache.get(FaceId(0), 'b'), Some(found(1)));
        assert_eq!(cache.get(FaceId(0), 'c'), Some(found(2)));
    }

    #[test]
    fn reinserting_a_key_does_not_grow_the_cache() {
        let mut cache = LookupCache::new(FontLookupCacheLimits {
            max_cached_chars: 2,
        });
        cache.insert(FaceId(0), 'a', found(0));
        cache.insert(FaceId(0), 'a', CachedLookup::Missing);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(FaceId(0), 'a'), Some(CachedLookup::Missing));
    }

    #[test]
    fn same_char_for_different_primary_faces_is_cached_separately() {
        let mut cache = LookupCache::new(FontLookupCacheLimits {
            max_cached_chars: 4,
        });
        cache.insert(FaceId(0), 'a', found(0));
        cache.insert(FaceId(1), 'a', CachedLookup::Missing);
        assert_eq!(cache.get(FaceId(0), 'a'), Some(found(0)));
        assert_eq!(cache.get(FaceId(1), 'a'), Some(CachedLookup::Missing));
    }

    #[test]
    fn forgetting_a_face_drops_only_entries_that_point_to_it() {
        let mut cache = LookupCache::new(FontLookupCacheLimits {
            max_cached_chars: 8,
        });
        cache.insert(FaceId(0), 'a', found(5));
        cache.insert(FaceId(0), 'b', found(6));
        cache.insert(FaceId(0), 'c', CachedLookup::Missing);
        cache.forget_face(FaceId(5));
        assert_eq!(cache.get(FaceId(0), 'a'), None);
        assert_eq!(cache.get(FaceId(0), 'b'), Some(found(6)));
        assert_eq!(cache.get(FaceId(0), 'c'), Some(CachedLookup::Missing));
        assert_eq!(cache.len(), 2);
        // Порядок вытеснения остался согласован с таблицей: переполнение не теряет записи.
        cache.insert(FaceId(0), 'd', found(1));
        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn zero_limit_disables_caching() {
        let mut cache = LookupCache::new(FontLookupCacheLimits {
            max_cached_chars: 0,
        });
        cache.insert(FaceId(0), 'a', found(0));
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.get(FaceId(0), 'a'), None);
    }
}
