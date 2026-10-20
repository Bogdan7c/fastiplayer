//! Fake surfaces и allocator для тестов пула и его потребителей: без VA display и железа.

use std::cell::{Cell, RefCell};
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::Arc;

use cros_codecs::Resolution;
use cros_codecs::libva::{VA_RT_FORMAT_YUV420, VA_RT_FORMAT_YUV420_10};

use super::{
    OutputSurfaceAllocationError, OutputSurfaceAllocator, OutputSurfaceFormat,
    OutputSurfaceIdleError, OutputSurfaceLease, OutputSurfaceLeaseOutcome, OutputSurfacePool,
    OutputSurfacePoolCounters, PooledOutputSurface, SurfaceIdleWait,
};

/// Журнал разрушенных fake surfaces (по id).
pub(crate) type DestroyedSurfaceLog = Rc<RefCell<Vec<u32>>>;

/// Что fake-драйвер отвечает на проверку простоя surface при возврате в пул.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FakeSurfaceIdleBehavior {
    /// Surface свободна сразу.
    Idle,
    /// Над surface ещё идёт работа; ожидание завершается успешно.
    BusyThenIdle,
    /// Драйвер не может сообщить состояние surface.
    QueryFails,
}

/// Fake surface: id + запись в журнал при разрушении.
pub(crate) struct FakeSurface {
    pub(crate) id: u32,
    pub(crate) format: OutputSurfaceFormat,
    destroyed: DestroyedSurfaceLog,
    /// Поведение проверки простоя; общий рычаг allocator-а, чтобы тест менял его на лету.
    idle_behavior: Rc<Cell<FakeSurfaceIdleBehavior>>,
}

impl Drop for FakeSurface {
    fn drop(&mut self) {
        self.destroyed.borrow_mut().push(self.id);
    }
}

impl PooledOutputSurface for FakeSurface {
    fn wait_until_idle(&self) -> Result<SurfaceIdleWait, OutputSurfaceIdleError> {
        match self.idle_behavior.get() {
            FakeSurfaceIdleBehavior::Idle => Ok(SurfaceIdleWait::AlreadyIdle),
            FakeSurfaceIdleBehavior::BusyThenIdle => Ok(SurfaceIdleWait::WaitedForDriver),
            FakeSurfaceIdleBehavior::QueryFails => Err(OutputSurfaceIdleError {
                message: format!("fake driver: surface {} status unknown", self.id),
            }),
        }
    }
}

/// Fake allocator: выдаёт surfaces с растущими id или ошибку драйвера по флагу.
pub(crate) struct FakeAllocator {
    next_id: Cell<u32>,
    fail_allocations: Rc<Cell<bool>>,
    destroyed: DestroyedSurfaceLog,
    idle_behavior: Rc<Cell<FakeSurfaceIdleBehavior>>,
}

impl OutputSurfaceAllocator for FakeAllocator {
    type Surface = FakeSurface;

    fn allocate(
        &self,
        format: OutputSurfaceFormat,
    ) -> Result<Self::Surface, OutputSurfaceAllocationError> {
        if self.fail_allocations.get() {
            return Err(OutputSurfaceAllocationError {
                format,
                message: "fake driver: out of video memory".to_string(),
            });
        }
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        Ok(FakeSurface {
            id,
            format,
            destroyed: self.destroyed.clone(),
            idle_behavior: self.idle_behavior.clone(),
        })
    }
}

/// Пул под тестом и рычаги fake allocator-а.
pub(crate) struct PoolFixture {
    pub(crate) pool: OutputSurfacePool<FakeAllocator>,
    pub(crate) destroyed: DestroyedSurfaceLog,
    pub(crate) fail_allocations: Rc<Cell<bool>>,
    pub(crate) idle_behavior: Rc<Cell<FakeSurfaceIdleBehavior>>,
    pub(crate) counters: Arc<OutputSurfacePoolCounters>,
}

pub(crate) fn format_1080p() -> OutputSurfaceFormat {
    OutputSurfaceFormat {
        coded_resolution: Resolution::from((1920, 1088)),
        rt_format: VA_RT_FORMAT_YUV420,
    }
}

pub(crate) fn format_4k_p010() -> OutputSurfaceFormat {
    OutputSurfaceFormat {
        coded_resolution: Resolution::from((3840, 2160)),
        rt_format: VA_RT_FORMAT_YUV420_10,
    }
}

pub(crate) fn frames(count: usize) -> NonZeroUsize {
    NonZeroUsize::new(count).unwrap_or(NonZeroUsize::MIN)
}

/// Пустой пул с fake allocator-ом и форматом 1080p.
pub(crate) fn pool_with_capacity(capacity: usize) -> PoolFixture {
    let destroyed = DestroyedSurfaceLog::default();
    let fail_allocations = Rc::new(Cell::new(false));
    let idle_behavior = Rc::new(Cell::new(FakeSurfaceIdleBehavior::Idle));
    let counters = Arc::<OutputSurfacePoolCounters>::default();
    let allocator = FakeAllocator {
        next_id: Cell::new(1),
        fail_allocations: fail_allocations.clone(),
        destroyed: destroyed.clone(),
        idle_behavior: idle_behavior.clone(),
    };
    PoolFixture {
        pool: OutputSurfacePool::new(
            allocator,
            format_1080p(),
            frames(capacity),
            counters.clone(),
        ),
        destroyed,
        fail_allocations,
        idle_behavior,
        counters,
    }
}

/// Берёт аренду или паникует: тестам нужен именно успешный исход.
pub(crate) fn lease(pool: &OutputSurfacePool<FakeAllocator>) -> OutputSurfaceLease<FakeSurface> {
    match pool.try_lease() {
        OutputSurfaceLeaseOutcome::Leased(lease) => lease,
        OutputSurfaceLeaseOutcome::Exhausted => panic!("pool unexpectedly exhausted"),
        OutputSurfaceLeaseOutcome::AllocationFailed(error) => {
            panic!("unexpected allocation failure: {error}")
        }
    }
}
