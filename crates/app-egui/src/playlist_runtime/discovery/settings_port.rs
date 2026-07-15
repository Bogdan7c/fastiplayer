//! Production D62 settings transaction port для exact discovery scope.

use std::sync::{Arc, Mutex};

use playlist_discovery::{DiscoveryCancellationCause, DiscoveryJobHandle};

use super::SiblingDiscoveryScopeId;
use crate::playlist_runtime::settings::{FrozenDiscoveryScope, PlaylistDiscoverySettingsPort};

/// Intent-фаза исключает противоречивую комбинацию `Option<Job>` и positional bool.
pub(super) enum DiscoverySettingsTarget {
    /// Directory snapshot ещё строится; freeze запрещает его последующий admission.
    ManifestPending,
    /// Probe job уже принят executor-ом и владеет admission buffer-ом.
    ActiveJob(DiscoveryJobHandle),
}

/// Shared owner позволяет transaction port-у управлять exact process-lifetime scope.
#[derive(Clone, Default)]
pub(super) struct SharedDiscoverySettingsControl {
    inner: Arc<Mutex<DiscoverySettingsControl>>,
}

#[derive(Default)]
struct DiscoverySettingsControl {
    scope_id: Option<SiblingDiscoveryScopeId>,
    target: Option<DiscoverySettingsTarget>,
    frozen: bool,
    finalized: bool,
}

struct ProductionDiscoverySettingsPort {
    control: SharedDiscoverySettingsControl,
}

impl SharedDiscoverySettingsControl {
    /// Создаёт отдельный transaction adapter над тем же exact control state.
    pub(super) fn port(&self) -> Box<dyn PlaylistDiscoverySettingsPort> {
        Box::new(ProductionDiscoverySettingsPort {
            control: self.clone(),
        })
    }

    /// Публикует новую intent-фазу exact scope.
    pub(super) fn update(
        &self,
        scope_id: SiblingDiscoveryScopeId,
        target: DiscoverySettingsTarget,
    ) {
        let mut control = self.lock_recovering_poison();
        control.scope_id = Some(scope_id);
        control.target = Some(target);
        control.finalized = false;
    }

    /// Очищает только matching scope, не затрагивая более новый scan.
    pub(super) fn clear(&self, scope_id: SiblingDiscoveryScopeId) {
        let mut control = self.lock_recovering_poison();
        if control.scope_id == Some(scope_id) {
            *control = DiscoverySettingsControl::default();
        }
    }

    /// Manifest result не должен перейти к executor-у после post-persist finalize.
    pub(super) fn is_finalized(&self) -> bool {
        self.lock_recovering_poison().finalized
    }

    /// Job, появившийся после manifest race, наследует staged freeze.
    pub(super) fn is_frozen(&self) -> bool {
        self.lock_recovering_poison().frozen
    }

    fn lock_recovering_poison(&self) -> std::sync::MutexGuard<'_, DiscoverySettingsControl> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl PlaylistDiscoverySettingsPort for ProductionDiscoverySettingsPort {
    fn freeze_active_scan(&mut self) -> Result<Option<FrozenDiscoveryScope>, String> {
        let mut control = self
            .control
            .inner
            .lock()
            .map_err(|_| "discovery settings control poisoned".to_owned())?;
        let Some(scope_id) = control.scope_id else {
            return Ok(None);
        };
        if let Some(DiscoverySettingsTarget::ActiveJob(job)) = &control.target
            && !job.freeze_admission()
        {
            return Err("active discovery job cannot freeze admission".to_owned());
        }
        control.frozen = true;
        Ok(Some(FrozenDiscoveryScope::new(scope_id.get())))
    }

    fn resume_frozen_scan(&mut self, scope: FrozenDiscoveryScope) -> Result<(), String> {
        let mut control = self
            .control
            .inner
            .lock()
            .map_err(|_| "discovery settings control poisoned".to_owned())?;
        if control.scope_id.map(SiblingDiscoveryScopeId::get) != Some(scope.get())
            || control.finalized
        {
            return Err("stale frozen discovery scope".to_owned());
        }
        if let Some(DiscoverySettingsTarget::ActiveJob(job)) = &control.target
            && !job.resume_admission()
        {
            return Err("frozen discovery job cannot resume admission".to_owned());
        }
        control.frozen = false;
        Ok(())
    }

    fn finalize_cancel_frozen_scan(&mut self, scope: FrozenDiscoveryScope) {
        let mut control = self.control.lock_recovering_poison();
        if control.scope_id.map(SiblingDiscoveryScopeId::get) != Some(scope.get()) {
            return;
        }
        if let Some(DiscoverySettingsTarget::ActiveJob(job)) = &control.target {
            let _cancelled_now = job.cancel(DiscoveryCancellationCause::StructuralInvalidation);
        }
        control.finalized = true;
        control.frozen = false;
    }
}
