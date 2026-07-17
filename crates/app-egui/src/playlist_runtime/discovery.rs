//! Target-first local replacement и process-lifetime sibling discovery orchestration.

mod action_api;
#[allow(
    dead_code,
    reason = "Session 16 action API is rendered by Session 19 UI"
)]
mod action_jobs;
mod initial_playback;
mod installed_target;
mod manifest_worker;
mod mapping;
mod metadata_sort;
mod navigation;
mod settings_port;
mod youtube_metadata;

#[allow(
    unused_imports,
    reason = "Session 19 consumes Session 16 job read models"
)]
pub(crate) use action_jobs::{
    ManualAddJobId, ManualAddStartError, PlaylistDiscoveryJobsReadModel,
    VisibleRefreshRequestOutcome,
};
pub(crate) use metadata_sort::{
    MetadataSortCancelOutcome, MetadataSortJobId, MetadataSortPhase, MetadataSortTerminalOutcome,
};
pub(in crate::playlist_runtime) use youtube_metadata::YoutubeMetadataDemand;
#[cfg(test)]
pub(in crate::playlist_runtime) use youtube_metadata::{
    YoutubeMetadataResolver, YoutubeMetadataTaskOutcome,
};

#[allow(unused_imports)]
pub(crate) use navigation::{PlaylistDiscoveryNavigationAction, PlaylistDiscoveryNavigationStatus};

use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use player_core::{MediaInstallRequestId, PlaybackIntentRevision};
use playlist_core::{PlaylistItemDraft, PlaylistItemId, ReservedQueueMutation};
use playlist_discovery::{
    AdmissionAckOutcome, AdmissionBatchId, BatchApplySemantics, DirectoryManifest,
    DirectoryManifestBuildError, DiscoveryCancellationCause, DiscoveryEvent, DiscoveryExecutor,
    DiscoveryFinalOutcome, DiscoveryJobHandle, DiscoveryRecord, DiscoveryRecordKey,
    DiscoveryRequest, DiscoveryRequestRevision, DiscoveryWakePort, LocalMediaKind,
    ManifestCandidateKey, RawManifestLimitReached, ReprioritizeHint,
    SiblingDiscoveryPolicySnapshot, SiblingDiscoveryRequest, SiblingPolicyRevision,
    WakeDisconnected,
};

use super::controller::{
    DiscoveryContinuation, InitialQueuePlaybackGuard, InstallReadyOutcome, PlaylistInstallMutation,
    PlaylistInstallRequest, SiblingDiscoveryScopeId,
};
use super::identity::PendingTargetOrigin;
use super::settings::{FutureDiscoveryPolicy, PlaylistDiscoverySettingsPort};
use super::{PlaylistMediaOpenGateError, PlaylistRuntime};
use crate::app_wake::{AppWakePort, WakeDelivery};
use crate::media_open::{AuthorizationDispatchResolution, MediaOpenRequestId};
pub(crate) use initial_playback::InstalledTargetDiscoveryStartError;
use manifest_worker::{ManifestWork, ManifestWorker};
pub(crate) use mapping::cached_metadata;
pub(crate) use mapping::target_draft_from_prepared;
use mapping::{
    batch_matches, draft_from_record, insertion_anchor, manifest_priority_hint, sibling_filter,
};
use settings_port::{DiscoverySettingsTarget, SharedDiscoverySettingsControl};

impl PlaylistRuntime {
    /// Связывает exact player staging request с target-only D08 mutation.
    pub(crate) fn accept_explicit_target_install(
        &mut self,
        request_id: MediaOpenRequestId,
        player_request_id: MediaInstallRequestId,
        target_draft: PlaylistItemDraft,
        intent_revision: PlaybackIntentRevision,
    ) -> Result<(), PlaylistMediaOpenGateError> {
        let controller = self
            .controller
            .as_mut()
            .ok_or(PlaylistMediaOpenGateError::LoadDecisionPending)?;
        let expected_queue_revision = controller.queue().revision_snapshot();
        controller
            .accept_install_request(PlaylistInstallRequest {
                request_id,
                player_request_id,
                target_item_id: None,
                origin: PendingTargetOrigin::ExplicitOpen,
                intent_revision,
                expected_queue_revision,
                mutation: PlaylistInstallMutation::Reserved(
                    ReservedQueueMutation::replace_with_current(
                        Vec::new(),
                        target_draft,
                        Vec::new(),
                    ),
                ),
            })
            .map_err(PlaylistMediaOpenGateError::InstallAdmission)
    }

