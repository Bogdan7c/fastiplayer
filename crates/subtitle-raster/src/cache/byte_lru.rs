//! LRU-кэш с лимитом в байтах.

use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::Arc;

use super::fast_hash::FastBuildHasher;

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

/// Запись кэша: ключ (нужен при вытеснении), значение, учтённый размер и отметка последнего
/// использования.
struct Slot<K, V> {
    key: K,
    value: Arc<V>,
    bytes: usize,
    last_used: u64,
}

/// Элемент очереди порядка использования: отметка и номер ячейки в `slots`.
///
/// Очередь «ленивая»: при каждом использовании записи в хвост добавляется новый элемент, а
/// старый остаётся и считается устаревшим, когда его отметка не совпадает с `last_used` ячейки
/// (или ячейка пуста). Так использование записи стоит одной вставки в конец очереди, без
/// удаления из середины упорядоченного дерева и без клонирования ключа.
#[derive(Clone, Copy)]
struct Ticket {
    stamp: u64,
    slot: usize,
}

/// Кэш «ключ -> `Arc<V>`», вытесняющий давно не использованное, когда сумма размеров
/// превышает лимит.
///
/// Учёт памяти: кэш верит `bytes`, который передал вызывающий, и сам ничего не меряет. Ключ
/// хранится дважды (в индексе и в ячейке записи, `K: Clone`), поэтому вызывающий обязан
/// включать в `bytes` память ключа ПО ЧИСЛУ КОПИЙ: у ключей с владеющими буферами (`String`,
/// `Vec`) это удвоенный размер буфера, у ключей с общим `Arc` - один раз (клон не копирует
/// данные). Иначе лимит занижает реальное потребление.
///
/// Инварианты (для обычной вставки [`Self::insert`]; отложенная вставка
/// [`Self::insert_deferred`] нарушает первый пункт до вызова [`Self::cut`]):
/// - `current_bytes <= limit_bytes` ВСЕГДА (после любой операции);
/// - запись, которая одна больше лимита, не кэшируется (счётчик `rejected_oversized`):
///   вызывающий всё равно получает своё значение и рисует его, но повторно оно будет
///   растеризовано заново. Это осознанный компромисс: иначе одна огромная реплика вытеснила
///   бы весь кэш и всё равно превысила лимит;
/// - у каждой живой записи в одной из очередей есть элемент с её актуальной отметкой, и
///   отметки уникальны: обычные растут от [`WARM_CLOCK_START`], «испытуемые» - от нуля.
pub(crate) struct ByteLruCache<K, V> {
    limit_bytes: usize,
    /// Ключ -> номер ячейки в `slots`.
    index: HashMap<K, usize, FastBuildHasher>,
    /// Ячейки записей; пустые (`None`) перечислены в `free_slots`.
    slots: Vec<Option<Slot<K, V>>>,
    free_slots: Vec<usize>,
    /// Очередь «испытуемых» записей: самая давняя - первая.
    probation: VecDeque<Ticket>,
    /// Очередь обычных записей: самая давняя - первая.
    warm: VecDeque<Ticket>,
    /// Часы обычных записей. Начинаются с середины диапазона, чтобы записи «на испытании»
    /// (часы `probation_clock` от нуля) всегда считались старше любой обычной.
    clock: u64,
    /// Часы записей «на испытании» ([`ByteLruCache::insert_on_probation`]).
    probation_clock: u64,
    statistics: CacheStatistics,
}

/// Начальное значение часов обычных записей: все записи «на испытании» лежат ниже.
const WARM_CLOCK_START: u64 = 1 << 63;

/// Во сколько раз очередь может превысить число записей, прежде чем из неё уберут устаревшие
/// элементы (амортизированно O(1) на использование).
const QUEUE_SLACK_FACTOR: usize = 2;
/// Запас длины очереди, при котором уборку не делают (чтобы не убирать из крошечных кэшей).
const QUEUE_SLACK_BASE: usize = 64;

impl<K: Hash + Eq + Clone, V> ByteLruCache<K, V> {
    /// Пустой кэш с лимитом `limit_bytes` (0 — ничего не кэшируется).
    pub(crate) fn new(limit_bytes: usize) -> Self {
        Self {
            limit_bytes,
            index: HashMap::default(),
            slots: Vec::new(),
            free_slots: Vec::new(),
            probation: VecDeque::new(),
            warm: VecDeque::new(),
            clock: WARM_CLOCK_START,
            probation_clock: 0,
            statistics: CacheStatistics::default(),
        }
    }

