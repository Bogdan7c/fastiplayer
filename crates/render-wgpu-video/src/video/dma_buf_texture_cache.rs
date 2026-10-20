//! Ограниченный кэш импортированных DMA-BUF image по стабильной идентичности буфера.
//!
//! До cpu-parity сессии 03 кэш ключевался по `FrameResourceHandle`, который
//! уникален на каждый кадр, поэтому промахивался всегда и рендерер делал полный
//! Vulkan-импорт (dup fd, `vkCreateImage`, `vkAllocateMemory`, bind, два view)
//! на каждом кадре. Теперь ключ — `DmaBufFrameIdentity`: backend surface id +
//! kernel identity объектов + layout. Пока декодер переиспользует ту же
//! поверхность из своего пула, рендерер переиспользует тот же image.
//!
//! Модуль чистый и generic по типу image: он не знает о Vulkan и не делает
//! импорт, поэтому его инварианты проверяются hermetic-тестами без GPU.
//!
//! Инварианты:
//! - число записей никогда не превышает `capacity` (= ёмкость пула декодера);
//! - при смене формата потока (другая сигнатура: fourcc/layout/размер/modifier)
//!   все записи старого формата уничтожаются ДО вставки первой записи нового,
//!   чтобы GTT/RSS не держали два набора буферов;
//! - тот же surface id с другой идентичностью (декодер пересоздал буфер под тем
//!   же номером) заменяет старую запись, а не живёт рядом с ней;
//! - при полном кэше вытесняется запись, которую дольше всех не запрашивали.

use std::sync::Arc;

use video_core::{DmaBufFrameIdentity, FrameResourceHandle};

/// Когда запись кэша считается попаданием.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DmaBufTextureReusePolicy {
    /// Один буфер → один image на любое число кадров (есть явная синхронизация).
    AcrossFrames,

    /// Fail-closed: image отдаётся повторно только для того же кадра
    /// (`FrameResourceHandle`), как при покадровом импорте до сессии 03.
    SameFrameHandleOnly,
}

/// Ёмкость кэша импортов. Должна совпадать с ёмкостью пула поверхностей
/// декодера: при меньшем кэше LRU на круговом пуле промахивается всегда, при
/// большем кэш держит image буферов, которых у декодера уже нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaBufImportCacheCapacity(usize);

impl DmaBufImportCacheCapacity {
    /// Ёмкость, равная числу слотов zero-copy пула декодера (ноль поднимается до одного).
    #[must_use]
    pub const fn matching_surface_pool(surface_pool_slots: usize) -> Self {
        Self(if surface_pool_slots == 0 {
            1
        } else {
            surface_pool_slots
        })
    }

    /// Число записей.
    #[must_use]
    pub const fn entries(self) -> usize {
        self.0
    }
}

/// Снимок счётчиков кэша для диагностики и тестов.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DmaBufImportCacheSnapshot {
    /// Сколько image сейчас в кэше.
    pub entries: usize,

    /// Верхняя граница записей.
    pub capacity: usize,

    /// Сколько lookup-ов нашли готовый image (промахи = число импортов у materializer-а).
    pub hits: u64,

    /// Сколько записей заменено из-за новой идентичности под тем же surface id.
    pub replaced_same_resource: u64,

    /// Сколько записей уничтожено из-за смены формата потока.
    pub evicted_format_change: u64,

    /// Сколько записей вытеснено по ёмкости (LRU).
    pub evicted_capacity: u64,
}

/// Снимок кэша и счётчиков импорта DMA-BUF materializer-а для диагностики.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DmaBufImportStats {
    /// Состояние кэша по идентичности буфера.
    pub cache: DmaBufImportCacheSnapshot,

    /// Сколько раз реально выполнен Vulkan-импорт (создание image/memory/views).
    pub imports_performed: u64,

    /// Сколько импортов завершились ошибкой.
    pub import_failures: u64,

    /// `true`, если image переиспользуется между кадрами (есть явная синхронизация).
    pub image_reuse_enabled: bool,
}

/// Одна запись кэша.
struct CacheEntry<T> {
    /// Ключ: стабильная идентичность буфера.
    identity: Arc<DmaBufFrameIdentity>,

    /// Кадр, для которого выполнен импорт; нужен политике `SameFrameHandleOnly`.
    handle: FrameResourceHandle,

    /// Импортированный image; `Arc`, потому что views держат его живым дольше записи.
    texture: Arc<T>,

