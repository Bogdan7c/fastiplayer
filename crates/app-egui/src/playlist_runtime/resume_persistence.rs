//! Process-lifetime policy owner маленького position sidecar.

use std::cell::RefCell;
use std::num::NonZeroU64;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use player_core::{MediaInstanceId, PlaybackState, PlayerSnapshot};
use playlist_state::{
    AtomicWriteOutcome, PlaylistResumeStore, QuarantineFileName, QuarantineOutcome,
    ResumeCheckpoint, ResumeInspectionOutcome, ResumeSaveRevision, ResumeSubmitOutcome,
    ResumeWorker, ResumeWorkerShutdownOutcome, ResumeWriteSnapshot,
};

use super::controller::PlaylistController;
use super::settings::PlaylistResumeIntervalPort;
use super::{
    LifecycleTimelineCheckpointPosition, PlaylistBindingGeneration, PlaylistLineagePersistence,
    PlaylistRuntime, PlaylistRuntimeBinding, StartupPosition,
};
use crate::process_shutdown::ShutdownDeadline;

/// Strong install уже знает seekability и окончательную подтверждённую позицию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstalledCheckpointPosition {
    Seekable(Duration),
    NonSeekable,
    /// Live media никогда не создаёт и не очищает persistent checkpoint.
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumeStoreAccess {
    Uninitialized,
    Writable,
    SaveBlocked,
}

#[derive(Debug)]
struct ResumeIntervalSchedule {
    interval: Duration,
    last_periodic_capture: Option<Instant>,
}

impl ResumeIntervalSchedule {
    fn new(interval_ms: u64) -> Self {
        Self {
            interval: Duration::from_millis(interval_ms),
            last_periodic_capture: None,
        }
    }

    fn reschedule(&mut self, interval_ms: u64) {
        // Pending/latest snapshot принадлежит writer owner-у и при live apply не очищается.
        self.interval = Duration::from_millis(interval_ms);
    }

    fn periodic_capture_is_due(&self, now: Instant) -> bool {
        self.last_periodic_capture
            .is_none_or(|last| now.saturating_duration_since(last) >= self.interval)
    }

    fn record_capture(&mut self, now: Instant) {
        self.last_periodic_capture = Some(now);
    }
}

pub(super) struct ResumeIntervalPort {
    schedule: Rc<RefCell<ResumeIntervalSchedule>>,
}

impl PlaylistResumeIntervalPort for ResumeIntervalPort {
    fn reschedule_resume_interval(&mut self, interval_ms: u64) -> Result<(), String> {
        self.schedule.borrow_mut().reschedule(interval_ms);
        Ok(())
    }
}

/// Sidecar owner не имеет доступа к renderer/player handles и принимает только confirmed snapshots.
pub(super) struct PlaylistResumePersistenceOwner {
    store: Option<Arc<PlaylistResumeStore>>,
    store_access: ResumeStoreAccess,
    worker: Option<ResumeWorker>,
    schedule: Rc<RefCell<ResumeIntervalSchedule>>,
    enabled: bool,
    persistent_lineage: bool,
    loaded_checkpoint: Option<ResumeCheckpoint>,
    last_observed_state: Option<PlaybackState>,
    last_requested: Option<Option<ResumeCheckpoint>>,
    latest_snapshot: Option<ResumeWriteSnapshot>,
    next_revision: u64,
    last_reported_revision: Option<ResumeSaveRevision>,
    worker_start_failed: bool,
    shutdown_complete: bool,
}

impl PlaylistResumePersistenceOwner {
    pub(super) fn new(interval_ms: u64, enabled: bool) -> Self {
        Self {
            store: None,
            store_access: ResumeStoreAccess::Uninitialized,
            worker: None,
            schedule: Rc::new(RefCell::new(ResumeIntervalSchedule::new(interval_ms))),
            enabled,
            persistent_lineage: false,
            loaded_checkpoint: None,
            last_observed_state: None,
            last_requested: None,
            latest_snapshot: None,
            next_revision: 1,
            last_reported_revision: None,
            worker_start_failed: false,
            shutdown_complete: false,
        }
    }

    pub(super) fn interval_port(&self) -> Box<dyn PlaylistResumeIntervalPort> {
        Box::new(ResumeIntervalPort {
            schedule: self.schedule.clone(),
        })
    }

