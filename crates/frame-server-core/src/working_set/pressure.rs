use super::TimelineHoverPrepareFrameKey;

/// Режим, в котором caller хочет начать hover prepare.
///
/// Тип намеренно нейтральный: frame-server-core не знает реальный player state,
/// а только получает уже принятое внешним owner-ом решение.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineHoverPrepareAdmissionMode {
    /// Обычный hover/focus prepare может поддерживать Latest-N primary byproducts.
    NormalHover,
    /// One-shot `resume_pending`: seek-owned pin уже должен быть учтён caller-ом.
    ResumePendingAfterSeekPin,
    /// Live scrub владеет decode ресурсами, поэтому hover prepare не конкурирует.
    ActiveLiveScrub,
}

/// Provider/resource budget после учёта внешних владельцев, включая S17 pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineHoverPrepareProviderBudget {
    /// Есть ещё один provider-owned resource slot для hover prepare.
    SpareSlotAvailable,
    /// Provider не может удержать ещё один pin после активных владельцев.
    ExhaustedAfterActivePins,
}

/// Запрос admission перед запуском/материализацией hover prepare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimelineHoverPrepareAdmissionRequest {
    prepared_key: TimelineHoverPrepareFrameKey,
    protected_key: TimelineHoverPrepareFrameKey,
    mode: TimelineHoverPrepareAdmissionMode,
    provider_budget: TimelineHoverPrepareProviderBudget,
}

impl TimelineHoverPrepareAdmissionRequest {
    /// Создаёт typed admission input без доступа caller-а к storage working set-а.
    #[must_use]
    pub const fn new(
        prepared_key: TimelineHoverPrepareFrameKey,
        protected_key: TimelineHoverPrepareFrameKey,
        mode: TimelineHoverPrepareAdmissionMode,
        provider_budget: TimelineHoverPrepareProviderBudget,
    ) -> Self {
        Self {
            prepared_key,
            protected_key,
            mode,
            provider_budget,
        }
    }

    #[must_use]
    pub const fn prepared_key(self) -> TimelineHoverPrepareFrameKey {
        self.prepared_key
    }

    #[must_use]
    pub const fn protected_key(self) -> TimelineHoverPrepareFrameKey {
        self.protected_key
    }

    #[must_use]
    pub const fn mode(self) -> TimelineHoverPrepareAdmissionMode {
        self.mode
    }

    #[must_use]
    pub const fn provider_budget(self) -> TimelineHoverPrepareProviderBudget {
        self.provider_budget
    }
}

/// Как primary storage сможет принять entry, если caller продолжит prepare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineHoverPrepareSlotPlan {
    /// Entry с таким key уже есть; insert заменит её без роста working set-а.
    ReplaceExistingPrimary,
    /// В primary storage есть свободный slot.
    UseSparePrimarySlot,
    /// Normal hover может освободить старейший byproduct, не трогая current target.
    EvictOldestPrimaryByproduct,
}

/// Результат dry-run admission; не хранит и не освобождает lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineHoverPrepareAdmissionOutcome {
    Admitted {
        slot_plan: TimelineHoverPrepareSlotPlan,
    },
    NoOp {
        reason: TimelineHoverPrepareNoOpReason,
    },
}

/// Почему hover prepare не должен стартовать/материализоваться.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineHoverPrepareNoOpReason {
    ActiveLiveScrubSuspendsHoverPrepare,
    ProviderResourcePressure,
    NoSpareHoverSlot {
        capacity: usize,
        used_slots: usize,
        protected_key: TimelineHoverPrepareFrameKey,
    },
}

/// Итог insert-а, который уже получил prepared entry от provider-а.
pub enum TimelineHoverPrepareInsertOutcome<BranchToken> {
    Inserted {
        slot_plan: TimelineHoverPrepareSlotPlan,
        evicted_primary_byproducts: usize,
    },
    NoOp {
        entry: super::TimelineHoverPreparedFrameEntry<BranchToken>,
        reason: TimelineHoverPrepareNoOpReason,
    },
}

/// Один release step под provider/resource pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineHoverPreparePressureReleaseOutcome {
    ReleasedRecentSuperseded {
        released_key: TimelineHoverPrepareFrameKey,
    },
    ReleasedPrimaryByproduct {
        released_key: TimelineHoverPrepareFrameKey,
    },
    NothingReleased {
        reason: TimelineHoverPreparePressureReleaseMissReason,
    },
}

/// Почему pressure step не нашёл hover-owned entry, которую можно release-нуть.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineHoverPreparePressureReleaseMissReason {
    NoHoverOwnedEntries,
    OnlyProtectedCurrentTarget {
        protected_key: TimelineHoverPrepareFrameKey,
    },
}