    /// Проверяет, принадлежит ли Ready exact controller-guarded install-у.
    pub(crate) fn playlist_install_matches(&self, request_id: MediaOpenRequestId) -> bool {
        self.controller
            .as_ref()
            .and_then(|controller| controller.install_request_id())
            == Some(request_id)
    }

    /// Ready -> fallible reservation -> dispatch -> authoritative resolution.
    pub(crate) fn authorize_ready_target_install(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> Result<AuthorizationDispatchResolution, PlaylistMediaOpenGateError> {
        let ready = self
            .controller
            .as_mut()
            .ok_or(PlaylistMediaOpenGateError::LoadDecisionPending)?
            .on_ready_to_commit(request_id);
        match ready {
            InstallReadyOutcome::RequestAuthorization {
                request_id: ready_request,
            } if ready_request == request_id => {}
            InstallReadyOutcome::ReservationRejected { error, .. } => {
                return Err(PlaylistMediaOpenGateError::InstallReservation(error));
            }
            InstallReadyOutcome::Fatal(violation) => {
                return Err(PlaylistMediaOpenGateError::ControllerInvariant(violation));
            }
            _ => {
                return Err(PlaylistMediaOpenGateError::ControllerInvariant(
                    super::controller::PlaylistControllerInvariantViolation::StaleReadyToCommit,
                ));
            }
        }
        self.controller
            .as_mut()
            .expect("controller checked above")
            .begin_authorization_dispatch(request_id)
            .map_err(PlaylistMediaOpenGateError::ControllerInvariant)?;
        let authorization = self.media_open.authorize_ready(request_id);
        let resolution = match &authorization {
            Ok(resolution) => *resolution,
            Err(_) => self
                .media_open
                .snapshot()
                .and_then(|snapshot| snapshot.authorization_resolution)
                .ok_or(PlaylistMediaOpenGateError::ControllerInvariant(
                    super::controller::PlaylistControllerInvariantViolation::MissingAuthorizationResolution,
                ))?,
        };
        self.controller
            .as_mut()
            .expect("controller remains installed")
            .resolve_authorization_dispatch(request_id, resolution)
            .map_err(PlaylistMediaOpenGateError::ControllerInvariant)?;
        authorization
            .map(|_| resolution)
            .map_err(PlaylistMediaOpenGateError::Coordinator)
    }

    /// Event-driven drain; каждый accepted batch отдельно публикует dirty/save revision.
    pub(crate) fn drain_playlist_discovery(&mut self) -> bool {
        let visible = {
            let Some(controller) = self.controller.as_mut() else {
                return false;
            };
            let dirty_before = controller.dirty_revision();
            let visible = self
                .discovery
                .drain(controller, self.manual_add_queue_generation.value());
            let dirty_after = controller.dirty_revision();
            if dirty_after != dirty_before {
                self.publish_controller_mutation_if_dirty(dirty_before);
            }
            visible
        };
        visible | self.drain_metadata_sort()
    }

    #[allow(dead_code)]
    pub(crate) fn playlist_discovery_status(&self) -> &PlaylistDiscoveryStatus {
        self.discovery.status()
    }

    #[allow(dead_code)]
    pub(crate) fn playlist_discovery_insertion_hint(
        &self,
    ) -> Option<&PlaylistDiscoveryInsertionHint> {
        self.discovery.last_insertion_hint()
    }
}

/// Bounded UI-facing summary без filesystem paths и unbounded diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlaylistDiscoveryStatus {
    Idle,
    Enumerating {
        scope_id: SiblingDiscoveryScopeId,
    },
    Probing {
        scope_id: SiblingDiscoveryScopeId,
        processed: usize,
        total: usize,
    },
    Completed {
        scope_id: SiblingDiscoveryScopeId,
        outcome: DiscoveryFinalOutcome,
    },
    TargetOnlyWarning {
        scope_id: SiblingDiscoveryScopeId,
        warning: PlaylistDiscoveryWarning,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlaylistDiscoveryWarning {
    InvalidExplicitTarget,
    CurrentDirectory,
    ReadParentDirectory,
    RawManifestLimitReached(RawManifestLimitReached),
    ExecutorUnavailable,
    SubmitRejected,
    StaleStructuralRevision,
    BatchRejected,
}

/// Stable-ID hint позволяет renderer-у удержать top-visible row без scroll jump.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlaylistDiscoveryInsertionHint {
    pub inserted_item_ids: Arc<[PlaylistItemId]>,
    pub before_item_id: Option<PlaylistItemId>,
}

struct AppDiscoveryWake {
    wake_port: AppWakePort,
}

impl DiscoveryWakePort for AppDiscoveryWake {
    fn wake(&self) -> Result<(), WakeDisconnected> {
        match self.wake_port.request_wake() {
            WakeDelivery::Armed | WakeDelivery::Coalesced => Ok(()),
            WakeDelivery::EventLoopClosed => Err(WakeDisconnected),
        }
    }
}

struct ManifestPending {
    scope_id: SiblingDiscoveryScopeId,
    target_item_id: PlaylistItemId,
    opened_media_kind: LocalMediaKind,
    policy: SiblingDiscoveryPolicySnapshot,
    continuation: DiscoveryContinuation,
}

struct ActiveDiscoveryScope {
    scope_id: SiblingDiscoveryScopeId,
    request_revision: DiscoveryRequestRevision,
    policy_revision: SiblingPolicyRevision,
    continuation: DiscoveryContinuation,
    job: DiscoveryJobHandle,
    committed_ids_by_key: BTreeMap<ManifestCandidateKey, PlaylistItemId>,
    pending_readiness_acks: Vec<AdmissionBatchId>,
    #[allow(
        dead_code,
        reason = "read by Session 15A action boundary before UI wiring"
    )]
    manifest: Arc<DirectoryManifest>,
    #[allow(
        dead_code,
        reason = "read by Session 15A action boundary before UI wiring"
    )]
    target_key: ManifestCandidateKey,
    admission_revisions: [u64; 2],
    readiness_revisions: [u64; 2],
}