    /// Inspection выполняется после process lease и затрагивает только маленький bounded artifact.
    pub(super) fn install_store(&mut self, store: Arc<PlaylistResumeStore>) {
        let inspection = store.inspect();
        match inspection {
            ResumeInspectionOutcome::Missing => {
                self.store_access = ResumeStoreAccess::Writable;
            }
            ResumeInspectionOutcome::Loaded(checkpoint) => {
                self.loaded_checkpoint = checkpoint;
                self.store_access = ResumeStoreAccess::Writable;
            }
            ResumeInspectionOutcome::CorruptNeedsQuarantine {
                inspected_identity,
                cause,
            } => {
                let quarantine = store.apply_quarantine(
                    &inspected_identity,
                    &QuarantineFileName::resume_from_timestamp(SystemTime::now()),
                );
                match quarantine {
                    QuarantineOutcome::Applied { .. } => {
                        self.store_access = ResumeStoreAccess::Writable;
                        tracing::warn!(
                            ?cause,
                            "Повреждённый playlist-resume sidecar изолирован; очередь не изменялась"
                        );
                    }
                    QuarantineOutcome::SourceChanged
                    | QuarantineOutcome::FailedSaveBlocked { .. } => {
                        self.store_access = ResumeStoreAccess::SaveBlocked;
                        tracing::warn!(
                            ?cause,
                            ?quarantine,
                            "Playlist-resume quarantine не завершён; sidecar защищён от записи"
                        );
                    }
                }
            }
            ResumeInspectionOutcome::NewerSchemaSaveBlocked { schema_version } => {
                self.store_access = ResumeStoreAccess::SaveBlocked;
                tracing::warn!(
                    schema_version,
                    "Новая schema playlist-resume защищена от перезаписи"
                );
            }
            ResumeInspectionOutcome::ProtectedSaveBlocked { cause } => {
                self.store_access = ResumeStoreAccess::SaveBlocked;
                tracing::warn!(
                    ?cause,
                    "Неопознанный playlist-resume sidecar защищён от перезаписи"
                );
            }
        }
        self.store = Some(store);
    }

