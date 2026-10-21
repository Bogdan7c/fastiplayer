//! Кэш bind group-ов видео-рендера и гейт записи uniform-буфера.
//!
//! Раньше каждый кадр делал `queue.write_buffer` (uniform) и
//! `device.create_bind_group` (текстуры кадра + uniform + sampler), хотя на
//! стабильном потоке меняется только сам кадр, а uniform почти всегда
//! побайтово тот же. Профиль показал ~3 % главного потока на этих двух вызовах.
//!
//! Модуль даёт два независимых механизма:
//!
//! 1. [`UniformWriteGate`] — рендерер помнит последние записанные байты uniform
//!    на каждый слот и пропускает `write_buffer`, если байты не изменились.
//!    Это безопасно: `write_buffer` ставится в очередь и исполняется на
//!    ближайшем `queue.submit`, значит, уже записанное содержимое буфера
//!    остаётся в нём до следующей записи.
//! 2. [`BindGroupCache`] — маленький потокобезопасный кэш bind group-ов,
//!    который **живёт внутри владельца texture views** (импортированная
//!    DMA-BUF текстура или слот host-planar загрузки) и умирает вместе с ним.
//!
//! ## Почему кэш у текстуры, а не у рендерера
//!
//! На аппаратном пути кадры идут по кругу через ~20 поверхностей пула, поэтому
//! кэш «одна запись на рендерер» никогда бы не попал. Кэш у текстуры попадает,
//! как только та же поверхность показывается повторно.
//!
//! ## Жизненный цикл и отсутствие циклов
//!
//! * Рендерер НЕ хранит ссылок на кэш и bind group-ы: он получает `&cache` из
//!   кадра на время одного `render_frame` и клонирует `BindGroup` (это `Arc`).
//!   Поэтому вытесненная из `DmaBufTextureCache` текстура освобождается ровно
//!   тогда же, что и раньше (гарантия сессии 03: смена формата освобождает
//!   старые импорты до импорта новых) — кэш рендерера её не удерживает.
//! * `BindGroup` держит wgpu-view своей же текстуры; кэш лежит внутри
//!   владельца этих views. Цикла через наш `Arc` нет: владелец -> кэш ->
//!   `BindGroup` -> wgpu-view -> wgpu-текстура, обратной ссылки на владельца нет.
//! * В структуре владельца поле кэша объявляется РАНЬШЕ views/storage: Rust
//!   уничтожает поля в порядке объявления, значит bind group-ы отпускаются до
//!   views и до drop-callback-а storage.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use super::VideoRenderTargetLoad;

/// Максимум записей в кэше одной текстуры.
///
/// Реально на текстуру нужно 1–2 записи: один рендерер × два uniform-слота
/// (main + overlay). Запас до 4 покрывает пересоздание рендерера: записи
/// прежнего экземпляра вытесняются по FIFO, а не копятся.
pub(crate) const FRAME_BIND_GROUP_CACHE_CAPACITY: usize = 4;

/// Уникальный идентификатор экземпляра pipeline-состояния рендерера.
///
/// Выдаётся при создании [`PassBinding`]: bind group зависит от layout-а,
/// sampler-а и uniform-буфера конкретного экземпляра, поэтому запись, созданная
/// одним экземпляром (в том числе до пересоздания рендерера), не должна
/// использоваться другим. Разные виды pipeline-а (NV12/P010/host 8-bit/host
/// 16-bit) имеют разные `PassBindingId`, так что вид отдельно в ключе не нужен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PassBindingId(u64);

impl PassBindingId {
    /// Выдаёт следующий неиспользованный идентификатор.
    fn allocate() -> Self {
        // Порядок не важен: нужна только уникальность значения.
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// Ключ записи кэша: экземпляр pipeline-состояния + слот uniform-буфера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BindGroupKey {
    /// Кто создал bind group (layout, sampler, uniform-буферы).
    pub(crate) owner: PassBindingId,

    /// Какой из uniform-слотов (main/overlay) зашит в bind group.
    pub(crate) uniform_slot: usize,
}

/// Откуда взялась bind group, отданная вызывающему коду.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindGroupSource {
    /// Взята из кэша владельца текстуры.
    Reused,