/// Process-lifetime owner manifest stage, executor, scope registry и terminal model.
pub(super) struct PlaylistDiscoveryCoordinator {
    executor: Option<DiscoveryExecutor>,
    cpu_executor: Option<bounded_work_executor::BoundedExecutor>,
    action_jobs: action_jobs::DiscoveryActionJobs,
    metadata_sort: metadata_sort::MetadataSortOwner,
    youtube_metadata: youtube_metadata::YoutubeMetadataOwner,
    manifest_worker: Option<ManifestWorker>,
    settings_control: SharedDiscoverySettingsControl,
    next_scope_id: u64,
    manifest_job: Option<ManifestPending>,
    active_scope: Option<ActiveDiscoveryScope>,
    status: PlaylistDiscoveryStatus,
    last_insertion_hint: Option<PlaylistDiscoveryInsertionHint>,
    navigation_action: Option<navigation::PlaylistDiscoveryNavigationAction>,
    navigation_status: navigation::PlaylistDiscoveryNavigationStatus,
    initial_playback: initial_playback::InitialQueuePlaybackCoordinator,
}

impl PlaylistDiscoveryCoordinator {
    pub(super) fn new(wake_port: AppWakePort) -> Self {
        let discovery_wake: Arc<dyn DiscoveryWakePort> = Arc::new(AppDiscoveryWake {
            wake_port: wake_port.clone(),
        });
        let executor = DiscoveryExecutor::start(discovery_wake).ok();
        let cpu_executor = metadata_sort::start_cpu_executor();
        let manifest_worker = ManifestWorker::start(wake_port.clone());
        Self {
            executor,
            cpu_executor,
            action_jobs: action_jobs::DiscoveryActionJobs::new(),
            metadata_sort: metadata_sort::MetadataSortOwner::new(wake_port.clone()),
            youtube_metadata: youtube_metadata::YoutubeMetadataOwner::new(wake_port),
            manifest_worker,
            settings_control: SharedDiscoverySettingsControl::default(),
            next_scope_id: 1,
            manifest_job: None,
            active_scope: None,
            status: PlaylistDiscoveryStatus::Idle,
            last_insertion_hint: None,
            navigation_action: None,
            navigation_status: navigation::PlaylistDiscoveryNavigationStatus::Idle,
            initial_playback: initial_playback::InitialQueuePlaybackCoordinator::default(),
        }
    }