    pub(super) fn activate_lineage(&mut self, lineage: PlaylistLineagePersistence) {
        self.persistent_lineage = matches!(lineage, PlaylistLineagePersistence::Persistent);
        if !self.persistent_lineage {
            self.loaded_checkpoint = None;
            return;
        }
        self.start_worker_if_allowed();
    }

    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if enabled {
            self.start_worker_if_allowed();
        } else {
            self.loaded_checkpoint = None;
        }
    }

    /// Только exact restored current получает disk position; fallback targets всегда KeepStart.
    pub(super) fn startup_position(
        &mut self,
        item_id: playlist_core::PlaylistItemId,
        locator: &playlist_core::PlaylistLocator,
    ) -> StartupPosition {
        if !self.enabled || !self.persistent_lineage {
            return StartupPosition::KeepStart;
        }
        let Some(checkpoint) = self.loaded_checkpoint.take() else {
            return StartupPosition::KeepStart;
        };
        match checkpoint.matches(item_id, locator) {
            Ok(true) => StartupPosition::Restore(checkpoint.position()),
            Ok(false) => {
                tracing::debug!(
                    "Playlist-resume checkpoint не совпал с exact current locator и проигнорирован"
                );
                StartupPosition::KeepStart
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "Exact locator fingerprint недоступен; resume checkpoint проигнорирован"
                );
                StartupPosition::KeepStart
            }
        }
    }

    pub(super) fn record_installed(
        &mut self,
        controller: &PlaylistController,
        binding_generation: PlaylistBindingGeneration,
        media_instance_id: MediaInstanceId,
        position: InstalledCheckpointPosition,
        now: Instant,
    ) {
        self.report_latest_attempt();
        self.last_observed_state = None;
        let capture =
            capture_for_exact_active(controller, binding_generation, media_instance_id, position);
        self.submit_capture(capture, now);
    }

    pub(super) fn observe_snapshot(
        &mut self,
        controller: &PlaylistController,
        binding: PlaylistRuntimeBinding,
        snapshot: &PlayerSnapshot,
        now: Instant,
    ) {
        self.report_latest_attempt();
        let Some(active) = exact_active_item(
            controller,
            binding.binding_generation(),
            snapshot.media_instance_id,
        ) else {
            return;
        };
        let previous_state = self.last_observed_state.replace(snapshot.playback_state);
        let immediate_position = match snapshot.playback_state {
            PlaybackState::Ended => Some(Duration::ZERO),
            PlaybackState::Paused | PlaybackState::Stopped
                if previous_state != Some(snapshot.playback_state) =>
            {
                Some(snapshot.current_position)
            }
            _ => None,
        };
        let periodic_due = snapshot.playback_state == PlaybackState::Playing
            && self.schedule.borrow().periodic_capture_is_due(now);
        if immediate_position.is_none() && !periodic_due {
            return;
        }
        let position = if snapshot.timeline.mode == media_core::TimelineMode::Live {
            InstalledCheckpointPosition::Live
        } else if snapshot.timeline.seekable {
            InstalledCheckpointPosition::Seekable(
                immediate_position.unwrap_or(snapshot.current_position),
            )
        } else {
            InstalledCheckpointPosition::NonSeekable
        };
        let capture = checkpoint_for_item(active.item_id, active.locator, position);
        self.submit_capture(capture, now);
    }

    pub(super) fn record_confirmed_seek(
        &mut self,
        controller: &PlaylistController,
        binding_generation: PlaylistBindingGeneration,
        media_instance_id: MediaInstanceId,
        position: Duration,
        now: Instant,
    ) {
        self.report_latest_attempt();
        let Some(active_media) = controller.active_media() else {
            return;
        };
        if active_media.player_binding_generation() != binding_generation {
            return;
        }
        let Some(item_id) = active_media.item_id() else {
            return;
        };
        if active_media.media_instance_id() != media_instance_id {
            return;
        }
        let Some(current) = controller.queue().traversal_current() else {
            return;
        };
        if current.item_id() != item_id {
            return;
        }
        let Some(item) = controller.queue().item(item_id) else {
            return;
        };
        let capture = checkpoint_for_item(
            item_id,
            item.locator(),
            InstalledCheckpointPosition::Seekable(position),
        );
        self.submit_capture(capture, now);
    }

    pub(super) fn force_snapshot(
        &mut self,
        controller: &PlaylistController,
        binding: PlaylistRuntimeBinding,
        snapshot: &PlayerSnapshot,
        timeline_position: LifecycleTimelineCheckpointPosition,
        now: Instant,
    ) {
        self.report_latest_attempt();
        let Some(active) = exact_active_item(
            controller,
            binding.binding_generation(),
            snapshot.media_instance_id,
        ) else {
            return;
        };
        let explicit_timeline_position = timeline_position.explicit_position();
        if explicit_timeline_position.is_none()
            && matches!(
                snapshot.playback_state,
                PlaybackState::Opening | PlaybackState::Seeking | PlaybackState::Scrubbing
            )
        {
            return;
        }
        let position = if snapshot.timeline.mode == media_core::TimelineMode::Live {
            InstalledCheckpointPosition::Live
        } else if snapshot.timeline.seekable {
            InstalledCheckpointPosition::Seekable(
                if let Some(settled_position) = explicit_timeline_position {
                    settled_position
                } else if snapshot.playback_state == PlaybackState::Ended {
                    Duration::ZERO
                } else {
                    snapshot.current_position
                },
            )
        } else {
            InstalledCheckpointPosition::NonSeekable
        };
        let capture = checkpoint_for_item(active.item_id, active.locator, position);
        self.submit_capture(capture, now);
    }

    pub(super) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> ResumeWorkerShutdownOutcome {
        if self.shutdown_complete {
            return ResumeWorkerShutdownOutcome::AlreadyCompleted;
        }
        let Some(worker) = self.worker.as_mut() else {
            self.shutdown_complete = true;
            return if self.worker_start_failed {
                ResumeWorkerShutdownOutcome::WorkerUnavailable
            } else {
                ResumeWorkerShutdownOutcome::AlreadyCompleted
            };
        };
        let outcome = worker.shutdown(self.latest_snapshot.clone(), deadline.remaining());
        if !matches!(outcome, ResumeWorkerShutdownOutcome::TimedOut) {
            self.shutdown_complete = true;
        }
        outcome
    }

    fn start_worker_if_allowed(&mut self) {
        self.start_worker_for(ResumeWriteReason::PlaybackCapture);
    }

    /// Clear обязан записать `null` даже при отключённом обычном capture policy.
    fn start_worker_for(&mut self, reason: ResumeWriteReason) {
        if self.worker.is_some()
            || (reason == ResumeWriteReason::PlaybackCapture && !self.enabled)
            || !self.persistent_lineage
            || self.store_access != ResumeStoreAccess::Writable
        {
            return;
        }
        let Some(store) = self.store.clone() else {
            return;
        };
        match ResumeWorker::start(store) {
            Ok(worker) => self.worker = Some(worker),
            Err(error) => {
                self.worker_start_failed = true;
                tracing::error!(error = %error, "Playlist resume writer не запущен");
            }
        }
    }

    fn submit_latest(&mut self, checkpoint: Option<ResumeCheckpoint>, now: Instant) {
        self.submit_latest_for(checkpoint, now, ResumeWriteReason::PlaybackCapture);
    }

    /// Общая latest-only запись различает обычный capture и обязательный Clear tombstone.
    fn submit_latest_for(
        &mut self,
        checkpoint: Option<ResumeCheckpoint>,
        now: Instant,
        reason: ResumeWriteReason,
    ) {
        if (reason == ResumeWriteReason::PlaybackCapture && !self.enabled)
            || !self.persistent_lineage
            || self.store_access != ResumeStoreAccess::Writable
            || self.last_requested.as_ref() == Some(&checkpoint)
        {
            return;
        }
        self.start_worker_for(reason);
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        let Some(revision) = NonZeroU64::new(self.next_revision) else {
            self.worker_start_failed = true;
            tracing::error!("Playlist resume revision исчерпана");
            return;
        };
        let snapshot =
            ResumeWriteSnapshot::new(ResumeSaveRevision::new(revision), checkpoint.clone());
        match worker.submit(snapshot.clone()) {
            ResumeSubmitOutcome::Accepted => {
                self.last_requested = Some(checkpoint);
                self.latest_snapshot = Some(snapshot);
                self.next_revision = self.next_revision.checked_add(1).unwrap_or(0);
                self.schedule.borrow_mut().record_capture(now);
            }
            ResumeSubmitOutcome::SameOrOlderRevision => {
                tracing::warn!("Playlist resume writer отверг неожиданно старую revision");
            }
            ResumeSubmitOutcome::Disconnected => {
                self.worker_start_failed = true;
                tracing::error!("Playlist resume writer потерял command channel");
            }
        }
    }

    fn submit_capture(&mut self, capture: CheckpointCapture, now: Instant) {
        if let CheckpointCapture::Write(checkpoint) = capture {
            self.submit_latest(checkpoint, now);
        }
    }

    /// Успешный Clear немедленно делает старую позицию недоступной для Undo/restart.
    pub(super) fn clear_after_playlist_clear(&mut self, now: Instant) {
        self.report_latest_attempt();
        self.loaded_checkpoint = None;
        self.last_observed_state = None;
        self.submit_latest_for(None, now, ResumeWriteReason::PlaylistClear);
    }

    fn report_latest_attempt(&mut self) {
        let Some(report) = self.worker.as_ref().and_then(ResumeWorker::latest_report) else {
            return;
        };
        if self.last_reported_revision == Some(report.revision) {
            return;
        }
        self.last_reported_revision = Some(report.revision);
        match report.outcome {
            AtomicWriteOutcome::Durable => {}
            AtomicWriteOutcome::ReplacedDurabilityUnconfirmed(cause) => {
                self.make_failed_revision_retryable(report.revision);
                tracing::warn!(
                    revision = report.revision.get(),
                    ?cause,
                    "Playlist resume target заменён, но directory durability не подтверждена"
                );
            }
            AtomicWriteOutcome::NotReplaced(failure) => {
                self.make_failed_revision_retryable(report.revision);
                tracing::warn!(
                    revision = report.revision.get(),
                    ?failure,
                    "Playlist resume checkpoint не записан"
                );
            }
        }
    }

    fn make_failed_revision_retryable(&mut self, revision: ResumeSaveRevision) {
        if self
            .latest_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.revision() == revision)
        {
            // Следующий periodic/immediate/terminal capture получает новую revision.
            self.last_requested = None;
        }
    }
}

