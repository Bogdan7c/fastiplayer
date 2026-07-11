use std::collections::BTreeMap;

use crate::diff::SettingsDiff;
use crate::error::{SettingsError, SettingsResult};
use crate::metadata::{
    SettingApplyMode, SettingGroupId, SettingId, SettingRouteId, SettingSectionId, SettingValue,
    SettingsSurfaceId,
};
use crate::registry::SettingsRegistry;

/// Monotonic generation of one runtime settings route.
///
/// Поколение хранится в core как нейтральное число: конкретный runtime сам решает,
/// когда внешний owner route действительно изменился.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RouteGeneration(u64);

impl RouteGeneration {
    /// Creates a generation value from an explicit counter.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw counter for diagnostics and tests.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }

    /// Returns the next generation without mutating the current value.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// Scope used by reset actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResetScope {
    /// Reset one field by stable setting id.
    Field(SettingId),

    /// Reset all resettable settings in one section/group pair.
    Group {
        /// Stable section id from placement metadata.
        section: SettingSectionId,

        /// Stable group id from placement metadata.
        group: SettingGroupId,
    },

    /// Reset all resettable settings assigned to one visual surface.
    Surface(SettingsSurfaceId),

    /// Replace the whole draft document with `D::default()`.
    All,
}

/// Result of a single draft edit.
#[derive(Debug, Clone, PartialEq)]
pub struct SetValueReport {
    /// Edited setting id.
    pub setting_id: SettingId,

    /// Owner-level route affected by the edit.
    pub route: SettingRouteId,

    /// Metadata-declared apply mode of the setting.
    pub apply_mode: SettingApplyMode,

    /// True when the draft value actually changed.
    pub draft_changed: bool,

    /// True when an `ImmediatePreview` route update was queued.
    pub preview_queued: bool,
}

/// Result of a reset action.
#[derive(Debug, Clone, PartialEq)]
pub struct ResetReport {
    /// Scope requested by the caller.
    pub scope: ResetScope,

    /// Registered settings whose draft value actually changed.
    pub affected_settings: Vec<SettingId>,

    /// Diff between committed and draft after the reset.
    pub changed_settings: SettingsDiff,

    /// Preview routes that received a coalesced pending update.
    pub preview_routes: Vec<SettingRouteId>,
}

/// Latest pending preview update for one route.
#[derive(Clone)]
pub struct PendingPreviewUpdate<D> {
    /// Owner-level route id.
    pub route: SettingRouteId,

    /// Full draft document at the moment of the latest coalesced update.
    pub document: D,

    /// Preview-capable settings currently relevant for this route update.
    pub affected_settings: Vec<SettingId>,

    /// Route generation observed when the edit transaction first touched route.
    pub baseline_generation: RouteGeneration,
}

/// Runtime preview state captured lazily for one route.
#[derive(Clone)]
pub struct PreviewRouteState<D> {
    /// Runtime document snapshot before the first preview edit for this route.
    pub baseline_document: D,

    /// Last document snapshot that preview runtime accepted for this route.
    pub active_document: D,

    /// Preview-capable settings touched by the route transaction.
    pub affected_settings: Vec<SettingId>,

    /// Route generation observed when the preview baseline was captured.
    pub baseline_generation: RouteGeneration,
}

/// Core-owned preview transaction state.
pub struct PreviewTransactionState<D> {
    routes: BTreeMap<SettingRouteId, PreviewRouteState<D>>,
    pending: BTreeMap<SettingRouteId, PendingPreviewUpdate<D>>,
}

impl<D> Default for PreviewTransactionState<D> {
    fn default() -> Self {
        Self {
            routes: BTreeMap::new(),
            pending: BTreeMap::new(),
        }
    }
}

impl<D> PreviewTransactionState<D> {
    /// Returns true when no route has captured a baseline and no update is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty() && self.pending.is_empty()
    }

    /// Returns the captured state for one preview route.
    #[must_use]
    pub fn route_state(&self, route: &SettingRouteId) -> Option<&PreviewRouteState<D>> {
        self.routes.get(route)
    }

    /// Returns latest pending preview update for one route.
    #[must_use]
    pub fn pending_update(&self, route: &SettingRouteId) -> Option<&PendingPreviewUpdate<D>> {
        self.pending.get(route)
    }

    /// Returns all pending preview route ids in stable order.
    #[must_use]
    pub fn pending_routes(&self) -> Vec<SettingRouteId> {
        self.pending.keys().cloned().collect()
    }

    fn clear(&mut self) {
        self.routes.clear();
        self.pending.clear();
    }
}

/// Report returned by document-level validation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    /// Settings checked by the validator.
    pub checked_settings: Vec<SettingId>,
}

impl ValidationReport {
    /// Creates a successful validation report.
    #[must_use]
    pub fn valid(checked_settings: Vec<SettingId>) -> Self {
        Self { checked_settings }
    }
}