    /// Достаёт запись и помечает её свежей. Считает попадание или промах.
    ///
    /// Ключ можно передать в заимствованной форме (`Q`, на которую `K` реализует `Borrow`):
    /// так поиск не строит владеющий ключ (например, `String`) на каждое обращение.
    pub(crate) fn get<Q>(&mut self, key: &Q) -> Option<Arc<V>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.clock = self.clock.wrapping_add(1);
        let slot_index = self.index.get(key).copied();
        let slot = slot_index.and_then(|index| self.slots.get_mut(index)?.as_mut());
        let (Some(slot_index), Some(slot)) = (slot_index, slot) else {
            self.statistics.misses += 1;
            return None;
        };
        slot.last_used = self.clock;
        let value = Arc::clone(&slot.value);
        self.warm.push_back(Ticket {
            stamp: self.clock,
            slot: slot_index,
        });
        self.statistics.hits += 1;
        self.compact_queues_if_bloated();
        Some(value)
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
        let stamp = self.next_warm_stamp();
        self.store(key, value, bytes, stamp);
    }

    /// Кладёт запись «на испытание»: как [`Self::insert`], но запись считается самой старой и
    /// вытесняется раньше любой обычной (свою очередь среди «испытуемых» она держит по порядку
    /// вставки). Первое же попадание ([`Self::get`]) превращает её в обычную.
    ///
    /// Для записей, о повторном использовании которых заранее ничего не известно (реплика с
    /// мгновенным состоянием анимации): поток таких записей не вытесняет проверенные.
    pub(crate) fn insert_on_probation(&mut self, key: K, value: Arc<V>, bytes: usize) {
        if bytes > self.limit_bytes {
            self.statistics.rejected_oversized += 1;
            return;
        }
        self.remove(&key);
        while self.statistics.current_bytes + bytes > self.limit_bytes {
            if !self.evict_oldest() {
                break;
            }
        }
        self.probation_clock += 1;
        let stamp = self.probation_clock;
        self.store(key, value, bytes, stamp);
    }

    /// Кладёт запись БЕЗ вытеснения и без проверки размера: кэш может временно перерасти лимит.
    ///
    /// Для кэшей масок в стиле libass: вытеснение делает [`Self::cut`] в начале следующего
    /// кадра, а внутри кадра ничего не вытесняется (иначе рабочий набор одного кадра мог бы
    /// вытеснять сам себя). Повторная вставка под тем же ключом заменяет запись.
    pub(crate) fn insert_deferred(&mut self, key: K, value: Arc<V>, bytes: usize) {
        self.remove(&key);
        let stamp = self.next_warm_stamp();
        self.store(key, value, bytes, stamp);
    }

    /// Вытесняет самые давние записи, пока сумма размеров не станет не больше лимита
    /// (libass: `ass_cache_cut`). Парная операция к [`Self::insert_deferred`].
    pub(crate) fn cut(&mut self) {
        while self.statistics.current_bytes > self.limit_bytes {
            if !self.evict_oldest() {
                break;
            }
        }
    }

    /// Забывает все записи (счётчики сохраняются).
    pub(crate) fn clear(&mut self) {
        self.index.clear();
        self.slots.clear();
        self.free_slots.clear();
        self.probation.clear();
        self.warm.clear();
        self.statistics.current_bytes = 0;
        self.statistics.entries = 0;
    }

    /// Счётчики кэша.
    pub(crate) fn statistics(&self) -> CacheStatistics {
        self.statistics
    }

    /// Следующая отметка обычной записи.
    fn next_warm_stamp(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    /// Кладёт запись с отметкой `stamp` и учитывает её размер (ключа в кэше ещё нет).
    fn store(&mut self, key: K, value: Arc<V>, bytes: usize, stamp: u64) {
        let slot = Slot {
            key: key.clone(),
            value,
            bytes,
            last_used: stamp,
        };
        let slot_index = match self.free_slots.pop() {
            Some(free) => {
                if let Some(cell) = self.slots.get_mut(free) {
                    *cell = Some(slot);
                }
                free
            }
            None => {
                self.slots.push(Some(slot));
                self.slots.len() - 1
            }
        };
        let queue = if stamp < WARM_CLOCK_START {
            &mut self.probation
        } else {
            &mut self.warm
        };
        queue.push_back(Ticket {
            stamp,
            slot: slot_index,
        });
        self.index.insert(key, slot_index);
        self.statistics.current_bytes += bytes;
        self.statistics.entries = self.index.len();
        self.compact_queues_if_bloated();
    }

    /// Убирает запись по ключу без учёта в вытеснениях.
    fn remove(&mut self, key: &K) {
        if let Some(slot_index) = self.index.remove(key) {
            self.release_slot(slot_index);
            self.statistics.entries = self.index.len();
        }
    }

    /// Освобождает ячейку записи, уже убранной из `index`, и снимает её размер с учёта.
    fn release_slot(&mut self, slot_index: usize) -> Option<Slot<K, V>> {
        let slot = self.slots.get_mut(slot_index)?.take()?;
        self.free_slots.push(slot_index);
        self.statistics.current_bytes -= slot.bytes;
        Some(slot)
    }

    /// Элемент очереди актуален, если ячейка жива и хранит ту же отметку.
    fn ticket_is_current(slots: &[Option<Slot<K, V>>], ticket: Ticket) -> bool {
        slots
            .get(ticket.slot)
            .and_then(Option::as_ref)
            .is_some_and(|slot| slot.last_used == ticket.stamp)
    }

    /// Вытесняет самую давнюю запись; `false` — кэш пуст.
    fn evict_oldest(&mut self) -> bool {
        loop {
            // «Испытуемые» всегда старше обычных (их отметки ниже `WARM_CLOCK_START`).
            let ticket = match self.probation.pop_front() {
                Some(ticket) => ticket,
                None => match self.warm.pop_front() {
                    Some(ticket) => ticket,
                    None => return false,
                },
            };
            if !Self::ticket_is_current(&self.slots, ticket) {
                continue;
            }
            if let Some(slot) = self.release_slot(ticket.slot) {
                self.index.remove(&slot.key);
                self.statistics.evictions += 1;
            }
            self.statistics.entries = self.index.len();
            return true;
        }
    }

    /// Убирает из очередей устаревшие элементы, когда их накопилось слишком много.
    fn compact_queues_if_bloated(&mut self) {
        let allowed = self.index.len() * QUEUE_SLACK_FACTOR + QUEUE_SLACK_BASE;
        if self.warm.len() <= allowed && self.probation.len() <= allowed {
            return;
        }
        let slots = &self.slots;
        self.warm
            .retain(|ticket| Self::ticket_is_current(slots, *ticket));
        self.probation
            .retain(|ticket| Self::ticket_is_current(slots, *ticket));
    }
}

#[cfg(test)]
mod tests;