    /// Создана сейчас (промах кэша или кадр без кэша).
    Created,
}

/// Потокобезопасный FIFO-кэш bind group-ов одной текстуры.
///
/// Обобщён по значению, чтобы логику ёмкости/вытеснения проверять без GPU;
/// в production используется [`FrameBindGroupCache`].
pub(crate) struct BindGroupCache<T> {
    /// Записи в порядке вставки; старейшая — спереди.
    entries: Mutex<VecDeque<(BindGroupKey, T)>>,
}

/// Кэш bind group-ов, лежащий внутри владельца texture views.
pub(crate) type FrameBindGroupCache = BindGroupCache<wgpu::BindGroup>;

impl<T> Default for BindGroupCache<T> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(VecDeque::with_capacity(FRAME_BIND_GROUP_CACHE_CAPACITY)),
        }
    }
}

impl<T: Clone> BindGroupCache<T> {
    /// Возвращает запись по ключу или создаёт её через `create`.
    ///
    /// Лок держится на время `create`: вызов приходит только с render-потока,
    /// конкуренции нет, а так два вызова не создадут дубликат одного ключа.
    /// Если лок отравлен (другой поток упал внутри), кэш обходится: bind group
    /// создаётся на кадр, как до появления кэша, — рендер не должен падать
    /// из-за оптимизации.
    pub(crate) fn get_or_create(
        &self,
        key: BindGroupKey,
        create: impl FnOnce() -> T,
    ) -> (T, BindGroupSource) {
        let Ok(mut entries) = self.entries.lock() else {
            return (create(), BindGroupSource::Created);
        };
        if let Some((_, cached)) = entries.iter().find(|(entry_key, _)| *entry_key == key) {
            return (cached.clone(), BindGroupSource::Reused);
        }
        let created = create();
        if entries.len() >= FRAME_BIND_GROUP_CACHE_CAPACITY {
            // Вытесняем старейшую запись: чаще всего это экземпляр прежнего рендерера.
            entries.pop_front();
        }
        entries.push_back((key, created.clone()));
        (created, BindGroupSource::Created)
    }

    /// Сколько записей сейчас лежит в кэше.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.lock().map_or(0, |entries| entries.len())
    }
}

/// Решение гейта: нужно ли писать uniform в GPU-буфер.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UniformWriteDecision {
    /// Байты изменились (или это первая запись слота): `write_buffer` обязателен.
    Write,

    /// Байты совпали с уже записанными: `write_buffer` можно пропустить.
    Skip,
}

/// Помнит последние записанные байты uniform на каждый слот.
///
/// Состояние начинается пустым, поэтому первая запись нового буфера (или
/// нового рендерера) всегда выполняется.
#[derive(Default)]
pub(crate) struct UniformWriteGate {
    /// Последние записанные байты по слотам; `None` — слот ещё не писали.
    last_written: [Option<Vec<u8>>; VideoRenderTargetLoad::UNIFORM_SLOT_COUNT],
}

impl UniformWriteGate {
    /// Решает, нужна ли запись, и запоминает байты, если нужна.
    ///
    /// Слоты независимы: запись main-слота не влияет на overlay-слот.
    pub(crate) fn decide(&mut self, slot: usize, uniform_bytes: &[u8]) -> UniformWriteDecision {
        let Some(last_written) = self.last_written.get_mut(slot) else {
            // Неизвестный слот: безопаснее писать всегда, чем пропустить запись.
            return UniformWriteDecision::Write;
        };
        match last_written {
            Some(previous) if previous.as_slice() == uniform_bytes => UniformWriteDecision::Skip,
            Some(previous) => {
                // Переиспользуем выделенную память: в steady state без аллокаций.
                previous.clear();
                previous.extend_from_slice(uniform_bytes);
                UniformWriteDecision::Write
            }
            None => {
                *last_written = Some(uniform_bytes.to_vec());
                UniformWriteDecision::Write
            }
        }
    }
}