/// Durability result produced by a persistence delegate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistReport {
    /// Main persistence outcome.
    pub outcome: PersistOutcome,

    /// Optional warning for non-fatal durability issues, such as directory fsync.
    pub durability_warning: Option<String>,
}

impl PersistReport {
    /// Creates a report for successfully persisted settings.
    #[must_use]
    pub const fn persisted() -> Self {
        Self {
            outcome: PersistOutcome::Persisted,
            durability_warning: None,
        }
    }

    /// Creates a report for a no-op persistence path.
    #[must_use]
    pub const fn skipped_no_changes() -> Self {
        Self {
            outcome: PersistOutcome::SkippedNoChanges,
            durability_warning: None,
        }
    }

    /// Adds a non-fatal durability warning to the report.
    #[must_use]
    pub fn with_durability_warning(mut self, warning: impl Into<String>) -> Self {
        self.durability_warning = Some(warning.into());
        self
    }
}

/// Persistence outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistOutcome {
    /// Draft document was durably saved.
    Persisted,

    /// Persistence was intentionally skipped because there were no changes.
    SkippedNoChanges,
}

/// Runtime mechanism used for one route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMechanism {
    /// Owner updated runtime state in-place.
    InPlace,

    /// Owner asked a worker to reconfigure itself.
    WorkerReconfigure,

    /// Owner rebuilt a pipeline while preserving its invariants.
    PipelineRebuild,

    /// Owner recreated renderer resources.
    RendererRecreate,

    /// Previously active preview became committed runtime state.
    PreviewPromoted,
}

/// Runtime result for one owner-level apply route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyRouteResult {
    /// Route fully applied the update.
    Applied,

    /// Route was already in the requested state.
    Noop,

    /// Active preview state became committed without extra runtime work.
    PreviewPromoted,

    /// Route applied only part of the update.
    PartialFailure { message: String },

    /// Route failed without applying the requested update.
    Failed { message: String },

    /// Runtime owner boundary is temporarily busy; the update was not queued.
    RuntimeBusy { activity: String },

    /// Route generation conflict blocked the update.
    Conflict {
        /// Generation captured when the transaction first touched the route.
        baseline: RouteGeneration,

        /// Current generation observed before apply.
        current: RouteGeneration,
    },
}

impl ApplyRouteResult {
    fn is_full_success(&self) -> bool {
        matches!(self, Self::Applied | Self::Noop | Self::PreviewPromoted)
    }
}

/// Report for one owner-level apply route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyRouteReport {
    /// Owner-level route id.
    pub route: SettingRouteId,

    /// Route result with explicit failure/conflict states.
    pub result: ApplyRouteResult,

    /// Mechanism chosen by the route owner.
    pub mechanism: ApplyMechanism,

    /// Concrete setting ids affected by this route update.
    pub affected_settings: Vec<SettingId>,
}

impl ApplyRouteReport {
    /// Creates a successful in-place route report.
    #[must_use]
    pub fn applied(route: SettingRouteId, affected_settings: Vec<SettingId>) -> Self {
        Self {
            route,
            result: ApplyRouteResult::Applied,
            mechanism: ApplyMechanism::InPlace,
            affected_settings,
        }
    }

    /// Creates a failed route report.
    #[must_use]
    pub fn failed(
        route: SettingRouteId,
        affected_settings: Vec<SettingId>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            route,
            result: ApplyRouteResult::Failed {
                message: message.into(),
            },
            mechanism: ApplyMechanism::InPlace,
            affected_settings,
        }
    }

    /// Creates a partial-failure route report.
    #[must_use]
    pub fn partial_failure(
        route: SettingRouteId,
        affected_settings: Vec<SettingId>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            route,
            result: ApplyRouteResult::PartialFailure {
                message: message.into(),
            },
            mechanism: ApplyMechanism::InPlace,
            affected_settings,
        }
    }
}

/// Final controller state after an apply attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyFinalState {
    /// Draft and committed were already identical.
    NoChanges,

    /// Validation delegate rejected the draft before persistence.
    ValidationFailed,

    /// Persistence failed after runtime commit, and runtime compensation succeeded.
    PersistFailed,

    /// Runtime preflight rejected the transaction before the first owner mutation.
    RuntimeBlocked,

    /// Runtime apply failed, and every completed owner mutation was compensated.
    RuntimeApplyFailed,

    /// Compensating rollback failed; the original error remains in the route reports.
    RollbackFailed,

    /// Route generation conflicts blocked persistence/runtime apply.
    BlockedByConflicts,

    /// Persisted document and runtime state are fully aligned.
    FullyApplied,
}

/// Route generation conflict captured in an apply report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteGenerationConflict {
    /// Conflicting route id.
    pub route: SettingRouteId,

    /// Generation captured when local draft first touched the route.
    pub baseline: RouteGeneration,

    /// Current route generation observed before apply.
    pub current: RouteGeneration,

    /// Settings that would have been applied on this route.
    pub affected_settings: Vec<SettingId>,
}

