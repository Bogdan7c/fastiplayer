//! Session-owned projection dynamic live snapshot-а в public player timeline.

use crossbeam_channel::Receiver;
use frame_server_core::CancelScrubReason;
use media_core::{
    DynamicMediaTimelinePort, DynamicMediaTimelinePortGeneration, DynamicMediaTimelineRevision,
    DynamicMediaTimelineSnapshot, MediaTime, TimelineMode, TimelineNotSeekableReason,
    TimelineRange,
};

use crate::seek_state::SeekTargetRetention;
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
    availability_range: Option<media_core::TimelineRange>,
    activity_disconnected: bool,
}

/// Единственный session-owned active binding.
#[derive(Debug, Default)]
pub(super) struct DynamicTimelineRuntime {
    binding: Option<DynamicTimelineBinding>,
}

/// Player-owned решение live same-item restore по fresh snapshot текущего port-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LiveSameItemPositionRestoreDecision {
    /// Старая абсолютная позиция всё ещё доступна и должна пройти exact seek lifecycle.
    RestoreRetainedPosition(std::time::Duration),
    /// Новая live generation уже стартовала с provider-declared safe edge.
    AdjustedToLiveEdge {
        requested_position: std::time::Duration,
        live_edge: std::time::Duration,
        reason: crate::InstalledLiveEdgeAdjustmentReason,
    },
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
                    availability_range: initial.state.availability_range(),
                    activity_disconnected: false,
                });
                self.apply_dynamic_timeline_snapshot(initial, true);
            }
        }
    }

    /// Сообщает lifecycle-коду, что installed media владеет изменяемой live timeline.
    pub(super) const fn has_dynamic_timeline_binding(&self) -> bool {
        self.dynamic_timeline.binding.is_some()
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

    /// Возвращает safe live edge, если pause-position уже выпала из DVR window.
    ///
    /// Сам refresh намеренно не двигает paused playback: иначе sliding window
    /// запускало бы seek на каждом MPD update. Решение применяется один раз на
    /// явном Play и проходит через обычный seek lifecycle с flush/generation.
    pub(super) fn expired_live_resume_target(&self) -> Option<MediaTime> {
        if self.snapshot.timeline.mode != TimelineMode::Live {
            return None;
        }
        let binding = self.dynamic_timeline.binding.as_ref()?;
        if self.snapshot.media_instance_id != Some(binding.media_instance_id) {
            return None;
        }
        let availability_range = binding.availability_range?;
        let current_position = MediaTime::from_duration(self.current_source_position);
        if availability_range.contains(current_position) {
            return None;
        }
        self.snapshot
            .timeline
            .live_edge
            .map(|live_edge| availability_range.clamp(live_edge))
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
        let availability_range = dynamic_snapshot.state.availability_range();
        binding.observed_revision = dynamic_snapshot.revision;
        binding.availability_range = availability_range;
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

        self.expire_dynamic_seek_targets_outside_retention(seekable_range, availability_range);
    }

    /// Перечитывает latest snapshot exact installed live port-а и решает restore.
    pub(super) fn decide_live_same_item_position_restore(
        &mut self,
        media_instance_id: MediaInstanceId,
        previous_absolute_position: std::time::Duration,
    ) -> Result<LiveSameItemPositionRestoreDecision, crate::InstalledMediaStateRestoreOutcome> {
        let Some(binding) = self.dynamic_timeline.binding.as_ref() else {
            return Err(crate::InstalledMediaStateRestoreOutcome::StaleInstance);
        };
        if binding.media_instance_id != media_instance_id
            || self.snapshot.media_instance_id != Some(media_instance_id)
        {
            return Err(crate::InstalledMediaStateRestoreOutcome::StaleInstance);
        }

        let fresh_snapshot = binding.port.observe().snapshot;
        self.apply_dynamic_timeline_snapshot(fresh_snapshot, false);

        let requested_media_time = MediaTime::from_duration(previous_absolute_position);
        if fresh_snapshot
            .state
            .seekable_range()
            .is_some_and(|range| range.contains(requested_media_time))
        {
            return Ok(
                LiveSameItemPositionRestoreDecision::RestoreRetainedPosition(
                    previous_absolute_position,
                ),
            );
        }

        let reason = match fresh_snapshot.state.seekable_range() {
            Some(available_range) => {
                crate::InstalledLiveEdgeAdjustmentReason::PreviousPositionOutsideDvr {
                    available_range,
                }
            }
            None => crate::InstalledLiveEdgeAdjustmentReason::DvrWindowUnavailable,
        };
        let live_edge = fresh_snapshot.state.live_edge().as_duration();
        self.current_source_position = live_edge;
        self.snapshot
            .set_timeline_position(fresh_snapshot.state.live_edge());
        self.pipeline.set_media_clock_base(live_edge);

        Ok(LiveSameItemPositionRestoreDecision::AdjustedToLiveEdge {
            requested_position: previous_absolute_position,
            live_edge,
            reason,
        })
    }

    /// Проверяет target по диапазону, который соответствует исходному seek intent-у.
    fn expire_dynamic_seek_targets_outside_retention(
        &mut self,
        public_seekable_range: Option<TimelineRange>,
        availability_range: Option<TimelineRange>,
    ) {
        if let Some(seek_commit) = self.seek_runtime.active_commit() {
            let retained_range = seek_target_retention_range(
                seek_commit.target_retention,
                public_seekable_range,
                availability_range,
            );
            let target_expired =
                !retained_range.is_some_and(|range| range.contains(seek_commit.target_position));
            let staged_anchor_expired = self.installed_staged_position.as_ref().is_some_and(
                |installed| {
                    self.snapshot.media_instance_id == Some(installed.media_instance_id)
                        && matches!(
                            installed.outcome,
                            super::staged_media_install::InstalledStagedPositionOutcome::AwaitingSeekCommit {
                                seek_generation,
                            } if seek_generation == seek_commit.generation
                        )
                        && !public_seekable_range
                            .is_some_and(|range| range.contains(seek_commit.actual_position))
                },
            ) || self.pending_installed_position_restore.as_ref().is_some_and(|pending| {
                pending.requires_live_anchor_retention
                    && self.snapshot.media_instance_id == Some(pending.media_instance_id)
                    && pending.seek_generation == seek_commit.generation
                    && !public_seekable_range
                        .is_some_and(|range| range.contains(seek_commit.actual_position))
            });

            if target_expired || staged_anchor_expired {
                let diagnostic_range = if target_expired {
                    retained_range
                } else {
                    public_seekable_range
                };
                self.expire_dynamic_seek_or_scrub(diagnostic_range);
            }
            return;
        }

        if let Some((target_position, target_retention)) =
            self.prepared_demux_seek.pending_timeline_target()
        {
            let retained_range = seek_target_retention_range(
                target_retention,
                public_seekable_range,
                availability_range,
            );
            if !retained_range.is_some_and(|range| range.contains(target_position)) {
                self.expire_dynamic_seek_or_scrub(retained_range);
            }
            return;
        }

        let public_target_expired = self
            .snapshot
            .timeline
            .target_position
            .is_some_and(|target| {
                !public_seekable_range.is_some_and(|range| range.contains(target))
            });
        if public_target_expired {
            self.expire_dynamic_seek_or_scrub(public_seekable_range);
        }
    }

    fn expire_dynamic_seek_or_scrub(&mut self, available_range: Option<media_core::TimelineRange>) {
        if let Some(seek_commit) = self.seek_runtime.active_commit() {
            self.fail_dynamic_seek_target_expired(seek_commit, available_range);
            return;
        }

        let prepared_seek_pending = self.prepared_demux_seek.receipt_pending();
        self.expire_pending_exact_timeline_seek(available_range);
        self.prepared_demux_seek.supersede_pending();
        let error = crate::PlayerError::new(
            crate::PlayerErrorKind::SeekTargetExpired,
            format!("Live seek/scrub target expired outside latest DVR range {available_range:?}"),
        );
        self.fail_pending_seek_receipts(error.clone());
        if prepared_seek_pending {
            self.fail_started_demux_seek(error);
            return;
        }

        self.cancel_active_scrub_for_external_command(CancelScrubReason::StaleContext);
        self.record_recoverable_error(error);
    }
}

/// Выбирает owner-range, который имеет право инвалидировать конкретный seek target.
fn seek_target_retention_range(
    target_retention: SeekTargetRetention,
    public_seekable_range: Option<TimelineRange>,
    availability_range: Option<TimelineRange>,
) -> Option<TimelineRange> {
    match target_retention {
        SeekTargetRetention::ExactPublicRange => public_seekable_range,
        SeekTargetRetention::LiveAvailability => availability_range,
    }
}
