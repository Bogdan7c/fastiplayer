//! Ограниченный пул выходных VA surfaces декодера.
//!
//! До этого модуля на каждый декодированный кадр выполнялись `vaCreateSurfaces`
//! и `vaDestroySurfaces`: surface жила внутри cros-codecs picture и умирала вместе
//! с ней. Теперь surface принадлежит пулу, picture получает её в аренду
//! ([`OutputSurfaceLease`]), а когда отпущен последний владелец (DPB декодера,
//! decoded handle плеера, подавленный preroll-кадр), аренда в `Drop` возвращает
//! surface в список свободных.
//!
//! Инварианты пула:
//! 1. Число живых surfaces (свободные + выданные, включая surfaces старого формата,
//!    которые ещё держит плеер) никогда не превышает ёмкость. Пустой пул — это
//!    `OutputSurfaceLeaseOutcome::Exhausted` (backpressure), а не новая аллокация.
//! 2. Ошибка аллокации — отдельный исход `OutputSurfaceLeaseOutcome::AllocationFailed`,
//!    она не сливается с backpressure: иначе декодер вечно ждал бы буфер, которого
//!    драйвер не может создать.
//! 3. Surface другого формата (разрешение или VA RT format) не переиспользуется:
//!    после `OutputSurfacePool::reconfigure` старые свободные surfaces уничтожаются,
//!    а выданные старые при возврате не попадают в список свободных.
//! 4. Surfaces создаются лениво: только когда свободных нет и ёмкость позволяет.
//!    Поэтому памяти занято столько, сколько реально нужно одновременно, а не
//!    сразу вся ёмкость.
//! 5. В список свободных попадает только surface, над которой драйвер закончил
//!    работу ([`PooledOutputSurface::wait_until_idle`]). Иначе следующая аренда
//!    могла бы отдать под новый кадр surface, в которую GPU ещё пишет предыдущий.
//!    Surface, у которой это нельзя проверить, уничтожается, а не выдаётся.
//!
//! Пул однопоточный (`Rc`/`Cell`), как и VA display/decoder: живёт в decoder thread.
//! Наружу (в другие потоки) уходят только атомарные счётчики [`OutputSurfacePoolStats`].

use std::cell::{Cell, RefCell};
use std::fmt;
use std::num::NonZeroUsize;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use cros_codecs::Resolution;

#[cfg(test)]
pub(crate) mod test_support;
mod va_surfaces;

pub use va_surfaces::VaapiOutputSurfaceLease;
pub(crate) use va_surfaces::{VaOutputSurfaceAllocator, VaapiOutputSurfacePool};

/// Формат выходной surface: coded resolution + VA RT format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputSurfaceFormat {
    /// Coded resolution, под которую выделяется память surface.
    pub coded_resolution: Resolution,
    /// VA RT format (`VA_RT_FORMAT_YUV420`, `VA_RT_FORMAT_YUV420_10`, ...).
    pub rt_format: u32,
}

/// Создатель surfaces конкретного формата.
///
/// В production это VA display ([`VaOutputSurfaceAllocator`]), в тестах — fake без
/// железа. Пул не знает, что такое VA surface: он только считает и хранит.
pub(crate) trait OutputSurfaceAllocator {
    /// Тип surface, которой владеет пул.
    type Surface: PooledOutputSurface;

    /// Создаёт одну новую surface заданного формата.
    fn allocate(
        &self,
        format: OutputSurfaceFormat,
    ) -> Result<Self::Surface, OutputSurfaceAllocationError>;
}

/// Драйвер не смог создать surface (например, кончилась видеопамять).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputSurfaceAllocationError {
    /// Формат, который не удалось выделить.
    pub format: OutputSurfaceFormat,
    /// Текст ошибки драйвера.
    pub message: String,
}

impl fmt::Display for OutputSurfaceAllocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to allocate {}x{} output surface (rt_format {:#x}): {}",
            self.format.coded_resolution.width,
            self.format.coded_resolution.height,
            self.format.rt_format,
            self.message
        )
    }
}

impl std::error::Error for OutputSurfaceAllocationError {}

