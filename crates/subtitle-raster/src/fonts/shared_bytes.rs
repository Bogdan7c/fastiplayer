//! Разделяемые байты файлов системных шрифтов с «слабым» владением.
//!
//! Проблема, которую решает модуль: файл шрифта (особенно `.ttc` с десятками лиц и CJK-шрифты
//! на десятки мегабайт) нельзя читать заново на каждое лицо и нельзя держать в памяти вечно.
//! Поэтому библиотека хранит здесь только [`Weak`], а сильные ссылки ([`Arc`]) держат те, кому
//! байты реально нужны: подготовленные лица `FaceCache`. Вытеснили лицо из `FaceCache` — память
//! файла освободилась (если его не держит другое лицо того же файла); понадобился снова —
//! файл прочитается заново.
//!
//! Ключ — идентификатор файла (для системных шрифтов путь из `fontdb`), общий для всех лиц
//! одной коллекции `.ttc`: все они получают один и тот же `Arc`.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, PoisonError, Weak};

/// Кэш «файл -> слабая ссылка на его байты».
#[derive(Debug)]
pub(super) struct SharedBytesCache<Key> {
    entries: Mutex<HashMap<Key, Weak<[u8]>>>,
}

impl<Key> Default for SharedBytesCache<Key> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl<Key: Eq + Hash> SharedBytesCache<Key> {
    /// Живые байты файла `key`; если их никто не держит, вызывает `load` (чтение с диска).
    ///
    /// `None` — загрузить не удалось (причину пишет `load`); повторять попытку при следующем
    /// обращении — осознанно: файл мог появиться снова.
    pub(super) fn get_or_load(
        &self,
        key: Key,
        load: impl FnOnce() -> Option<Arc<[u8]>>,
    ) -> Option<Arc<[u8]>> {
        // Блокировку держим и на время загрузки: так два обращения к одному файлу не прочитают
        // его дважды. Загрузчик не берёт эту блокировку повторно.
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(alive) = entries.get(&key).and_then(Weak::upgrade) {
            return Some(alive);
        }
        let loaded = load()?;
        // Чистим умершие записи при каждой вставке, чтобы таблица не росла за счёт файлов,
        // которые давно вытеснены.
        entries.retain(|_, weak| weak.strong_count() > 0);
        entries.insert(key, Arc::downgrade(&loaded));
        Some(loaded)
    }

    /// Сколько файлов и байт сейчас реально удерживается в памяти кем-то снаружи.
    pub(super) fn resident(&self) -> ResidentBytes {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .values()
            .filter_map(Weak::upgrade)
            .fold(ResidentBytes::default(), |total, bytes| ResidentBytes {
                files: total.files + 1,
                bytes: total.bytes.saturating_add(bytes.len()),
            })
    }
}

/// Сколько памяти занимают сейчас живые байты файлов.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ResidentBytes {
    /// Число файлов.
    pub(super) files: usize,
    /// Суммарный размер.
    pub(super) bytes: usize,
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn bytes_of(value: u8, len: usize) -> Arc<[u8]> {
        Arc::from(vec![value; len])
    }

    #[test]
    fn same_key_shares_one_allocation_while_someone_holds_it() {
        let cache = SharedBytesCache::<&str>::default();
        let loads = Cell::new(0);
        let load = || {
            loads.set(loads.get() + 1);
            Some(bytes_of(7, 100))
        };
        let first = cache.get_or_load("a.ttc", load).expect("loaded");
        let second = cache.get_or_load("a.ttc", load).expect("shared");
        assert!(
            Arc::ptr_eq(&first, &second),
            "two faces of one file share bytes"
        );
        assert_eq!(loads.get(), 1, "the file is read once");
        assert_eq!(
            cache.resident(),
            ResidentBytes {
                files: 1,
                bytes: 100
            }
        );
    }

    #[test]
    fn memory_is_released_when_the_last_strong_holder_drops_it() {
        let cache = SharedBytesCache::<&str>::default();
        let holder = cache
            .get_or_load("a", || Some(bytes_of(1, 64)))
            .expect("loaded");
        assert_eq!(cache.resident().files, 1);
        drop(holder);
        assert_eq!(
            cache.resident(),
            ResidentBytes::default(),
            "the library keeps no strong ref"
        );
    }

    #[test]
    fn evicted_file_is_read_again_on_next_use() {
        let cache = SharedBytesCache::<&str>::default();
        let loads = Cell::new(0);
        let load = || {
            loads.set(loads.get() + 1);
            Some(bytes_of(3, 8))
        };
        drop(cache.get_or_load("a", load));
        let again = cache.get_or_load("a", load).expect("reloaded");
        assert_eq!(loads.get(), 2);
        assert_eq!(again.len(), 8);
    }

    #[test]
    fn failed_load_is_reported_and_not_remembered() {
        let cache = SharedBytesCache::<&str>::default();
        assert!(cache.get_or_load("gone", || None).is_none());
        let recovered = cache.get_or_load("gone", || Some(bytes_of(9, 4)));
        assert_eq!(
            recovered.map(|bytes| bytes.len()),
            Some(4),
            "a later attempt may succeed"
        );
    }

    #[test]
    fn different_files_do_not_share_and_dead_entries_are_pruned() {
        let cache = SharedBytesCache::<u32>::default();
        let first = cache.get_or_load(1, || Some(bytes_of(1, 10))).expect("one");
        drop(cache.get_or_load(2, || Some(bytes_of(2, 20))));
        let third = cache
            .get_or_load(3, || Some(bytes_of(3, 30)))
            .expect("three");
        assert!(!Arc::ptr_eq(&first, &third));
        assert_eq!(
            cache.resident(),
            ResidentBytes {
                files: 2,
                bytes: 40
            }
        );
        let internal = cache.entries.lock().expect("lock").len();
        assert_eq!(internal, 2, "the dead entry of file 2 was pruned on insert");
    }
}