/// Full apply report suitable for UI status and diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct ApplyReport {
    /// Document-level validation result when validation was attempted.
    pub validation: Option<ValidationReport>,

    /// Persistence result when persistence was attempted.
    pub persistence: Option<PersistReport>,

    /// Owner-level route reports.
    pub routes: Vec<ApplyRouteReport>,

    /// Compensating rollback reports in the order they were executed.
    pub rollback: Vec<RollbackReport>,

    /// Diff between committed and draft at apply start.
    pub changed_settings: SettingsDiff,

    /// Explicit final state of the controller transaction.
    pub final_state: ApplyFinalState,

    /// Generation conflicts that blocked the transaction.
    pub conflicts: Vec<RouteGenerationConflict>,

    /// Delegate errors converted into report-visible diagnostics.
    pub errors: Vec<SettingsError>,
}

/// Request passed to the validation delegate.
pub struct ValidationRequest<'a, D> {
    /// Last durable committed document before the apply attempt.
    pub committed: &'a D,

    /// Draft document that should be validated.
    pub draft: &'a D,

    /// Changed registered settings between committed and draft.
    pub changed_settings: &'a SettingsDiff,
}

/// Request passed to the persistence delegate.
pub struct PersistRequest<'a, D> {
    /// Draft document that should be persisted.
    pub document: &'a D,

    /// Changed registered settings between committed and draft.
    pub changed_settings: &'a SettingsDiff,
}

/// One grouped route update passed to committed runtime apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteApplyUpdate {
    /// Owner-level route id.
    pub route: SettingRouteId,

    /// Concrete setting ids affected by this route.
    pub affected_settings: Vec<SettingId>,

    /// Generation captured when local draft first touched the route.
    pub baseline_generation: Option<RouteGeneration>,
}

/// Request passed to committed runtime apply delegate.
pub struct CommittedApplyRequest<'a, D> {
    /// Last fully committed document before this apply attempt.
    pub previous_committed: &'a D,

    /// Validated draft document that runtime should apply.
    pub requested: &'a D,

    /// Grouped owner-level route updates.
    pub route_updates: &'a [RouteApplyUpdate],

    /// Changed registered settings between committed and draft.
    pub changed_settings: &'a SettingsDiff,
}

/// Request passed to compensating committed-runtime rollback.
pub struct CommittedRollbackRequest<'a, D> {
    /// Last fully committed document that runtime must restore.
    pub previous_committed: &'a D,

    /// Draft document whose runtime mutations are being compensated.
    pub attempted: &'a D,

    /// Original grouped route updates; the delegate rolls completed owners back in reverse order.
    pub route_updates: &'a [RouteApplyUpdate],

    /// Changed registered settings between committed and draft.
    pub changed_settings: &'a SettingsDiff,
}

/// Request passed to preview apply delegate.
pub struct PreviewApplyRequest<'a, D> {
    /// Latest pending update for one route.
    pub update: &'a PendingPreviewUpdate<D>,
}

/// Result of a preview apply attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewApplyResult {
    /// Preview update became active.
    Applied,

    /// Runtime already had the requested preview state.
    Noop,

    /// Runtime is temporarily unable to accept the update; core keeps it queued.
    Backpressured,

    /// Route does not support this preview update.
    Unsupported { message: String },

    /// Runtime failed in a non-retryable way for this preview update.
    Fatal { message: String },
}

impl PreviewApplyResult {
    fn keeps_pending(&self) -> bool {
        matches!(self, Self::Backpressured)
    }

    fn makes_update_active(&self) -> bool {
        matches!(self, Self::Applied | Self::Noop)
    }
}

/// Report for a preview apply attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewApplyReport {
    /// Route that received the preview update.
    pub route: SettingRouteId,

    /// Preview result with explicit backpressure/error states.
    pub result: PreviewApplyResult,

    /// Settings included in the route update.
    pub affected_settings: Vec<SettingId>,
}

impl PreviewApplyReport {
    /// Creates a report for an applied preview update.
    #[must_use]
    pub fn applied(route: SettingRouteId, affected_settings: Vec<SettingId>) -> Self {
        Self {
            route,
            result: PreviewApplyResult::Applied,
            affected_settings,
        }
    }
}

/// Request passed to preview rollback delegate.
pub struct PreviewRollbackRequest<'a, D> {
    /// Route that owns the preview runtime state.
    pub route: &'a SettingRouteId,

    /// Baseline document captured before preview started for this route.
    pub baseline_document: &'a D,

    /// Last preview document accepted by runtime.
    pub active_document: &'a D,

    /// Settings touched by this preview route transaction.
    pub affected_settings: &'a [SettingId],

    /// Generation captured with the preview baseline.
    pub baseline_generation: RouteGeneration,
}