/// Контракт surface, которую пул может выдать повторно.
///
/// cros-codecs в Blocking-режиме синхронизирует картинку сразу после submit, но на
/// путях ошибок (упал `vaSyncSurface`/`vaEndPicture`, picture выброшена до sync)
/// аренда возвращается, пока драйвер ещё может писать в surface. Поэтому перед
/// повторной выдачей пул спрашивает у surface, свободна ли она, и при необходимости
/// ждёт драйвер.
pub trait PooledOutputSurface {
    /// Дожидается, пока у драйвера не останется незавершённой работы над surface.
    ///
    /// Возвращает, пришлось ли ждать (для диагностики). Ошибка означает, что
    /// состояние surface неизвестно: такую surface нельзя выдавать повторно.
    fn wait_until_idle(&self) -> Result<SurfaceIdleWait, OutputSurfaceIdleError>;
}

/// Чем закончилось ожидание свободной surface при возврате в пул.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceIdleWait {
    /// Драйвер уже закончил работу: ждать не пришлось.
    AlreadyIdle,
    /// Над surface ещё шла работа, пул дождался её завершения.
    WaitedForDriver,
}

/// Драйвер не смог сообщить состояние surface или дождаться её освобождения.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputSurfaceIdleError {
    /// Текст ошибки драйвера.
    pub message: String,
}

impl fmt::Display for OutputSurfaceIdleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to wait for an idle output surface: {}",
            self.message
        )
    }
}

impl std::error::Error for OutputSurfaceIdleError {}

/// Результат попытки взять surface в аренду.
pub(crate) enum OutputSurfaceLeaseOutcome<S: PooledOutputSurface> {
    /// Surface выдана: из списка свободных или только что создана.
    Leased(OutputSurfaceLease<S>),
    /// Все surfaces заняты, а ёмкость исчерпана: вызывающий ждёт возврата (backpressure).
    Exhausted,
    /// Ёмкость позволяла создать surface, но драйвер вернул ошибку.
    AllocationFailed(OutputSurfaceAllocationError),
}

/// Итог смены формата/ёмкости пула — для логов и тестов.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OutputSurfaceReconfigureReport {
    /// Формат действительно сменился (свободные surfaces старого формата уничтожены).
    pub format_changed: bool,
    /// Сколько свободных surfaces уничтожено (старый формат или сжатие ёмкости).
    pub destroyed_free_surfaces: usize,
    /// Ёмкость после reconfigure.
    pub capacity: NonZeroUsize,
    /// Ёмкость поднята выше настройки пользователя до минимума кодека.
    pub raised_to_stream_minimum: bool,
}

/// Сколько surfaces стриму нужно, чтобы декодирование не встало.
///
/// Это минимум кодека (DPB + запас cros-codecs, `StreamInfo::min_num_frames`)
/// плюс кадры, которые одновременно держат вне декодера плеер и рендерер:
/// без этой добавки DPB занял бы почти всю ёмкость, а плеер, ждущий preroll,
/// никогда не отпустил бы кадры — взаимная блокировка. Добавку считает
/// вызывающий слой (`decoder::output_surface_format`), пул только сравнивает.
///
/// Решение владельца (2026-10-20): если настройка пользователя меньше этого
/// минимума, пул поднимается до минимума, чтобы видео не застыло, и пишет warn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StreamSurfaceMinimum {
    /// Стриму нужно столько surfaces одновременно.
    Frames(NonZeroUsize),
    /// Минимум неизвестен — действует настройка пользователя как есть.
    Unknown,
}

/// Снимок счётчиков пула; безопасно читается из любого потока.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutputSurfacePoolStats {
    /// Действующая ёмкость пула.
    pub capacity: usize,
    /// Наибольшая ёмкость за всё время пула (ёмкость может меняться на стримах).
    ///
    /// Инвариант «живых не больше ёмкости» проверяется именно против неё:
    /// после сжатия ёмкости старые выданные surfaces ещё живы, и текущая
    /// ёмкость временно меньше числа живых.
    pub peak_capacity: usize,
    /// Живые surfaces: свободные + выданные (включая старый формат).
    pub live_surfaces: usize,
    /// Свободные surfaces текущего формата.
    pub free_surfaces: usize,
    /// Максимум живых surfaces за всё время пула.
    pub peak_live_surfaces: usize,
    /// Сколько раз вызван `vaCreateSurfaces` (создано surfaces).
    pub surfaces_created: u64,
    /// Сколько surfaces уничтожено (старый формат, сжатие, не-idle, teardown пула).
    pub surfaces_destroyed: u64,
    /// Сколько surfaces навсегда забрано из пула (`into_detached_surface`); они живы.
    pub surfaces_detached: u64,
    /// Сколько аренд выдано повторно из списка свободных (без аллокации).
    pub leases_reused: u64,
    /// Сколько раз аренда не выдана из-за исчерпания ёмкости.
    pub exhausted_lease_attempts: u64,
    /// Сколько раз драйвер не смог создать surface.
    pub allocation_failures: u64,
    /// Сколько surfaces вернулись с незавершённой работой драйвера и были дожданы.
    pub surfaces_synced_on_return: u64,
    /// Сколько surfaces уничтожено при возврате, потому что драйвер не смог
    /// подтвердить их освобождение (входят в `surfaces_destroyed`).
    pub surfaces_destroyed_not_idle: u64,
}

