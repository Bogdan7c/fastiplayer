//! Ограниченный LRU-кэш импортов DMA-BUF по паспорту буфера (чистая логика, без GL).
//!
//! Кэш generic по хранимому значению (в видеослое это две GL-текстуры): он ничего не знает о GL
//! и не удаляет значения сам. Каждая операция, которая убирает записи, ВОЗВРАЩАЕТ их владельцу,
//! а тот удаляет GL-объекты (там, где есть контекст). Так инварианты проверяются hermetic-тестами.
//!
//! Инварианты (те же, что у кэша импортов Vulkan-пути wgpu):
//! - записей не больше [`DmaBufImportCacheCapacity`];
//! - смена сигнатуры формата уничтожает ВСЕ старые записи ДО вставки первой новой: память двух
//!   наборов буферов не держится одновременно;
//! - тот же `resource_id` с другим паспортом (производитель пересоздал буфер) заменяет старую
//!   запись, а не живёт рядом с ней;
//! - при полном кэше вытесняется запись, к которой дольше всех не обращались.

use crate::dma_buf_frame::DmaBufBufferIdentity;

/// Ёмкость кэша: не меньше одной записи.
///
/// Должна совпадать с числом поверхностей пула декодера: при меньшем кэше LRU на круговом пуле
/// промахивается всегда, при большем — держит текстуры буферов, которых у декодера уже нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaBufImportCacheCapacity(usize);

impl DmaBufImportCacheCapacity {
    /// Ёмкость, равная числу слотов пула поверхностей декодера (ноль поднимается до одного).
    #[must_use]
    pub const fn matching_decoder_surface_pool(slots: usize) -> Self {
        Self(if slots == 0 { 1 } else { slots })
    }

    /// Ёмкость по умолчанию, пока вызывающий не знает размер пула декодера. Типичный пул
    /// поверхностей VA-API — десяток слотов; точное число вызывающий задаёт через
    /// [`crate::VideoLayerRenderer::set_dma_buf_import_capacity`].
    pub const DEFAULT: Self = Self(8);

    /// Число записей.
    #[must_use]
    pub const fn entries(self) -> usize {
        self.0
    }
}

impl Default for DmaBufImportCacheCapacity {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Снимок состояния и счётчиков кэша для диагностики и тестов.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DmaBufImportCacheSnapshot {
    /// Записей сейчас.
    pub entries: usize,
    /// Верхняя граница записей.
    pub capacity: usize,
    /// Показов из кэша без импорта.
    pub hits: u64,
    /// Выполненных импортов (записей вставлено).
    pub imports: u64,
    /// Импортов, закончившихся ошибкой.
    pub import_failures: u64,
    /// Записей заменено: тот же `resource_id`, другой паспорт.
    pub replaced_same_resource: u64,
    /// Записей уничтожено сменой формата потока.
    pub evicted_format_change: u64,
    /// Записей вытеснено по ёмкости (LRU), в том числе уменьшением ёмкости.
    pub evicted_capacity: u64,
    /// Записей убрано явной очисткой (смена декодера, удаление слоя).
    pub purged: u64,
}

/// Запись кэша.
#[derive(Debug)]
struct CacheEntry<T> {
    identity: DmaBufBufferIdentity,
    value: T,
    /// Монотонная метка последнего обращения (для LRU).
    last_used_tick: u64,
}

/// LRU-кэш импортов.
#[derive(Debug)]
pub(crate) struct DmaBufImportCache<T> {
    /// Записи без порядка; поиск линейный (записей — единицы или десятки).
    entries: Vec<CacheEntry<T>>,
    capacity: usize,
    next_tick: u64,
    counters: DmaBufImportCacheSnapshot,
}

impl<T> DmaBufImportCache<T> {
    /// Пустой кэш.
    pub(crate) fn new(capacity: DmaBufImportCacheCapacity) -> Self {
        Self {
            entries: Vec::with_capacity(capacity.entries()),
            capacity: capacity.entries(),
            next_tick: 0,
            counters: DmaBufImportCacheSnapshot {
                capacity: capacity.entries(),
                ..DmaBufImportCacheSnapshot::default()
            },
        }
    }

    /// Найти запись для показа: попадание считается и освежает LRU-метку.
    pub(crate) fn lookup(&mut self, identity: &DmaBufBufferIdentity) -> Option<&T> {
        let tick = self.advance_tick();
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.identity == *identity)?;
        entry.last_used_tick = tick;
        self.counters.hits += 1;
        Some(&entry.value)
    }