/// Rollback result for one preview route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackResult {
    /// Runtime rolled the route back to the captured baseline.
    RolledBack,

    /// Runtime was already at the baseline.
    Noop,

    /// Runtime owner could not restore its previous committed state.
    Failed { message: String },
}

/// Report for one preview rollback route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackReport {
    /// Route that was rolled back.
    pub route: SettingRouteId,

    /// Rollback result.
    pub result: RollbackResult,

    /// Settings touched by this preview route transaction.
    pub affected_settings: Vec<SettingId>,
}

impl RollbackReport {
    /// Creates a successful rollback report.
    #[must_use]
    pub fn rolled_back(route: SettingRouteId, affected_settings: Vec<SettingId>) -> Self {
        Self {
            route,
            result: RollbackResult::RolledBack,
            affected_settings,
        }
    }

    /// Creates a failed rollback report without hiding the original apply error.
    #[must_use]
    pub fn failed(
        route: SettingRouteId,
        affected_settings: Vec<SettingId>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            route,
            result: RollbackResult::Failed {
                message: message.into(),
            },
            affected_settings,
        }
    }
}

/// Report returned by cancel.
#[derive(Debug, Clone, PartialEq)]
pub struct CancelReport {
    /// Diff discarded from draft.
    pub discarded_changes: SettingsDiff,

    /// Rollback reports for preview routes with captured baselines.
    pub rolled_back_routes: Vec<RollbackReport>,
}

/// Delegate responsible for authoritative document validation.
pub trait SettingsValidator<D> {
    /// Validates the draft before persistence/runtime apply.
    fn validate(&mut self, request: ValidationRequest<'_, D>) -> SettingsResult<ValidationReport>;
}

/// Delegate responsible for durable settings persistence.
pub trait SettingsPersister<D> {
    /// Persists the validated draft document.
    fn persist(&mut self, request: PersistRequest<'_, D>) -> SettingsResult<PersistReport>;
}

/// Delegate responsible for committed runtime apply.
pub trait CommittedSettingsApplier<D> {
    /// Checks every affected owner before the first mutation.
    fn preflight_committed(
        &mut self,
        request: CommittedApplyRequest<'_, D>,
    ) -> SettingsResult<Vec<ApplyRouteReport>>;

    /// Applies grouped route updates to concrete runtime owners.
    fn apply_committed(
        &mut self,
        request: CommittedApplyRequest<'_, D>,
    ) -> SettingsResult<Vec<ApplyRouteReport>>;