/// Счётчики горячего пути привязок видео-рендера (для диагностики и тестов).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoHotPathStats {
    /// Сколько bind group-ов реально создано через `device.create_bind_group`.
    pub bind_groups_created: u64,

    /// Сколько bind group-ов взято из кэша владельца текстуры.
    pub bind_groups_reused: u64,

    /// Сколько раз uniform реально записан через `queue.write_buffer`.
    pub uniform_writes: u64,

    /// Сколько записей uniform пропущено, потому что байты не изменились.
    pub uniform_writes_skipped: u64,
}

impl VideoHotPathStats {
    /// Складывает счётчики нескольких pipeline-состояний в один отчёт.
    #[must_use]
    pub fn merged_with(self, other: Self) -> Self {
        Self {
            bind_groups_created: self
                .bind_groups_created
                .saturating_add(other.bind_groups_created),
            bind_groups_reused: self
                .bind_groups_reused
                .saturating_add(other.bind_groups_reused),
            uniform_writes: self.uniform_writes.saturating_add(other.uniform_writes),
            uniform_writes_skipped: self
                .uniform_writes_skipped
                .saturating_add(other.uniform_writes_skipped),
        }
    }
}

/// Привязочное состояние одного pipeline-а рендерера: id, гейт uniform, счётчики.
///
/// Владеется конкретным рендерером (NV12, P010, host 8-bit, host 16-bit);
/// остальной код видит только intent-методы.
pub(crate) struct PassBinding {
    /// Уникальный id экземпляра; часть ключа кэша bind group-ов.
    id: PassBindingId,

    /// Гейт записи uniform по слотам.
    uniform_gate: UniformWriteGate,

    /// Накопленные счётчики.
    stats: VideoHotPathStats,
}

impl PassBinding {
    /// Создаёт пустое состояние с новым уникальным id.
    pub(crate) fn new() -> Self {
        Self {
            id: PassBindingId::allocate(),
            uniform_gate: UniformWriteGate::default(),
            stats: VideoHotPathStats::default(),
        }
    }

    /// Решает про запись uniform и учитывает её в счётчиках (без обращения к GPU).
    pub(crate) fn plan_uniform_write(
        &mut self,
        slot: usize,
        uniform_bytes: &[u8],
    ) -> UniformWriteDecision {
        let decision = self.uniform_gate.decide(slot, uniform_bytes);
        match decision {
            UniformWriteDecision::Write => self.stats.uniform_writes += 1,
            UniformWriteDecision::Skip => self.stats.uniform_writes_skipped += 1,
        }
        decision
    }

    /// Пишет uniform в буфер слота, только если байты изменились.
    pub(crate) fn write_uniforms_if_changed(
        &mut self,
        queue: &wgpu::Queue,
        uniform_buffer: &wgpu::Buffer,
        slot: usize,
        uniform_bytes: &[u8],
    ) {
        if self.plan_uniform_write(slot, uniform_bytes) == UniformWriteDecision::Write {
            queue.write_buffer(uniform_buffer, 0, uniform_bytes);
        }
    }

    /// Отдаёт bind group для кадра: из кэша владельца текстуры либо новую.
    ///
    /// `frame_cache == None` (кадр без владельца: тесты, udmabuf без external
    /// sync) — прежнее поведение: bind group создаётся на кадр.
    pub(crate) fn resolve_bind_group<T: Clone>(
        &mut self,
        slot: usize,
        frame_cache: Option<&BindGroupCache<T>>,
        create: impl FnOnce() -> T,
    ) -> T {
        let (bind_group, source) = match frame_cache {
            Some(cache) => cache.get_or_create(
                BindGroupKey {
                    owner: self.id,
                    uniform_slot: slot,
                },
                create,
            ),
            None => (create(), BindGroupSource::Created),
        };
        match source {
            BindGroupSource::Reused => self.stats.bind_groups_reused += 1,
            BindGroupSource::Created => self.stats.bind_groups_created += 1,
        }
        bind_group
    }

    /// Текущие счётчики этого pipeline-состояния.
    pub(crate) const fn stats(&self) -> VideoHotPathStats {
        self.stats
    }
}

#[cfg(test)]
mod gpu_tests;
#[cfg(test)]
mod tests;