struct ExactActiveItem<'item> {
    item_id: playlist_core::PlaylistItemId,
    locator: &'item playlist_core::PlaylistLocator,
}

/// `Write(None)` — осознанный tombstone non-seekable media; `Skip` не трогает disk state.
enum CheckpointCapture {
    Skip,
    Write(Option<ResumeCheckpoint>),
}

/// Причина записи определяет, должна ли настройка capture блокировать sidecar update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumeWriteReason {
    PlaybackCapture,
    PlaylistClear,
}

fn exact_active_item(
    controller: &PlaylistController,
    binding_generation: PlaylistBindingGeneration,
    media_instance_id: Option<MediaInstanceId>,
) -> Option<ExactActiveItem<'_>> {
    let active_media = controller.active_media()?;
    if active_media.player_binding_generation() != binding_generation
        || Some(active_media.media_instance_id()) != media_instance_id
    {
        return None;
    }
    let item_id = active_media.item_id()?;
    let current = controller.queue().traversal_current()?;
    if current.item_id() != item_id {
        // Detach/tombstone lineage не имеет persistent current checkpoint-а.
        return None;
    }
    let item = controller.queue().item(item_id)?;
    Some(ExactActiveItem {
        item_id,
        locator: item.locator(),
    })
}

fn capture_for_exact_active(
    controller: &PlaylistController,
    binding_generation: PlaylistBindingGeneration,
    media_instance_id: MediaInstanceId,
    position: InstalledCheckpointPosition,
) -> CheckpointCapture {
    let Some(active) = exact_active_item(controller, binding_generation, Some(media_instance_id))
    else {
        return CheckpointCapture::Skip;
    };
    checkpoint_for_item(active.item_id, active.locator, position)
}

