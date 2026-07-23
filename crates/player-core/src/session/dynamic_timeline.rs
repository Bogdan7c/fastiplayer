//! Session-owned projection dynamic live snapshot-а в public player timeline.

use crossbeam_channel::Receiver;
use frame_server_core::CancelScrubReason;
use media_core::{
    DynamicMediaTimelinePort, DynamicMediaTimelinePortGeneration, DynamicMediaTimelineRevision,
    DynamicMediaTimelineSnapshot, TimelineMode, TimelineNotSeekableReason,
};

use crate::{MediaInstanceId, PreparedMediaTimelineMode};

use super::PlayerSession;

/// Wait-set descriptor не переносит payload и остаётся fenced port generation-ом.
#[derive(Debug, Clone)]
pub(crate) struct DynamicTimelineWaitSource {
    pub(crate) port_generation: DynamicMediaTimelinePortGeneration,
    pub(crate) observed_revision: DynamicMediaTimelineRevision,
    pub(crate) activity_receiver: Receiver<()>,
}

#[derive(Debug)]
struct DynamicTimelineBinding {
    media_instance_id: MediaInstanceId,
    port: DynamicMediaTimelinePort,
    observed_revision: DynamicMediaTimelineRevision,
    activity_disconnected: bool,
}

/// Единственный session-owned active binding.
#[derive(Debug, Default)]
pub(super) struct DynamicTimelineRuntime {
    binding: Option<DynamicTimelineBinding>,
}

impl PlayerSession {
    /// Устанавливает static/live mode в atomic media commit owner turn.
    pub(super) fn install_timeline_mode(
        &mut self,
        media_instance_id: MediaInstanceId,
        timeline_mode: PreparedMediaTimelineMode,
    ) {
        self.dynamic_timeline.binding = None;
        self.snapshot.timeline.live_edge = None;
        self.snapshot.timeline.live_epoch = None;
        self.snapshot.timeline.live_revision = None;

        match timeline_mode {
            PreparedMediaTimelineMode::Static { playback_window } => {
                self.playback_window = playback_window;
                self.snapshot.timeline.mode = TimelineMode::Static;
            }
            PreparedMediaTimelineMode::Live { port } => {
                debug_assert!(self.source_duration.is_none());
                self.playback_window = None;
                let initial = port.observe().snapshot;
                self.dynamic_timeline.binding = Some(DynamicTimelineBinding {
                    media_instance_id,
                    port,
                    observed_revision: initial.revision,
                    activity_disconnected: false,
                });
                self.apply_dynamic_timeline_snapshot(initial, true);
            }
        }
    }

    /// Consume latest revision; вызывается и в Paused, и в active playback.
    pub(crate) fn refresh_dynamic_timeline(&mut self) -> bool {
        let Some(binding) = self.dynamic_timeline.binding.as_ref() else {
            return false;
        };
        if self.snapshot.media_instance_id != Some(binding.media_instance_id) {
            return false;
        }
        let latest = binding.port.observe().snapshot;
        if latest.port_generation != binding.port.port_generation()
            || latest.revision == binding.observed_revision
        {
            return false;
        }
        self.apply_dynamic_timeline_snapshot(latest, false);
        true
    }

    /// Готовит observe→arm часть worker wait protocol.
    pub(crate) fn dynamic_timeline_wait_source(&self) -> Option<DynamicTimelineWaitSource> {
        let binding = self.dynamic_timeline.binding.as_ref()?;
        if binding.activity_disconnected
            || self.snapshot.media_instance_id != Some(binding.media_instance_id)
        {
            return None;
        }
        Some(DynamicTimelineWaitSource {
            port_generation: binding.port.port_generation(),
            observed_revision: binding.observed_revision,
            activity_receiver: binding.port.activity_receiver(),
        })
    }