    pub(super) fn settings_port(&self) -> Box<dyn PlaylistDiscoverySettingsPort> {
        self.settings_control.port()
    }

    /// Новый explicit transport intent не должен быть переигран deferred directory start-ом.
    pub(in crate::playlist_runtime) fn cancel_initial_queue_playback(&mut self) {
        self.initial_playback.cancel_all();
    }

    pub(super) fn status(&self) -> &PlaylistDiscoveryStatus {
        &self.status
    }

    pub(super) fn last_insertion_hint(&self) -> Option<&PlaylistDiscoveryInsertionHint> {
        self.last_insertion_hint.as_ref()
    }

    /// Неблокирующе закрывает discovery admission перед общим committed-state flush.
    pub(super) fn begin_shutdown(&mut self) {
        self.cancel_active(DiscoveryCancellationCause::LifecycleShutdown);
        self.initial_playback.cancel_all();
        self.action_jobs.begin_shutdown();
        self.metadata_sort.begin_shutdown();
        self.youtube_metadata.begin_shutdown();
        if let Some(manifest_worker) = &mut self.manifest_worker {
            manifest_worker.close_admission();
        }
        if let Some(executor) = &self.executor {
            let report = executor.shutdown();
            tracing::debug!(
                cancelled_jobs = report.cancelled_jobs,
                in_flight_work_units = report.in_flight_work_units,
                "Discovery executor начал неблокирующий shutdown"
            );
        }
        if let Some(executor) = &self.cpu_executor {
            executor.shutdown();
        }
    }

    pub(super) fn start(
        &mut self,
        target_item_id: PlaylistItemId,
        target_path: PathBuf,
        opened_media_kind: LocalMediaKind,
        policy: FutureDiscoveryPolicy,
        continuation: DiscoveryContinuation,
        initial_playback_guard: Option<InitialQueuePlaybackGuard>,
    ) {
        self.cancel_active(DiscoveryCancellationCause::Superseded);
        self.initial_playback.cancel_all();
        self.last_insertion_hint = None;
        let Some(scope_id) = self.allocate_scope_id() else {
            if let Some(guard) = initial_playback_guard {
                self.initial_playback.arm_ready_without_scope(guard);
            }
            self.status = PlaylistDiscoveryStatus::TargetOnlyWarning {
                scope_id: SiblingDiscoveryScopeId::from_non_zero(
                    NonZeroU64::new(u64::MAX).expect("maximum u64 is non-zero"),
                ),
                warning: PlaylistDiscoveryWarning::ExecutorUnavailable,
            };
            return;
        };
        if let Some(guard) = initial_playback_guard {
            self.initial_playback.arm_waiting(scope_id, guard);
        }
        if !policy.load_siblings {
            self.initial_playback.mark_scope_ready(scope_id);
            self.status = PlaylistDiscoveryStatus::Completed {
                scope_id,
                outcome: DiscoveryFinalOutcome::Completed,
            };
            return;
        }
        let policy_revision = SiblingPolicyRevision::new(policy.revision.get());
        let discovery_policy = SiblingDiscoveryPolicySnapshot::new(
            true,
            sibling_filter(policy.sibling_media_filter),
            policy_revision,
        );
        let Some(manifest_worker) = &self.manifest_worker else {
            self.initial_playback.mark_scope_ready(scope_id);
            self.status = PlaylistDiscoveryStatus::TargetOnlyWarning {
                scope_id,
                warning: PlaylistDiscoveryWarning::ExecutorUnavailable,
            };
            return;
        };
        if manifest_worker
            .submit(ManifestWork {
                scope_id,
                target_path,
            })
            .is_err()
        {
            self.initial_playback.mark_scope_ready(scope_id);
            self.status = PlaylistDiscoveryStatus::TargetOnlyWarning {
                scope_id,
                warning: PlaylistDiscoveryWarning::SubmitRejected,
            };
            return;
        }
        self.manifest_job = Some(ManifestPending {
            scope_id,
            target_item_id,
            opened_media_kind,
            policy: discovery_policy,
            continuation,
        });
        self.update_settings_control(scope_id, DiscoverySettingsTarget::ManifestPending);
        self.status = PlaylistDiscoveryStatus::Enumerating { scope_id };
    }