fn checkpoint_for_item(
    item_id: playlist_core::PlaylistItemId,
    locator: &playlist_core::PlaylistLocator,
    position: InstalledCheckpointPosition,
) -> CheckpointCapture {
    match position {
        InstalledCheckpointPosition::Seekable(position) => {
            match ResumeCheckpoint::for_locator(item_id, locator, position) {
                Ok(checkpoint) => CheckpointCapture::Write(Some(checkpoint)),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "Playlist resume checkpoint пропущен для неподдерживаемого native path"
                    );
                    CheckpointCapture::Skip
                }
            }
        }
        InstalledCheckpointPosition::NonSeekable => CheckpointCapture::Write(None),
        InstalledCheckpointPosition::Live => CheckpointCapture::Skip,
    }
}

impl PlaylistRuntime {
    /// Production bootstrap устанавливает отдельный store до открытия allocator gate.
    pub(crate) fn install_playlist_resume_store(&mut self, store: Arc<PlaylistResumeStore>) {
        self.resume_persistence.install_store(store);
    }

    /// Один frame snapshot одновременно обслуживает immediate edges и periodic policy.
    pub(crate) fn observe_resume_checkpoint_snapshot(
        &mut self,
        binding: PlaylistRuntimeBinding,
        snapshot: &PlayerSnapshot,
    ) {
        if self.validate_binding(binding).is_err() {
            return;
        }
        let Some(controller) = self.controller.as_ref() else {
            return;
        };
        self.resume_persistence
            .observe_snapshot(controller, binding, snapshot, Instant::now());
    }

    /// Exact seek receipt является authoritative подтверждением новой позиции.
    pub(crate) fn record_confirmed_resume_seek(
        &mut self,
        binding: PlaylistRuntimeBinding,
        media_instance_id: MediaInstanceId,
        position: Duration,
    ) {
        if self.validate_binding(binding).is_err() {
            return;
        }
        let Some(controller) = self.controller.as_ref() else {
            return;
        };
        self.resume_persistence.record_confirmed_seek(
            controller,
            binding.binding_generation(),
            media_instance_id,
            position,
            Instant::now(),
        );
    }

    /// Strong install checkpoint вызывается после domain/lineage commit-а.
    pub(crate) fn record_installed_resume_checkpoint(
        &mut self,
        binding_generation: PlaylistBindingGeneration,
        media_instance_id: MediaInstanceId,
        position: InstalledCheckpointPosition,
    ) {
        let Some(controller) = self.controller.as_ref() else {
            return;
        };
        self.resume_persistence.record_installed(
            controller,
            binding_generation,
            media_instance_id,
            position,
            Instant::now(),
        );
    }

    /// Queue Clear очищает sidecar независимо от последующего Undo queue snapshot-а.
    pub(crate) fn clear_resume_checkpoint_after_playlist_clear(&mut self, now: Instant) {
        self.resume_persistence.clear_after_playlist_clear(now);
    }

    /// Terminal shell вызывает boundary до остановки player owner-а.
    pub(crate) fn force_resume_checkpoint_after_seek_settlement(
        &mut self,
        binding: PlaylistRuntimeBinding,
        snapshot: &PlayerSnapshot,
        timeline_position: LifecycleTimelineCheckpointPosition,
    ) {
        if self.validate_binding(binding).is_err() {
            return;
        }
        let Some(controller) = self.controller.as_ref() else {
            return;
        };
        self.resume_persistence.force_snapshot(
            controller,
            binding,
            snapshot,
            timeline_position,
            Instant::now(),
        );
    }

    /// Активирует существующий `player.resume_last_position` без restart-а.
    pub(crate) fn set_resume_last_position_enabled(&mut self, enabled: bool) {
        self.resume_persistence.set_enabled(enabled);
    }
}

#[cfg(test)]
mod tests;