/// Атомарные счётчики пула: пишет decoder thread, читает кто угодно.
///
/// `Relaxed` достаточно: это диагностика без межпоточного протокола; на самом
/// decoder thread значения точные, потому что пишет только он.
#[derive(Debug, Default)]
pub struct OutputSurfacePoolCounters {
    capacity: AtomicUsize,
    peak_capacity: AtomicUsize,
    live_surfaces: AtomicUsize,
    free_surfaces: AtomicUsize,
    peak_live_surfaces: AtomicUsize,
    surfaces_created: AtomicU64,
    surfaces_destroyed: AtomicU64,
    surfaces_detached: AtomicU64,
    leases_reused: AtomicU64,
    exhausted_lease_attempts: AtomicU64,
    allocation_failures: AtomicU64,
    surfaces_synced_on_return: AtomicU64,
    surfaces_destroyed_not_idle: AtomicU64,
}

impl OutputSurfacePoolCounters {
    /// Возвращает согласованный для диагностики снимок счётчиков.
    #[must_use]
    pub fn snapshot(&self) -> OutputSurfacePoolStats {
        OutputSurfacePoolStats {
            capacity: self.capacity.load(Ordering::Relaxed),
            peak_capacity: self.peak_capacity.load(Ordering::Relaxed),
            live_surfaces: self.live_surfaces.load(Ordering::Relaxed),
            free_surfaces: self.free_surfaces.load(Ordering::Relaxed),
            peak_live_surfaces: self.peak_live_surfaces.load(Ordering::Relaxed),
            surfaces_created: self.surfaces_created.load(Ordering::Relaxed),
            surfaces_destroyed: self.surfaces_destroyed.load(Ordering::Relaxed),
            surfaces_detached: self.surfaces_detached.load(Ordering::Relaxed),
            leases_reused: self.leases_reused.load(Ordering::Relaxed),
            exhausted_lease_attempts: self.exhausted_lease_attempts.load(Ordering::Relaxed),
            allocation_failures: self.allocation_failures.load(Ordering::Relaxed),
            surfaces_synced_on_return: self.surfaces_synced_on_return.load(Ordering::Relaxed),
            surfaces_destroyed_not_idle: self.surfaces_destroyed_not_idle.load(Ordering::Relaxed),
        }
    }

    fn live_surfaces(&self) -> usize {
        self.live_surfaces.load(Ordering::Relaxed)
    }

    fn capacity(&self) -> usize {
        self.capacity.load(Ordering::Relaxed)
    }

    /// Фиксирует действующую ёмкость и её исторический максимум.
    fn record_capacity(&self, capacity: NonZeroUsize) {
        self.capacity.store(capacity.get(), Ordering::Relaxed);
        self.peak_capacity
            .fetch_max(capacity.get(), Ordering::Relaxed);
    }

    fn record_created(&self) {
        let live_surfaces = self.live_surfaces.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak_live_surfaces
            .fetch_max(live_surfaces, Ordering::Relaxed);
        self.surfaces_created.fetch_add(1, Ordering::Relaxed);
    }

    fn record_destroyed(&self, destroyed: usize) {
        self.live_surfaces.fetch_sub(destroyed, Ordering::Relaxed);
        self.surfaces_destroyed
            .fetch_add(destroyed as u64, Ordering::Relaxed);
    }

    /// Surface ушла из-под учёта пула живой: ёмкость освобождается без destroy.
    fn record_detached(&self) {
        self.live_surfaces.fetch_sub(1, Ordering::Relaxed);
        self.surfaces_detached.fetch_add(1, Ordering::Relaxed);
    }