    pub(super) fn drain(
        &mut self,
        controller: &mut super::controller::PlaylistController,
        queue_generation: u64,
    ) -> bool {
        let mut visible_change = self
            .youtube_metadata
            .drain(controller, std::time::Instant::now());
        visible_change |=
            self.action_jobs
                .drain(self.executor.as_ref(), controller, queue_generation);
        visible_change |= self.finish_manifest_if_ready(controller);
        let Some(mut active) = self.active_scope.take() else {
            return visible_change;
        };
        active.pending_readiness_acks.retain(|batch_id| {
            matches!(
                active.job.acknowledge_admitted_batch(*batch_id),
                AdmissionAckOutcome::AdmissionFrozen
            )
        });
        if controller.view_snapshot().structural_revision()
            != active.continuation.structural_revision
        {
            let _cancelled_now = active
                .job
                .cancel(DiscoveryCancellationCause::StructuralInvalidation);
            self.status = PlaylistDiscoveryStatus::TargetOnlyWarning {
                scope_id: active.scope_id,
                warning: PlaylistDiscoveryWarning::StaleStructuralRevision,
            };
            self.initial_playback.cancel_scope(active.scope_id);
            self.clear_settings_control(active.scope_id);
            return true;
        }
        for event in active.job.drain_events() {
            visible_change = true;
            match event {
                DiscoveryEvent::AdmittedBatch(batch) => {
                    if !batch_matches(&active, &batch)
                        || batch.apply_semantics() != BatchApplySemantics::ProgressiveSiblingCommit
                    {
                        continue;
                    }
                    let mut records = batch.records().to_vec();
                    records.sort_by_key(DiscoveryRecord::key);
                    let Some(anchor) = insertion_anchor(&active, &records) else {
                        let _cancelled_now = active
                            .job
                            .cancel(DiscoveryCancellationCause::StructuralInvalidation);
                        self.status = PlaylistDiscoveryStatus::TargetOnlyWarning {
                            scope_id: active.scope_id,
                            warning: PlaylistDiscoveryWarning::BatchRejected,
                        };
                        continue;
                    };
                    let drafts = match records
                        .iter()
                        .map(draft_from_record)
                        .collect::<Result<Vec<_>, _>>()
                    {
                        Ok(drafts) => drafts,
                        Err(_) => {
                            let _cancelled_now = active
                                .job
                                .cancel(DiscoveryCancellationCause::StructuralInvalidation);
                            self.status = PlaylistDiscoveryStatus::TargetOnlyWarning {
                                scope_id: active.scope_id,
                                warning: PlaylistDiscoveryWarning::BatchRejected,
                            };
                            continue;
                        }
                    };
                    match controller.commit_discovery_batch(active.continuation, anchor, drafts) {
                        Ok(committed) => {
                            self.last_insertion_hint = Some(PlaylistDiscoveryInsertionHint {
                                inserted_item_ids: Arc::from(committed.item_ids.clone()),
                                before_item_id: committed.anchor.before_item_id(),
                            });
                            for (record, item_id) in records.iter().zip(committed.item_ids.iter()) {
                                if let DiscoveryRecordKey::Manifest(key) = record.key() {
                                    active.committed_ids_by_key.insert(key, *item_id);
                                }
                            }
                            active.continuation = committed.continuation;
                            if matches!(
                                active.job.acknowledge_admitted_batch(batch.batch_id()),
                                AdmissionAckOutcome::AdmissionFrozen
                            ) && !active.pending_readiness_acks.contains(&batch.batch_id())
                            {
                                if active.pending_readiness_acks.len() < 2 {
                                    active.pending_readiness_acks.push(batch.batch_id());
                                } else {
                                    let _cancelled_now = active
                                        .job
                                        .cancel(DiscoveryCancellationCause::StructuralInvalidation);
                                    self.status = PlaylistDiscoveryStatus::TargetOnlyWarning {
                                        scope_id: active.scope_id,
                                        warning: PlaylistDiscoveryWarning::BatchRejected,
                                    };
                                }
                            }
                        }
                        Err(_) => {
                            let _cancelled_now = active
                                .job
                                .cancel(DiscoveryCancellationCause::StructuralInvalidation);
                            self.status = PlaylistDiscoveryStatus::TargetOnlyWarning {
                                scope_id: active.scope_id,
                                warning: PlaylistDiscoveryWarning::BatchRejected,
                            };
                        }
                    }
                }
                marker @ DiscoveryEvent::AdmissionAdvanced(admission) => {
                    self.initial_playback
                        .observe_admission_advanced(&active, admission);
                    // Marker-only events меняют только wait/action state, но не queue records.
                    self.route_navigation_event(controller, &mut active, marker);
                }
                marker @ DiscoveryEvent::FrontierReady(_) => {
                    // Marker-only events меняют только wait/action state, но не queue records.
                    self.route_navigation_event(controller, &mut active, marker);
                }
            }
        }
        if let Some(progress) = active.job.take_progress() {
            self.status = PlaylistDiscoveryStatus::Probing {
                scope_id: active.scope_id,
                processed: progress.processed,
                total: progress.total,
            };
            visible_change = true;
        }
        if let Some(summary) = active.job.take_final_summary() {
            self.finish_navigation_scope(controller, &active, summary.outcome);
            self.initial_playback
                .finish_scope(active.scope_id, summary.outcome);
            self.status = PlaylistDiscoveryStatus::Completed {
                scope_id: active.scope_id,
                outcome: summary.outcome,
            };
            self.clear_settings_control(active.scope_id);
            visible_change = true;
        } else {
            self.active_scope = Some(active);
        }
        visible_change
    }