    /// Запись без учёта обращения: для рисования текущего кадра (не попадание и не свежесть).
    pub(crate) fn peek(&self, identity: &DmaBufBufferIdentity) -> Option<&T> {
        self.entries
            .iter()
            .find(|entry| entry.identity == *identity)
            .map(|entry| &entry.value)
    }

    /// Освежить LRU-метку без подсчёта попадания (импорт уже лежащего в кэше буфера).
    pub(crate) fn touch(&mut self, identity: &DmaBufBufferIdentity) {
        let tick = self.advance_tick();
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.identity == *identity)
        {
            entry.last_used_tick = tick;
        }
    }

    /// Освободить место под будущий импорт `incoming` ДО самого импорта; возвращает убранные
    /// значения владельцу на удаление. Порядок: другой формат → тот же `resource_id` → LRU.
    pub(crate) fn prepare_for_insert(&mut self, incoming: &DmaBufBufferIdentity) -> Vec<T> {
        let mut removed = Vec::new();
        self.take_where(
            |entry| entry.identity.format != incoming.format,
            &mut removed,
            |counters, count| counters.evicted_format_change += count,
        );
        self.take_where(
            |entry| entry.identity.resource_id == incoming.resource_id,
            &mut removed,
            |counters, count| counters.replaced_same_resource += count,
        );
        self.evict_least_recently_used_until(self.capacity.saturating_sub(1), &mut removed);
        removed
    }

    /// Вставить свежеимпортированное значение. Повторяет подготовку места (идемпотентно), чтобы
    /// инварианты держались и без явного `prepare_for_insert`; убранные значения возвращает.
    pub(crate) fn insert(&mut self, identity: DmaBufBufferIdentity, value: T) -> Vec<T> {
        let removed = self.prepare_for_insert(&identity);
        let tick = self.advance_tick();
        self.entries.push(CacheEntry {
            identity,
            value,
            last_used_tick: tick,
        });
        self.counters.imports += 1;
        removed
    }

    /// Импорт закончился ошибкой.
    pub(crate) fn record_import_failure(&mut self) {
        self.counters.import_failures += 1;
    }

    /// Новая ёмкость; лишние записи (давние первыми) возвращаются владельцу.
    pub(crate) fn set_capacity(&mut self, capacity: DmaBufImportCacheCapacity) -> Vec<T> {
        self.capacity = capacity.entries();
        self.counters.capacity = self.capacity;
        let mut removed = Vec::new();
        self.evict_least_recently_used_until(self.capacity, &mut removed);
        removed
    }

    /// Убрать все записи (смена декодера, удаление слоя).
    pub(crate) fn drain_all(&mut self) -> Vec<T> {
        self.counters.purged += u64::try_from(self.entries.len()).unwrap_or(u64::MAX);
        self.entries.drain(..).map(|entry| entry.value).collect()
    }

    /// Снимок.
    pub(crate) fn snapshot(&self) -> DmaBufImportCacheSnapshot {
        DmaBufImportCacheSnapshot {
            entries: self.entries.len(),
            ..self.counters
        }
    }

    /// Есть ли запись с этим паспортом.
    pub(crate) fn contains(&self, identity: &DmaBufBufferIdentity) -> bool {
        self.entries.iter().any(|entry| entry.identity == *identity)
    }

    /// Убрать записи, подходящие под `matches`, добавить их значения в `removed` и записать
    /// их число в счётчик, который выбирает `count_into`.
    fn take_where(
        &mut self,
        matches: impl Fn(&CacheEntry<T>) -> bool,
        removed: &mut Vec<T>,
        count_into: impl FnOnce(&mut DmaBufImportCacheSnapshot, u64),
    ) {
        let (taken, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|entry| matches(entry));
        self.entries = kept;
        count_into(
            &mut self.counters,
            u64::try_from(taken.len()).unwrap_or(u64::MAX),
        );
        removed.extend(taken.into_iter().map(|entry| entry.value));
    }

    /// Вытеснять давнюю запись, пока записей больше `limit`.
    fn evict_least_recently_used_until(&mut self, limit: usize, removed: &mut Vec<T>) {
        while self.entries.len() > limit {
            let Some(oldest_index) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.last_used_tick)
                .map(|(index, _)| index)
            else {
                return;
            };
            removed.push(self.entries.swap_remove(oldest_index).value);
            self.counters.evicted_capacity += 1;
        }
    }

    fn advance_tick(&mut self) -> u64 {
        let tick = self.next_tick;
        self.next_tick = self.next_tick.wrapping_add(1);
        tick
    }
}

#[cfg(test)]
mod tests;