    /// Монотонная метка последнего обращения для LRU.
    last_used_tick: u64,
}

/// Ограниченный LRU-кэш импортов, generic по типу image.
pub(super) struct DmaBufTextureCache<T> {
    /// Записи без особого порядка; поиск линейный, записей ≤ 24.
    entries: Vec<CacheEntry<T>>,

    /// Верхняя граница записей.
    capacity: usize,

    /// Монотонный счётчик обращений для LRU.
    next_tick: u64,

    /// Накопленные счётчики для диагностики.
    counters: DmaBufImportCacheSnapshot,
}

impl<T> DmaBufTextureCache<T> {
    /// Создаёт пустой кэш с заданной ёмкостью.
    pub(super) fn new(capacity: DmaBufImportCacheCapacity) -> Self {
        let capacity = capacity.entries();
        Self {
            entries: Vec::with_capacity(capacity),
            capacity,
            next_tick: 0,
            counters: DmaBufImportCacheSnapshot {
                capacity,
                ..DmaBufImportCacheSnapshot::default()
            },
        }
    }

    /// Ищет image по идентичности (и по handle при fail-closed политике);
    /// попадание обновляет LRU-метку.
    pub(super) fn lookup(
        &mut self,
        identity: &DmaBufFrameIdentity,
        handle: FrameResourceHandle,
        policy: DmaBufTextureReusePolicy,
    ) -> Option<Arc<T>> {
        let tick = self.advance_tick();
        let entry = self.entries.iter_mut().find(|entry| {
            entry.identity.as_ref() == identity
                && match policy {
                    DmaBufTextureReusePolicy::AcrossFrames => true,
                    DmaBufTextureReusePolicy::SameFrameHandleOnly => entry.handle == handle,
                }
        });
        let entry = entry?;
        entry.last_used_tick = tick;
        self.counters.hits = self.counters.hits.saturating_add(1);
        Some(Arc::clone(&entry.texture))
    }

    /// Освобождает место под будущую запись ДО импорта: уничтожает записи другого
    /// формата, запись того же surface id с другой identity и, при полном кэше,
    /// самую давнюю. Так старые image уничтожаются раньше, чем создаётся новый
    /// (пиковая память не держит два набора буферов).
    pub(super) fn prepare_for_insert(&mut self, incoming: &DmaBufFrameIdentity) {
        self.evict_other_formats(incoming);
        self.evict_same_resource(incoming);
        self.evict_least_recently_used_if_full();
        self.counters.entries = self.entries.len();
    }

    /// Вставляет свежеимпортированный image и возвращает shared handle на него.
    ///
    /// Повторяет `prepare_for_insert` (idempotent), чтобы инварианты держались и
    /// без предварительного вызова.
    pub(super) fn insert(
        &mut self,
        identity: Arc<DmaBufFrameIdentity>,
        handle: FrameResourceHandle,
        texture: T,
    ) -> Arc<T> {
        self.prepare_for_insert(&identity);

        let tick = self.advance_tick();
        let texture = Arc::new(texture);
        self.entries.push(CacheEntry {
            identity,
            handle,
            texture: Arc::clone(&texture),
            last_used_tick: tick,
        });
        self.counters.entries = self.entries.len();
        texture
    }

    /// Ёмкость кэша.
    pub(super) const fn capacity(&self) -> DmaBufImportCacheCapacity {
        DmaBufImportCacheCapacity(self.capacity)
    }

    /// Снимок счётчиков.
    pub(super) fn snapshot(&self) -> DmaBufImportCacheSnapshot {
        DmaBufImportCacheSnapshot {
            entries: self.entries.len(),
            ..self.counters
        }
    }

    /// Уничтожает все записи другой сигнатуры формата (смена потока/разрешения).
    fn evict_other_formats(&mut self, incoming: &DmaBufFrameIdentity) {
        let incoming_signature = incoming.format_signature();
        let before = self.entries.len();
        self.entries
            .retain(|entry| entry.identity.format_signature() == incoming_signature);
        let evicted = before - self.entries.len();
        self.counters.evicted_format_change = self
            .counters
            .evicted_format_change
            .saturating_add(evicted as u64);
    }

    /// Уничтожает запись с тем же surface id, но другой идентичностью.
    fn evict_same_resource(&mut self, incoming: &DmaBufFrameIdentity) {
        let before = self.entries.len();
        self.entries
            .retain(|entry| !entry.identity.shares_resource_id(incoming));
        let replaced = before - self.entries.len();
        self.counters.replaced_same_resource = self
            .counters
            .replaced_same_resource
            .saturating_add(replaced as u64);
    }