    /// Restores completed owner mutations in reverse order.
    fn rollback_committed(
        &mut self,
        request: CommittedRollbackRequest<'_, D>,
    ) -> SettingsResult<Vec<RollbackReport>>;
}

/// Delegate responsible for sending live preview updates.
pub trait PreviewSettingsApplier<D> {
    /// Applies one pending preview route update.
    fn apply_preview(
        &mut self,
        request: PreviewApplyRequest<'_, D>,
    ) -> SettingsResult<PreviewApplyReport>;
}

/// Delegate responsible for rolling preview routes back to captured baselines.
pub trait PreviewRollbacker<D> {
    /// Rolls one preview route back.
    fn rollback_preview(
        &mut self,
        request: PreviewRollbackRequest<'_, D>,
    ) -> SettingsResult<RollbackReport>;
}

/// Neutral settings transaction controller.
pub struct SettingsController<D>
where
    D: Clone + Default,
{
    committed: D,
    draft: D,
    default_document: D,
    registry: SettingsRegistry<D>,
    preview: PreviewTransactionState<D>,
    route_generation_baselines: BTreeMap<SettingRouteId, RouteGeneration>,
    route_generations: BTreeMap<SettingRouteId, RouteGeneration>,
}

impl<D> SettingsController<D>
where
    D: Clone + Default,
{
    /// Creates a controller with `D::default()` as reset source.
    #[must_use]
    pub fn new(committed: D, registry: SettingsRegistry<D>) -> Self {
        Self::with_default_document(committed, D::default(), registry)
    }

    /// Creates a controller with an explicit default document.
    #[must_use]
    pub fn with_default_document(
        committed: D,
        default_document: D,
        registry: SettingsRegistry<D>,
    ) -> Self {
        Self {
            draft: committed.clone(),
            committed,
            default_document,
            registry,
            preview: PreviewTransactionState::default(),
            route_generation_baselines: BTreeMap::new(),
            route_generations: BTreeMap::new(),
        }
    }

    /// Returns the durable committed document.
    #[must_use]
    pub fn committed(&self) -> &D {
        &self.committed
    }

    /// Returns the current draft document.
    #[must_use]
    pub fn draft(&self) -> &D {
        &self.draft
    }

    /// Returns the settings registry owned by this controller.
    #[must_use]
    pub fn registry(&self) -> &SettingsRegistry<D> {
        &self.registry
    }

    /// Returns the preview transaction state.
    #[must_use]
    pub fn preview(&self) -> &PreviewTransactionState<D> {
        &self.preview
    }

    /// Starts a fresh edit transaction from the current committed document.
    pub fn begin_edit(&mut self) {
        self.draft = self.committed.clone();
        self.preview.clear();
        self.route_generation_baselines.clear();
    }

    /// Returns the current known generation for one route.
    #[must_use]
    pub fn route_generation(&self, route: &SettingRouteId) -> RouteGeneration {
        self.route_generations
            .get(route)
            .copied()
            .unwrap_or_default()
    }

    /// Updates the current known generation for one route.
    pub fn set_route_generation(&mut self, route: SettingRouteId, generation: RouteGeneration) {
        self.route_generations.insert(route, generation);
    }

    /// Returns the captured generation baseline for one touched route.
    #[must_use]
    pub fn route_generation_baseline(&self, route: &SettingRouteId) -> Option<RouteGeneration> {
        self.route_generation_baselines.get(route).copied()
    }

    /// Edits one draft value through the registry accessor boundary.
    pub fn set_value(
        &mut self,
        id: SettingId,
        value: SettingValue,
    ) -> SettingsResult<SetValueReport> {
        let descriptor = self
            .registry
            .descriptor(&id)
            .ok_or_else(|| SettingsError::UnknownSetting { id: id.clone() })?;
        let route = descriptor.route.clone();
        let apply_mode = descriptor.apply_mode;
        let previous_draft = self.draft.clone();

        self.registry.set_value(&mut self.draft, &id, value)?;

        let draft_changed = self.value_changed_between(&previous_draft, &self.draft, &id)?;
        if draft_changed {
            self.capture_route_generation_baseline(&route);
        }

        let preview_queued = draft_changed
            && apply_mode == SettingApplyMode::ImmediatePreview
            && self.queue_preview_update_for_route(&route, vec![id.clone()])?;

        Ok(SetValueReport {
            setting_id: id,
            route,
            apply_mode,
            draft_changed,
            preview_queued,
        })
    }

    /// Resets one field from the default document.
    pub fn reset_value(&mut self, id: SettingId) -> SettingsResult<ResetReport> {
        self.reset_scope(ResetScope::Field(id))
    }

    /// Resets all resettable settings in one section/group pair.
    pub fn reset_group(
        &mut self,
        section: SettingSectionId,
        group: SettingGroupId,
    ) -> SettingsResult<ResetReport> {
        self.reset_scope(ResetScope::Group { section, group })
    }

    /// Resets all resettable settings on one visual surface.
    pub fn reset_surface(&mut self, surface: SettingsSurfaceId) -> SettingsResult<ResetReport> {
        self.reset_scope(ResetScope::Surface(surface))
    }

    /// Replaces the whole draft document with the default document.
    pub fn reset_all(&mut self) -> SettingsResult<ResetReport> {
        self.reset_scope(ResetScope::All)
    }

    /// Resets draft values according to the requested scope.
    pub fn reset_scope(&mut self, scope: ResetScope) -> SettingsResult<ResetReport> {
        let previous_draft = self.draft.clone();

        match &scope {
            ResetScope::Field(id) => {
                self.registry
                    .reset_value(&mut self.draft, &self.default_document, id)?;
            }
            ResetScope::Group { section, group } => {
                for id in self.resettable_ids_for_group(section, group) {
                    self.registry
                        .reset_value(&mut self.draft, &self.default_document, &id)?;
                }
            }
            ResetScope::Surface(surface) => {
                for id in self.resettable_ids_for_surface(surface) {
                    self.registry
                        .reset_value(&mut self.draft, &self.default_document, &id)?;
                }
            }
            ResetScope::All => {
                self.draft = self.default_document.clone();
            }
        }

        let affected_settings = self.changed_setting_ids_between(&previous_draft, &self.draft)?;
        let preview_routes = self.capture_routes_and_queue_preview(&affected_settings)?;
        let changed_settings = self.diff()?;

        Ok(ResetReport {
            scope,
            affected_settings,
            changed_settings,
            preview_routes,
        })
    }

    /// Returns current diff between committed and draft.
    pub fn diff(&self) -> SettingsResult<SettingsDiff> {
        self.registry.diff(&self.committed, &self.draft)
    }

    /// Sends one pending preview update through the runtime delegate.
    pub fn apply_pending_preview<P>(
        &mut self,
        route: &SettingRouteId,
        previewer: &mut P,
    ) -> SettingsResult<Option<PreviewApplyReport>>
    where
        P: PreviewSettingsApplier<D>,
    {
        let Some(update) = self.preview.pending.get(route).cloned() else {
            return Ok(None);
        };

        let report = previewer.apply_preview(PreviewApplyRequest { update: &update })?;
        if !report.result.keeps_pending() {
            self.preview.pending.remove(route);
        }
        if report.result.makes_update_active()
            && let Some(route_state) = self.preview.routes.get_mut(route)
        {
            route_state.active_document = update.document;
            route_state.affected_settings = update.affected_settings.clone();
        }

        Ok(Some(report))
    }

    /// Applies the draft through validate -> preflight -> runtime commit -> persist.
    pub fn apply<A>(&mut self, delegate: &mut A) -> SettingsResult<ApplyReport>
    where
        A: SettingsValidator<D> + SettingsPersister<D> + CommittedSettingsApplier<D>,
    {
        let changed_settings = self.diff()?;
        if changed_settings.is_empty() {
            return Ok(ApplyReport {
                validation: None,
                persistence: Some(PersistReport::skipped_no_changes()),
                routes: Vec::new(),
                rollback: Vec::new(),
                changed_settings,
                final_state: ApplyFinalState::NoChanges,
                conflicts: Vec::new(),
                errors: Vec::new(),
            });
        }

        let route_updates = self.route_updates_for_diff(&changed_settings)?;
        let conflicts = self.generation_conflicts(&route_updates);
        if !conflicts.is_empty() {
            let routes = conflicts
                .iter()
                .map(|conflict| ApplyRouteReport {
                    route: conflict.route.clone(),
                    result: ApplyRouteResult::Conflict {
                        baseline: conflict.baseline,
                        current: conflict.current,
                    },
                    mechanism: ApplyMechanism::InPlace,
                    affected_settings: conflict.affected_settings.clone(),
                })
                .collect();
            return Ok(ApplyReport {
                validation: None,
                persistence: None,
                routes,
                rollback: Vec::new(),
                changed_settings,
                final_state: ApplyFinalState::BlockedByConflicts,
                conflicts,
                errors: Vec::new(),
            });
        }

        let validation = match delegate.validate(ValidationRequest {
            committed: &self.committed,
            draft: &self.draft,
            changed_settings: &changed_settings,
        }) {
            Ok(report) => report,
            Err(error) => {
                return Ok(ApplyReport {
                    validation: None,
                    persistence: None,
                    routes: Vec::new(),
                    rollback: Vec::new(),
                    changed_settings,
                    final_state: ApplyFinalState::ValidationFailed,
                    conflicts: Vec::new(),
                    errors: vec![error],
                });
            }
        };

        let apply_request = CommittedApplyRequest {
            previous_committed: &self.committed,
            requested: &self.draft,
            route_updates: &route_updates,
            changed_settings: &changed_settings,
        };
        let preflight_routes = match delegate.preflight_committed(apply_request) {
            Ok(reports) => reports,
            Err(error) => {
                return Ok(ApplyReport {
                    validation: Some(validation),
                    persistence: None,
                    routes: Vec::new(),
                    rollback: Vec::new(),
                    changed_settings,
                    final_state: ApplyFinalState::RuntimeBlocked,
                    conflicts: Vec::new(),
                    errors: vec![error],
                });
            }
        };
        if !preflight_routes.is_empty() {
            return Ok(ApplyReport {
                validation: Some(validation),
                persistence: None,
                routes: preflight_routes,
                rollback: Vec::new(),
                changed_settings,
                final_state: ApplyFinalState::RuntimeBlocked,
                conflicts: Vec::new(),
                errors: Vec::new(),
            });
        }

        let mut errors = Vec::new();
        let routes = match delegate.apply_committed(CommittedApplyRequest {
            previous_committed: &self.committed,
            requested: &self.draft,
            route_updates: &route_updates,
            changed_settings: &changed_settings,
        }) {
            Ok(routes) => routes,
            Err(error) => {
                let message = error.to_string();
                errors.push(error);
                route_updates
                    .iter()
                    .map(|update| {
                        ApplyRouteReport::failed(
                            update.route.clone(),
                            update.affected_settings.clone(),
                            message.clone(),
                        )
                    })
                    .collect()
            }
        };

        let runtime_commit_succeeded =
            !routes.is_empty() && routes.iter().all(|report| report.result.is_full_success());
        if !runtime_commit_succeeded {
            let (rollback, rollback_delegate_failed) =
                match delegate.rollback_committed(CommittedRollbackRequest {
                    previous_committed: &self.committed,
                    attempted: &self.draft,
                    route_updates: &route_updates,
                    changed_settings: &changed_settings,
                }) {
                    Ok(reports) => (reports, false),
                    Err(error) => {
                        errors.push(error);
                        (Vec::new(), true)
                    }
                };
            let rollback_failed = rollback_delegate_failed
                || rollback
                    .iter()
                    .any(|report| matches!(report.result, RollbackResult::Failed { .. }));
            return Ok(ApplyReport {
                validation: Some(validation),
                persistence: None,
                routes,
                rollback,
                changed_settings,
                final_state: if rollback_failed {
                    ApplyFinalState::RollbackFailed
                } else {
                    ApplyFinalState::RuntimeApplyFailed
                },
                conflicts: Vec::new(),
                errors,
            });
        }

        let persistence = match delegate.persist(PersistRequest {
            document: &self.draft,
            changed_settings: &changed_settings,
        }) {
            Ok(report) => report,
            Err(persist_error) => {
                errors.push(persist_error);
                let (rollback, rollback_delegate_failed) =
                    match delegate.rollback_committed(CommittedRollbackRequest {
                        previous_committed: &self.committed,
                        attempted: &self.draft,
                        route_updates: &route_updates,
                        changed_settings: &changed_settings,
                    }) {
                        Ok(reports) => (reports, false),
                        Err(rollback_error) => {
                            errors.push(rollback_error);
                            (Vec::new(), true)
                        }
                    };
                let rollback_failed = rollback_delegate_failed
                    || rollback
                        .iter()
                        .any(|report| matches!(report.result, RollbackResult::Failed { .. }));
                return Ok(ApplyReport {
                    validation: Some(validation),
                    persistence: None,
                    routes,
                    rollback,
                    changed_settings,
                    final_state: if rollback_failed {
                        ApplyFinalState::RollbackFailed
                    } else {
                        ApplyFinalState::PersistFailed
                    },
                    conflicts: Vec::new(),
                    errors,
                });
            }
        };

        self.commit_full_success(route_updates.iter().map(|update| update.route.clone()));
        Ok(ApplyReport {
            validation: Some(validation),
            persistence: Some(persistence),
            routes,
            rollback: Vec::new(),
            changed_settings,
            final_state: ApplyFinalState::FullyApplied,
            conflicts: Vec::new(),
            errors,
        })
    }

    /// Cancels the draft transaction and rolls back captured preview routes.
    pub fn cancel<R>(&mut self, rollbacker: &mut R) -> SettingsResult<CancelReport>
    where
        R: PreviewRollbacker<D>,
    {
        let discarded_changes = self.diff()?;
        let mut rolled_back_routes = Vec::new();

        for (route, route_state) in &self.preview.routes {
            let rollback_report = rollbacker.rollback_preview(PreviewRollbackRequest {
                route,
                baseline_document: &route_state.baseline_document,
                active_document: &route_state.active_document,
                affected_settings: &route_state.affected_settings,
                baseline_generation: route_state.baseline_generation,
            })?;
            rolled_back_routes.push(rollback_report);
        }

        self.draft = self.committed.clone();
        self.preview.clear();
        self.route_generation_baselines.clear();

        Ok(CancelReport {
            discarded_changes,
            rolled_back_routes,
        })
    }

    fn value_changed_between(&self, before: &D, after: &D, id: &SettingId) -> SettingsResult<bool> {
        Ok(self.registry.get_value(before, id)? != self.registry.get_value(after, id)?)
    }

    fn changed_setting_ids_between(&self, before: &D, after: &D) -> SettingsResult<Vec<SettingId>> {
        Ok(self
            .registry
            .diff(before, after)?
            .into_changes()
            .into_iter()
            .map(|change| change.id)
            .collect())
    }

    fn resettable_ids_for_group(
        &self,
        section: &SettingSectionId,
        group: &SettingGroupId,
    ) -> Vec<SettingId> {
        self.registry
            .descriptors()
            .filter(|descriptor| {
                descriptor.placement.section == *section
                    && descriptor.placement.group == *group
                    && descriptor.access.permits_write()
                    && descriptor.default_behavior.allows_reset()
            })
            .map(|descriptor| descriptor.id.clone())
            .collect()
    }

    fn resettable_ids_for_surface(&self, surface: &SettingsSurfaceId) -> Vec<SettingId> {
        self.registry
            .descriptors()
            .filter(|descriptor| {
                descriptor.placement.preferred_surface == *surface
                    && descriptor.access.permits_write()
                    && descriptor.default_behavior.allows_reset()
            })
            .map(|descriptor| descriptor.id.clone())
            .collect()
    }

    fn capture_routes_and_queue_preview(
        &mut self,
        affected_settings: &[SettingId],
    ) -> SettingsResult<Vec<SettingRouteId>> {
        let mut preview_routes = Vec::new();
        let mut affected_by_route: BTreeMap<SettingRouteId, Vec<SettingId>> = BTreeMap::new();

        for setting_id in affected_settings {
            let descriptor = self.registry.descriptor(setting_id).ok_or_else(|| {
                SettingsError::UnknownSetting {
                    id: setting_id.clone(),
                }
            })?;
            let route = descriptor.route.clone();
            let apply_mode = descriptor.apply_mode;

            self.capture_route_generation_baseline(&route);
            if apply_mode == SettingApplyMode::ImmediatePreview {
                push_unique(
                    affected_by_route.entry(route).or_default(),
                    setting_id.clone(),
                );
            }
        }

        for (route, fallback_affected_settings) in affected_by_route {
            if self.queue_preview_update_for_route(&route, fallback_affected_settings)? {
                preview_routes.push(route);
            }
        }

        Ok(preview_routes)
    }

    fn queue_preview_update_for_route(
        &mut self,
        route: &SettingRouteId,
        fallback_affected_settings: Vec<SettingId>,
    ) -> SettingsResult<bool> {
        self.capture_preview_baseline_if_missing(route);
        let affected_settings =
            self.preview_affected_settings_for_route(route, fallback_affected_settings)?;
        let baseline_generation = self
            .route_generation_baselines
            .get(route)
            .copied()
            .unwrap_or_default();

        if let Some(route_state) = self.preview.routes.get_mut(route) {
            for setting_id in &affected_settings {
                push_unique(&mut route_state.affected_settings, setting_id.clone());
            }
        }

        self.preview.pending.insert(
            route.clone(),
            PendingPreviewUpdate {
                route: route.clone(),
                document: self.draft.clone(),
                affected_settings,
                baseline_generation,
            },
        );
        Ok(true)
    }

    fn capture_preview_baseline_if_missing(&mut self, route: &SettingRouteId) {
        if self.preview.routes.contains_key(route) {
            return;
        }

        let baseline_generation = self.route_generation(route);
        let baseline_document = self.runtime_document_for_preview_baseline();
        self.preview.routes.insert(
            route.clone(),
            PreviewRouteState {
                baseline_document: baseline_document.clone(),
                active_document: baseline_document,
                affected_settings: Vec::new(),
                baseline_generation,
            },
        );
    }

    fn runtime_document_for_preview_baseline(&self) -> D {
        self.committed.clone()
    }

    fn preview_affected_settings_for_route(
        &self,
        route: &SettingRouteId,
        fallback_affected_settings: Vec<SettingId>,
    ) -> SettingsResult<Vec<SettingId>> {
        let mut affected_settings = Vec::new();
        for change in self.diff()?.into_changes() {
            let descriptor = self.registry.descriptor(&change.id).ok_or_else(|| {
                SettingsError::UnknownSetting {
                    id: change.id.clone(),
                }
            })?;
            if descriptor.route == *route
                && descriptor.apply_mode == SettingApplyMode::ImmediatePreview
            {
                push_unique(&mut affected_settings, change.id);
            }
        }

        if affected_settings.is_empty() {
            affected_settings = fallback_affected_settings;
        }
        Ok(affected_settings)
    }

    fn capture_route_generation_baseline(&mut self, route: &SettingRouteId) {
        if self.route_generation_baselines.contains_key(route) {
            return;
        }

        self.route_generation_baselines
            .insert(route.clone(), self.route_generation(route));
    }

    fn route_updates_for_diff(&self, diff: &SettingsDiff) -> SettingsResult<Vec<RouteApplyUpdate>> {
        let mut route_updates: BTreeMap<SettingRouteId, Vec<SettingId>> = BTreeMap::new();
        for change in diff.changes() {
            let descriptor = self.registry.descriptor(&change.id).ok_or_else(|| {
                SettingsError::UnknownSetting {
                    id: change.id.clone(),
                }
            })?;
            push_unique(
                route_updates.entry(descriptor.route.clone()).or_default(),
                change.id.clone(),
            );
        }

        Ok(route_updates
            .into_iter()
            .map(|(route, affected_settings)| RouteApplyUpdate {
                baseline_generation: self.route_generation_baselines.get(&route).copied(),
                route,
                affected_settings,
            })
            .collect())
    }

    fn generation_conflicts(
        &self,
        route_updates: &[RouteApplyUpdate],
    ) -> Vec<RouteGenerationConflict> {
        route_updates
            .iter()
            .filter_map(|update| {
                let baseline = update.baseline_generation?;
                let current = self.route_generation(&update.route);
                (baseline != current).then(|| RouteGenerationConflict {
                    route: update.route.clone(),
                    baseline,
                    current,
                    affected_settings: update.affected_settings.clone(),
                })
            })
            .collect()
    }

    fn commit_full_success(&mut self, routes: impl Iterator<Item = SettingRouteId>) {
        self.committed = self.draft.clone();
        self.preview.clear();
        self.route_generation_baselines.clear();

        for route in routes {
            let next_generation = self.route_generation(&route).next();
            self.route_generations.insert(route, next_generation);
        }
    }
}

fn push_unique<T>(values: &mut Vec<T>, value: T)
where
    T: PartialEq,
{
    if !values.contains(&value) {
        values.push(value);
    }
}

trait DefaultBehaviorResetExt {
    fn allows_reset(self) -> bool;
}

impl DefaultBehaviorResetExt for crate::metadata::DefaultBehavior {
    fn allows_reset(self) -> bool {
        matches!(self, Self::FromDefaultDocument)
    }
}