    fn record_synced_on_return(&self) {
        self.surfaces_synced_on_return
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Surface уничтожена при возврате, потому что драйвер не подтвердил её освобождение.
    fn record_destroyed_not_idle(&self) {
        self.record_destroyed(1);
        self.surfaces_destroyed_not_idle
            .fetch_add(1, Ordering::Relaxed);
    }
}

/// Общее состояние пула, на которое аренды держат `Weak`.
struct OutputSurfacePoolShared<S> {
    /// Текущий формат выдаваемых surfaces.
    format: Cell<OutputSurfaceFormat>,
    /// Поколение формата: аренда старого поколения при возврате уничтожает surface.
    format_epoch: Cell<u64>,
    /// Свободные surfaces текущего формата (LIFO: последняя вернувшаяся выдаётся первой).
    free_surfaces: RefCell<Vec<S>>,
    /// Ёмкость из настройки пользователя (`video.decoder_surface_pool_frames`).
    configured_capacity: NonZeroUsize,
    /// Счётчики и действующая ёмкость.
    counters: Arc<OutputSurfacePoolCounters>,
}

/// Teardown пула (decoder остановлен): свободные surfaces уничтожаются вместе с
/// `free_surfaces`, и счётчики должны это отразить, иначе снимок на живом `Arc`
/// показывал бы живые surfaces у мёртвого декодера. Выданные surfaces учтёт
/// их собственная аренда при drop-е.
impl<S> Drop for OutputSurfacePoolShared<S> {
    fn drop(&mut self) {
        let free_surfaces = self.free_surfaces.get_mut();
        let destroyed = free_surfaces.len();
        free_surfaces.clear();
        self.counters.free_surfaces.store(0, Ordering::Relaxed);
        self.counters.record_destroyed(destroyed);
    }
}

impl<S: PooledOutputSurface> OutputSurfacePoolShared<S> {
    /// Принимает surface обратно от аренды.
    ///
    /// Surface уничтожается вместо возврата, если она старого формата, если живых
    /// surfaces больше ёмкости (ёмкость уменьшилась после reconfigure) или если
    /// драйвер не может подтвердить, что закончил с ней работу.
    fn accept_returned_surface(&self, surface: S, lease_epoch: u64) {
        let is_current_format = lease_epoch == self.format_epoch.get();
        let is_over_capacity = self.counters.live_surfaces() > self.counters.capacity();
        if !is_current_format || is_over_capacity {
            drop(surface);
            self.counters.record_destroyed(1);
            return;
        }
        // Инвариант 5: в список свободных попадает только surface без незавершённой
        // работы драйвера. Обычно она уже синхронизирована cros-codecs, и проверка
        // стоит один `vaQuerySurfaceStatus`; ждать приходится только на путях ошибок.
        match surface.wait_until_idle() {
            Ok(SurfaceIdleWait::AlreadyIdle) => {}
            Ok(SurfaceIdleWait::WaitedForDriver) => self.counters.record_synced_on_return(),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "Output surface returned to the pool is not provably idle; destroying it"
                );
                drop(surface);
                self.counters.record_destroyed_not_idle();
                return;
            }
        }
        // Пул никогда не держит `free_surfaces` занятым во время drop-а аренд, но
        // fail-safe: если RefCell всё же занят, surface уничтожается и учитывается,
        // а не теряется из accounting-а.
        match self.free_surfaces.try_borrow_mut() {
            Ok(mut free_surfaces) => {
                free_surfaces.push(surface);
                self.counters
                    .free_surfaces
                    .store(free_surfaces.len(), Ordering::Relaxed);
            }
            Err(_) => {
                tracing::warn!("Output surface pool busy during lease return; destroying surface");
                drop(surface);
                self.counters.record_destroyed(1);
            }
        }
    }
}