    /// Освобождает место под новую запись, вытесняя самую давнюю по обращению.
    fn evict_least_recently_used_if_full(&mut self) {
        while self.entries.len() >= self.capacity {
            let Some(oldest_index) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.last_used_tick)
                .map(|(index, _)| index)
            else {
                return;
            };
            self.entries.swap_remove(oldest_index);
            self.counters.evicted_capacity = self.counters.evicted_capacity.saturating_add(1);
        }
    }

    /// Выдаёт следующую LRU-метку.
    fn advance_tick(&mut self) -> u64 {
        let tick = self.next_tick;
        self.next_tick = self.next_tick.wrapping_add(1);
        tick
    }
}

#[cfg(test)]
mod tests {
    use video_core::{
        DmaBufFrameExportLayout, DmaBufLayerDescriptor, DmaBufObjectIdentity,
        DmaBufObjectIdentityDescriptor,
    };

    use super::*;

    /// Идентичность одной поверхности: surface id и inode задаются отдельно,
    /// чтобы моделировать «тот же id, другой буфер».
    fn identity(resource_id: u64, inode: u64, width: u32) -> Arc<DmaBufFrameIdentity> {
        Arc::new(DmaBufFrameIdentity {
            resource_id,
            fourcc: 0x3231_564e,
            export_layout: DmaBufFrameExportLayout::ComposedLayers,
            width,
            height: 1080,
            objects: vec![DmaBufObjectIdentityDescriptor {
                identity: DmaBufObjectIdentity {
                    device: 1,
                    inode,
                    special_device: 0,
                },
                size: 3_110_400,
                drm_format_modifier: 0x0200_0000_1040_1b04,
            }],
            layers: vec![DmaBufLayerDescriptor {
                drm_format: 0x3231_564e,
                num_planes: 2,
                object_index: [0, 0, 0, 0],
                offset: [0, 2_073_600, 0, 0],
                pitch: [width, width, 0, 0],
            }],
        })
    }

    /// Fake image: номер импорта, чтобы отличать записи.
    #[derive(Debug, PartialEq, Eq)]
    struct FakeTexture(u32);

    /// Политика по умолчанию в тестах: синхронизация доказана, кэш работает между кадрами.
    const ACROSS: DmaBufTextureReusePolicy = DmaBufTextureReusePolicy::AcrossFrames;

    /// Короткая запись per-frame handle.
    const fn frame(id: u64) -> FrameResourceHandle {
        FrameResourceHandle(id)
    }

    /// Кэш с заданной ёмкостью.
    fn cache(capacity: usize) -> DmaBufTextureCache<FakeTexture> {
        DmaBufTextureCache::new(DmaBufImportCacheCapacity::matching_surface_pool(capacity))
    }

    /// Второй кадр (новый handle) на той же поверхности (тот же id, тот же inode) —
    /// попадание, и возвращается тот же image, а не новый.
    #[test]
    fn same_surface_identity_hits_for_a_new_frame_handle_without_new_import() {
        let mut cache = cache(24);
        let surface = identity(7, 700, 1920);
        assert!(
            cache.lookup(&surface, frame(1), ACROSS).is_none(),
            "cold cache must miss"
        );
        let imported = cache.insert(Arc::clone(&surface), frame(1), FakeTexture(1));

        let hit = cache
            .lookup(&surface, frame(2), ACROSS)
            .expect("same identity must hit even for the next frame handle");

        assert!(Arc::ptr_eq(&hit, &imported));
        let snapshot = cache.snapshot();
        assert_eq!(snapshot.hits, 1);
        assert_eq!(snapshot.entries, 1);
    }

    /// Fail-closed политика: без синхронизации та же surface под новым handle —
    /// промах, а повтор того же кадра (тот же handle) — попадание.
    #[test]
    fn same_frame_handle_only_policy_hits_only_for_the_same_frame() {
        let mut cache = cache(24);
        let surface = identity(7, 700, 1920);
        let policy = DmaBufTextureReusePolicy::SameFrameHandleOnly;
        let _ = cache.insert(Arc::clone(&surface), frame(1), FakeTexture(1));

        assert!(cache.lookup(&surface, frame(1), policy).is_some());
        assert!(
            cache.lookup(&surface, frame(2), policy).is_none(),
            "a new frame on the same buffer must re-import without explicit sync"
        );
    }