    /// Recheck после arm закрывает publish race непосредственно перед blocking select.
    pub(crate) fn dynamic_timeline_changed_after_arm(
        &mut self,
        port_generation: DynamicMediaTimelinePortGeneration,
        observed_revision: DynamicMediaTimelineRevision,
    ) -> bool {
        let Some(binding) = self.dynamic_timeline.binding.as_ref() else {
            return false;
        };
        if binding.activity_disconnected
            || binding.port.port_generation() != port_generation
            || binding.observed_revision != observed_revision
        {
            return false;
        }
        let Some(latest) = binding.port.recheck_after_arm(observed_revision) else {
            return false;
        };
        self.apply_dynamic_timeline_snapshot(latest, false);
        true
    }

    /// Disconnected publisher выключает wait source, сохраняя последний snapshot.
    pub(crate) fn disconnect_dynamic_timeline_activity(
        &mut self,
        port_generation: DynamicMediaTimelinePortGeneration,
    ) {
        let Some(binding) = self.dynamic_timeline.binding.as_mut() else {
            return;
        };
        if binding.port.port_generation() == port_generation {
            binding.activity_disconnected = true;
        }
    }

    fn apply_dynamic_timeline_snapshot(
        &mut self,
        dynamic_snapshot: DynamicMediaTimelineSnapshot,
        initial_install: bool,
    ) {
        let Some(binding) = self.dynamic_timeline.binding.as_mut() else {
            return;
        };
        if self.snapshot.media_instance_id != Some(binding.media_instance_id)
            || binding.port.port_generation() != dynamic_snapshot.port_generation
        {
            return;
        }
        binding.observed_revision = dynamic_snapshot.revision;

        let live_edge = dynamic_snapshot.state.live_edge();
        let seekable_range = dynamic_snapshot.state.seekable_range();
        self.snapshot.timeline.mode = TimelineMode::Live;
        self.snapshot.timeline.duration = None;
        self.snapshot.duration = None;
        self.snapshot.timeline.live_edge = Some(live_edge);
        self.snapshot.timeline.live_epoch = Some(dynamic_snapshot.source_epoch);
        self.snapshot.timeline.live_revision = Some(dynamic_snapshot.revision);
        self.snapshot.timeline.seekable_range = seekable_range;
        self.snapshot.timeline.seekable = seekable_range.is_some();
        self.snapshot.timeline.not_seekable_reason = seekable_range
            .is_none()
            .then_some(TimelineNotSeekableReason::LiveWindowUnavailable);

        if initial_install {
            self.current_source_position = live_edge.as_duration();
            self.snapshot.set_timeline_position(live_edge);
            self.pipeline.set_media_clock_base(live_edge.as_duration());
        }

        if let Some(range) = seekable_range {
            self.expire_dynamic_seek_target_outside(range);
        } else {
            self.expire_all_dynamic_seek_targets();
        }
    }

    fn expire_dynamic_seek_target_outside(&mut self, available_range: media_core::TimelineRange) {
        let active_seek_expired = self
            .seek_runtime
            .active_commit()
            .is_some_and(|seek_commit| !available_range.contains(seek_commit.target_position));
        let public_target_expired = self
            .snapshot
            .timeline
            .target_position
            .is_some_and(|target| !available_range.contains(target));
        if active_seek_expired || public_target_expired {
            self.expire_dynamic_seek_or_scrub(Some(available_range));
        }
    }

    fn expire_all_dynamic_seek_targets(&mut self) {
        if self.seek_runtime.active_commit().is_some()
            || self.snapshot.timeline.target_position.is_some()
        {
            self.expire_dynamic_seek_or_scrub(None);
        }
    }

    fn expire_dynamic_seek_or_scrub(&mut self, available_range: Option<media_core::TimelineRange>) {
        if let Some(seek_commit) = self.seek_runtime.active_commit() {
            self.fail_dynamic_seek_target_expired(seek_commit, available_range);
            return;
        }

        self.expire_pending_exact_timeline_seek(available_range);
        self.cancel_active_scrub_for_external_command(CancelScrubReason::StaleContext);
        let error = crate::PlayerError::new(
            crate::PlayerErrorKind::SeekTargetExpired,
            format!("Live scrub target expired outside latest DVR range {available_range:?}"),
        );
        self.fail_pending_seek_receipts(error.clone());
        self.record_recoverable_error(error);
    }
}