/// Аренда одной surface пула.
///
/// Владеет surface, пока жива. `Drop` возвращает surface в пул (или уничтожает,
/// если пул уже уничтожен, формат сменился или ёмкость сжалась).
///
/// Тип `pub` только потому, что он — `NativeHandle` публичного
/// [`crate::internal_vaapi_frame::InternalVaapiFrame`]; методов наружу crate нет.
pub struct OutputSurfaceLease<S: PooledOutputSurface> {
    /// `Some` всё время жизни аренды; забирается только в `Drop` и `into_detached_surface`.
    surface: Option<S>,
    /// Формат surface на момент выдачи.
    format: OutputSurfaceFormat,
    /// Поколение формата пула на момент выдачи.
    format_epoch: u64,
    /// Пул-владелец; `Weak`, чтобы аренда не продлевала жизнь пула.
    pool: Weak<OutputSurfacePoolShared<S>>,
    /// Счётчики пула: живут дольше пула, чтобы аренда, пережившая его,
    /// всё равно списала свою surface из живых.
    counters: Arc<OutputSurfacePoolCounters>,
}

impl<S: PooledOutputSurface> OutputSurfaceLease<S> {
    /// Формат арендованной surface.
    pub(crate) fn format(&self) -> OutputSurfaceFormat {
        self.format
    }

    /// Доступ к surface.
    #[expect(
        clippy::expect_used,
        reason = "surface is Some until Drop or into_detached_surface consume the lease"
    )]
    pub(crate) fn surface(&self) -> &S {
        self.surface
            .as_ref()
            .expect("output surface lease always holds its surface while alive")
    }

    /// Забирает surface из пула навсегда (она больше не учитывается пулом).
    ///
    /// Нужен только контракту cros-codecs `NativeHandle: Into<Surface>` (encoder
    /// import); decode-путь этим не пользуется. Surface остаётся живой, поэтому
    /// считается как detached, а не как destroyed.
    pub(crate) fn into_detached_surface(mut self) -> Option<S> {
        let surface = self.surface.take();
        if surface.is_some() {
            // Surface уходит из-под учёта пула: ёмкость освобождается.
            self.counters.record_detached();
        }
        surface
    }
}

impl<S: PooledOutputSurface> Drop for OutputSurfaceLease<S> {
    fn drop(&mut self) {
        let Some(surface) = self.surface.take() else {
            return;
        };
        match self.pool.upgrade() {
            Some(pool) => pool.accept_returned_surface(surface, self.format_epoch),
            // Пул уже уничтожен вместе с decoder-ом: surface просто разрушается,
            // но из живых её всё равно надо списать.
            None => {
                drop(surface);
                self.counters.record_destroyed(1);
            }
        }
    }
}

impl<S: PooledOutputSurface> fmt::Debug for OutputSurfaceLease<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputSurfaceLease")
            .field("format", &self.format)
            .field("format_epoch", &self.format_epoch)
            .field("holds_surface", &self.surface.is_some())
            .finish()
    }
}

/// Ограниченный пул выходных surfaces.
pub(crate) struct OutputSurfacePool<A: OutputSurfaceAllocator> {
    allocator: A,
    shared: Rc<OutputSurfacePoolShared<A::Surface>>,
}

impl<A: OutputSurfaceAllocator> OutputSurfacePool<A> {
    /// Создаёт пустой пул: surfaces появятся лениво при первой аренде.
    pub(crate) fn new(
        allocator: A,
        initial_format: OutputSurfaceFormat,
        configured_capacity: NonZeroUsize,
        counters: Arc<OutputSurfacePoolCounters>,
    ) -> Self {
        counters.record_capacity(configured_capacity);
        Self {
            allocator,
            shared: Rc::new(OutputSurfacePoolShared {
                format: Cell::new(initial_format),
                format_epoch: Cell::new(0),
                free_surfaces: RefCell::new(Vec::with_capacity(configured_capacity.get())),
                configured_capacity,
                counters,
            }),
        }
    }

    /// Текущий формат выдаваемых surfaces.
    pub(crate) fn format(&self) -> OutputSurfaceFormat {
        self.shared.format.get()
    }

    /// Снимок счётчиков.
    pub(crate) fn stats(&self) -> OutputSurfacePoolStats {
        self.shared.counters.snapshot()
    }