    /// Тот же surface id, но декодер пересоздал буфер (другой inode): старая
    /// запись уничтожается, новая занимает её место, записей не становится две.
    #[test]
    fn same_resource_id_with_new_backing_replaces_old_entry() {
        let mut cache = cache(24);
        let old_surface = identity(7, 700, 1920);
        let new_surface = identity(7, 701, 1920);
        let _old = cache.insert(Arc::clone(&old_surface), frame(1), FakeTexture(1));

        let _new = cache.insert(Arc::clone(&new_surface), frame(2), FakeTexture(2));

        assert!(
            cache.lookup(&old_surface, frame(3), ACROSS).is_none(),
            "stale buffer must miss"
        );
        assert_eq!(
            *cache
                .lookup(&new_surface, frame(3), ACROSS)
                .expect("new buffer must hit"),
            FakeTexture(2)
        );
        let snapshot = cache.snapshot();
        assert_eq!(snapshot.entries, 1);
        assert_eq!(snapshot.replaced_same_resource, 1);
    }

    /// Смена формата потока: все записи старого формата уничтожаются до
    /// вставки первой записи нового, даже если ёмкость позволяла держать оба.
    #[test]
    fn format_change_evicts_every_old_format_entry_before_inserting_new_one() {
        let mut cache = cache(24);
        for surface_id in 0..20 {
            let _ = cache.insert(
                identity(surface_id, 100 + surface_id, 1920),
                frame(surface_id),
                FakeTexture(1),
            );
        }
        assert_eq!(cache.snapshot().entries, 20);

        let _ = cache.insert(identity(0, 500, 3840), frame(100), FakeTexture(2));

        let snapshot = cache.snapshot();
        assert_eq!(snapshot.entries, 1, "only the new-format entry may remain");
        assert_eq!(snapshot.evicted_format_change, 20);
        assert!(
            cache
                .lookup(&identity(3, 103, 1920), frame(101), ACROSS)
                .is_none()
        );
    }

    /// Ёмкость не превышается: 25-я поверхность вытесняет самую давнюю по обращению.
    #[test]
    fn capacity_is_bounded_and_evicts_least_recently_used() {
        let mut cache = cache(24);
        for surface_id in 0..24 {
            let _ = cache.insert(
                identity(surface_id, 100 + surface_id, 1920),
                frame(surface_id),
                FakeTexture(1),
            );
        }
        // Поверхность 0 только что запрашивали, поэтому она «свежая»; самая старая — 1.
        assert!(
            cache
                .lookup(&identity(0, 100, 1920), frame(50), ACROSS)
                .is_some()
        );

        let _ = cache.insert(identity(24, 124, 1920), frame(51), FakeTexture(2));

        let snapshot = cache.snapshot();
        assert_eq!(snapshot.entries, 24);
        assert_eq!(snapshot.evicted_capacity, 1);
        assert!(
            cache
                .lookup(&identity(0, 100, 1920), frame(52), ACROSS)
                .is_some(),
            "recent entry kept"
        );
        assert!(
            cache
                .lookup(&identity(1, 101, 1920), frame(53), ACROSS)
                .is_none(),
            "LRU entry evicted"
        );
    }

    /// Нулевая ёмкость не ломает кэш: поднимается до одной записи.
    #[test]
    fn zero_capacity_is_raised_to_one_entry() {
        let mut cache = cache(0);
        let _ = cache.insert(identity(1, 1, 1920), frame(1), FakeTexture(1));
        let _ = cache.insert(identity(2, 2, 1920), frame(2), FakeTexture(2));

        let snapshot = cache.snapshot();
        assert_eq!(snapshot.capacity, 1);
        assert_eq!(snapshot.entries, 1);
        assert_eq!(snapshot.evicted_capacity, 1);
    }

    /// Image, вытесненный из кэша, остаётся живым у того, кто держит `Arc`.
    #[test]
    fn evicted_texture_survives_while_a_view_holder_keeps_it() {
        let mut cache = cache(1);
        let held = cache.insert(identity(1, 1, 1920), frame(1), FakeTexture(1));
        let _ = cache.insert(identity(2, 2, 1920), frame(2), FakeTexture(2));

        assert_eq!(*held, FakeTexture(1));
        assert_eq!(
            Arc::strong_count(&held),
            1,
            "cache dropped its own reference"
        );
    }
}