    fn finish_manifest_if_ready(
        &mut self,
        controller: &super::controller::PlaylistController,
    ) -> bool {
        let Some(manifest_worker) = &self.manifest_worker else {
            return false;
        };
        let result = loop {
            match manifest_worker.receiver.try_recv() {
                Ok(result) => {
                    if self.manifest_job.as_ref().map(|pending| pending.scope_id)
                        == Some(result.scope_id)
                    {
                        break result.result;
                    }
                    // Stale/superseded manifest outcome has no queue authority.
                }
                Err(mpsc::TryRecvError::Empty) => return false,
                Err(mpsc::TryRecvError::Disconnected) => {
                    break Err(DirectoryManifestBuildError::ReadParentDirectory(
                        std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "manifest worker disconnected",
                        ),
                    ));
                }
            }
        };
        let Some(manifest_job) = self.manifest_job.take() else {
            return false;
        };
        let finalized = self.settings_control.is_finalized();
        if finalized {
            self.initial_playback.cancel_scope(manifest_job.scope_id);
            self.status = PlaylistDiscoveryStatus::Completed {
                scope_id: manifest_job.scope_id,
                outcome: DiscoveryFinalOutcome::Cancelled(
                    DiscoveryCancellationCause::StructuralInvalidation,
                ),
            };
            self.clear_settings_control(manifest_job.scope_id);
            return true;
        }
        let manifest = match result {
            Ok(manifest) => Arc::new(manifest),
            Err(error) => {
                self.initial_playback
                    .mark_scope_ready(manifest_job.scope_id);
                self.status = PlaylistDiscoveryStatus::TargetOnlyWarning {
                    scope_id: manifest_job.scope_id,
                    warning: manifest_warning(error),
                };
                self.clear_settings_control(manifest_job.scope_id);
                return true;
            }
        };
        let target_key = manifest.explicit_target().candidate_key();
        self.initial_playback
            .observe_manifest(manifest_job.scope_id, &manifest, target_key);
        let request_revision = DiscoveryRequestRevision::new(manifest_job.scope_id.get());
        let request = DiscoveryRequest::Sibling(SiblingDiscoveryRequest::new(
            Arc::clone(&manifest),
            manifest_job.opened_media_kind,
            manifest_job.policy,
            request_revision,
        ));
        let Some(executor) = &self.executor else {
            self.initial_playback
                .mark_scope_ready(manifest_job.scope_id);
            self.status = PlaylistDiscoveryStatus::TargetOnlyWarning {
                scope_id: manifest_job.scope_id,
                warning: PlaylistDiscoveryWarning::ExecutorUnavailable,
            };
            self.clear_settings_control(manifest_job.scope_id);
            return true;
        };
        let job = match executor.submit(request) {
            Ok(job) => job,
            Err(_) => {
                self.initial_playback
                    .mark_scope_ready(manifest_job.scope_id);
                self.status = PlaylistDiscoveryStatus::TargetOnlyWarning {
                    scope_id: manifest_job.scope_id,
                    warning: PlaylistDiscoveryWarning::SubmitRejected,
                };
                self.clear_settings_control(manifest_job.scope_id);
                return true;
            }
        };
        let priority_hint = manifest_priority_hint(&manifest, target_key, controller);
        let reprioritize =
            job.reprioritize(ReprioritizeHint::new(priority_hint.into_boxed_slice()));
        tracing::debug!(
            reprioritized = reprioritize.reprioritized,
            stale = reprioritize.stale,
            "Sibling discovery применил neutral priority hint"
        );
        let mut committed_ids_by_key = BTreeMap::new();
        committed_ids_by_key.insert(target_key, manifest_job.target_item_id);
        let active = ActiveDiscoveryScope {
            scope_id: manifest_job.scope_id,
            request_revision,
            policy_revision: manifest_job.policy.revision(),
            continuation: manifest_job.continuation,
            job: job.clone(),
            committed_ids_by_key,
            pending_readiness_acks: Vec::with_capacity(2),
            manifest,
            target_key,
            admission_revisions: [0; 2],
            readiness_revisions: [0; 2],
        };
        let frozen = self.settings_control.is_frozen();
        if frozen && !job.freeze_admission() {
            let _cancelled_now = job.cancel(DiscoveryCancellationCause::StructuralInvalidation);
            self.initial_playback
                .mark_scope_ready(manifest_job.scope_id);
            self.status = PlaylistDiscoveryStatus::TargetOnlyWarning {
                scope_id: manifest_job.scope_id,
                warning: PlaylistDiscoveryWarning::BatchRejected,
            };
            self.clear_settings_control(manifest_job.scope_id);
            return true;
        }
        self.update_settings_control(
            manifest_job.scope_id,
            DiscoverySettingsTarget::ActiveJob(job),
        );
        self.active_scope = Some(active);
        true
    }

    fn allocate_scope_id(&mut self) -> Option<SiblingDiscoveryScopeId> {
        let scope = SiblingDiscoveryScopeId::from_non_zero(NonZeroU64::new(self.next_scope_id)?);
        self.next_scope_id = self.next_scope_id.checked_add(1)?;
        Some(scope)
    }

    fn cancel_active(&mut self, cause: DiscoveryCancellationCause) {
        if let Some(active) = self.active_scope.take() {
            let _cancelled_now = active.job.cancel(cause);
            self.initial_playback
                .finish_scope(active.scope_id, DiscoveryFinalOutcome::Cancelled(cause));
            self.clear_settings_control(active.scope_id);
        }
        if let Some(manifest) = self.manifest_job.take() {
            // Persistent bounded worker завершит syscall, но stale result больше не применяется.
            let scope_id = manifest.scope_id;
            self.initial_playback
                .finish_scope(scope_id, DiscoveryFinalOutcome::Cancelled(cause));
            self.clear_settings_control(scope_id);
        }
        if cause != DiscoveryCancellationCause::UserCancelled {
            self.initial_playback.cancel_all();
        }
    }

    fn update_settings_control(
        &self,
        scope_id: SiblingDiscoveryScopeId,
        target: DiscoverySettingsTarget,
    ) {
        self.settings_control.update(scope_id, target);
    }

    fn clear_settings_control(&self, scope_id: SiblingDiscoveryScopeId) {
        self.settings_control.clear(scope_id);
    }
}

impl Drop for PlaylistDiscoveryCoordinator {
    fn drop(&mut self) {
        self.begin_shutdown();
    }
}

fn manifest_warning(error: DirectoryManifestBuildError) -> PlaylistDiscoveryWarning {
    match error {
        DirectoryManifestBuildError::InvalidExplicitTarget => {
            PlaylistDiscoveryWarning::InvalidExplicitTarget
        }
        DirectoryManifestBuildError::CurrentDirectory(_) => {
            PlaylistDiscoveryWarning::CurrentDirectory
        }
        DirectoryManifestBuildError::ReadParentDirectory(_) => {
            PlaylistDiscoveryWarning::ReadParentDirectory
        }
        DirectoryManifestBuildError::RawManifestLimitReached(limit) => {
            PlaylistDiscoveryWarning::RawManifestLimitReached(limit)
        }
    }
}