    /// Выдаёт surface в аренду: свободную, новую (если позволяет ёмкость) или отказ.
    pub(crate) fn try_lease(&self) -> OutputSurfaceLeaseOutcome<A::Surface> {
        let counters = &self.shared.counters;
        if let Some(surface) = self.pop_free_surface() {
            counters.leases_reused.fetch_add(1, Ordering::Relaxed);
            return OutputSurfaceLeaseOutcome::Leased(self.wrap_lease(surface));
        }

        if counters.live_surfaces() >= counters.capacity() {
            counters
                .exhausted_lease_attempts
                .fetch_add(1, Ordering::Relaxed);
            return OutputSurfaceLeaseOutcome::Exhausted;
        }

        let format = self.format();
        match self.allocator.allocate(format) {
            Ok(surface) => {
                counters.record_created();
                tracing::debug!(
                    width = format.coded_resolution.width,
                    height = format.coded_resolution.height,
                    rt_format = format.rt_format,
                    live_surfaces = counters.live_surfaces(),
                    capacity = counters.capacity(),
                    "Output surface pool created VA surface"
                );
                OutputSurfaceLeaseOutcome::Leased(self.wrap_lease(surface))
            }
            Err(error) => {
                counters.allocation_failures.fetch_add(1, Ordering::Relaxed);
                OutputSurfaceLeaseOutcome::AllocationFailed(error)
            }
        }
    }

    /// Переключает пул на формат нового стрима и пересчитывает ёмкость.
    ///
    /// Вызывать только на lifecycle boundary (`FormatChanged`), когда decoder уже
    /// выбросил decoder-owned кадры. Surfaces старого формата, которые ещё держит
    /// плеер, вернутся позже и будут уничтожены при возврате; до этого они
    /// занимают ёмкость (инвариант «живых не больше ёмкости»).
    pub(crate) fn reconfigure(
        &self,
        format: OutputSurfaceFormat,
        stream_minimum: StreamSurfaceMinimum,
    ) -> OutputSurfaceReconfigureReport {
        let shared = &self.shared;
        let format_changed = shared.format.get() != format;
        let mut destroyed_free_surfaces = 0;
        if format_changed {
            shared.format.set(format);
            shared
                .format_epoch
                .set(shared.format_epoch.get().wrapping_add(1));
            destroyed_free_surfaces += self.destroy_free_surfaces_while(|_| true);
        }

        let (capacity, raised_to_stream_minimum) =
            effective_capacity(shared.configured_capacity, stream_minimum);
        shared.counters.record_capacity(capacity);
        // Если ёмкость сжалась, лишние свободные surfaces уничтожаются сразу.
        destroyed_free_surfaces += self
            .destroy_free_surfaces_while(|counters| counters.live_surfaces() > counters.capacity());

        OutputSurfaceReconfigureReport {
            format_changed,
            destroyed_free_surfaces,
            capacity,
            raised_to_stream_minimum,
        }
    }

    fn pop_free_surface(&self) -> Option<A::Surface> {
        let mut free_surfaces = self.shared.free_surfaces.borrow_mut();
        let surface = free_surfaces.pop();
        self.shared
            .counters
            .free_surfaces
            .store(free_surfaces.len(), Ordering::Relaxed);
        surface
    }

    /// Уничтожает свободные surfaces, пока `should_destroy` возвращает `true`.
    fn destroy_free_surfaces_while(
        &self,
        should_destroy: impl Fn(&OutputSurfacePoolCounters) -> bool,
    ) -> usize {
        let mut destroyed = 0;
        while should_destroy(&self.shared.counters) {
            // Surface уничтожается после отпускания `borrow_mut`, чтобы её Drop не
            // встретил занятый список свободных.
            let Some(surface) = self.pop_free_surface() else {
                break;
            };
            drop(surface);
            self.shared.counters.record_destroyed(1);
            destroyed += 1;
        }
        destroyed
    }

    fn wrap_lease(&self, surface: A::Surface) -> OutputSurfaceLease<A::Surface> {
        OutputSurfaceLease {
            surface: Some(surface),
            format: self.shared.format.get(),
            format_epoch: self.shared.format_epoch.get(),
            pool: Rc::downgrade(&self.shared),
            counters: self.shared.counters.clone(),
        }
    }
}

/// Действующая ёмкость: настройка пользователя, но не меньше минимума стрима.
fn effective_capacity(
    configured_capacity: NonZeroUsize,
    stream_minimum: StreamSurfaceMinimum,
) -> (NonZeroUsize, bool) {
    match stream_minimum {
        StreamSurfaceMinimum::Frames(minimum) if minimum > configured_capacity => (minimum, true),
        StreamSurfaceMinimum::Frames(_) | StreamSurfaceMinimum::Unknown => {
            (configured_capacity, false)
        }
    }
}

#[cfg(test)]
mod tests;
