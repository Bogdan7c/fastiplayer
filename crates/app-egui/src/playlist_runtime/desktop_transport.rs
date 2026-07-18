//! Process-lifetime desktop/MPRIS owner и renderer-neutral snapshot adapter.

use std::num::NonZeroU64;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

use desktop_integration::{
    DesktopCapabilities, DesktopCommand, DesktopCommandSink, DesktopIntegration,
    DesktopIntegrationError, DesktopIntegrationResult, DesktopIntegrationShutdownOutcome,
    DesktopLoopStatus, DesktopMetadata, DesktopPlaybackStatus, DesktopSeeked,
    DesktopSnapshotRevision, DesktopSnapshotView, DesktopTimelineSeekOutcome, DesktopTrackKey,
    EffectiveVolume, TimelineSeekRequestId,
};
use media_core::{MediaDuration, MediaTime};
use player_core::{PlaybackState, PlayerSnapshot};
use playlist_core::RepeatMode;
use tracing::{debug, warn};

use super::PlaylistRuntime;
use super::controller::AppTransportDisposition;
use super::controller::{
    ControllerStableIntentDispatch, StablePlaybackIntent, TransportGuardOutcome,
};
use super::identity::TransportActionOrigin;
use super::transport_ui::NavigationControlAvailability;
use crate::app_wake::{AppWakePort, WakeDelivery};
use crate::process_shutdown::ShutdownDeadline;

const DESKTOP_COMMAND_CAPACITY: usize = 16;

/// Cloneable producer, который не знает ни controller, ни player sender.
#[derive(Clone)]
struct DesktopMailboxSink {
    command_tx: SyncSender<DesktopCommand>,
    wake_port: AppWakePort,
}

impl DesktopCommandSink for DesktopMailboxSink {
    fn send_desktop_command(&self, command: DesktopCommand) -> DesktopIntegrationResult<()> {
        self.command_tx
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => DesktopIntegrationError::CommandBackpressure,
                TrySendError::Disconnected(_) => DesktopIntegrationError::CommandDisconnected,
            })?;
        if matches!(self.wake_port.request_wake(), WakeDelivery::EventLoopClosed) {
            return Err(DesktopIntegrationError::CommandDisconnected);
        }
        Ok(())
    }
}

/// Единственный process owner backend-а, mailbox-а, volume и stable track key.
pub(super) struct DesktopTransportOwner {
    integration: Option<DesktopIntegration>,
    command_rx: Receiver<DesktopCommand>,
    command_sink: DesktopMailboxSink,
    revision: u64,
    effective_volume: EffectiveVolume,
    pending_player_volume: Option<EffectiveVolume>,
    active_track_key: Option<DesktopTrackKey>,
    active_lineage: Option<u64>,
    last_snapshot: DesktopSnapshotView,
    last_seek_outcome: Option<DesktopTimelineSeekOutcome>,
}

impl DesktopTransportOwner {
    fn active_track_matches(&self, track_key: DesktopTrackKey) -> bool {
        self.active_track_key == Some(track_key)
    }

    pub(super) fn new(wake_port: AppWakePort) -> Self {
        let (command_tx, command_rx) = sync_channel(DESKTOP_COMMAND_CAPACITY);
        let effective_volume = EffectiveVolume::from_player(1.0).expect("constant volume is valid");
        let last_snapshot = DesktopSnapshotView::neutral(effective_volume);
        Self {
            integration: None,
            command_rx,
            command_sink: DesktopMailboxSink {
                command_tx,
                wake_port,
            },
            revision: 0,
            effective_volume,
            pending_player_volume: None,
            active_track_key: None,
            active_lineage: None,
            last_snapshot,
            last_seek_outcome: None,
        }
    }

    /// AppShell вызывает этот boundary только после successful process lease.
    pub(super) fn start(&mut self, initial_volume: f32) {
        self.effective_volume = EffectiveVolume::from_player(initial_volume)
            .unwrap_or_else(|_| EffectiveVolume::from_player(1.0).expect("constant volume"));
        self.last_snapshot.volume = self.effective_volume;
        match DesktopIntegration::spawn(self.command_sink.clone(), self.last_snapshot.clone()) {
            Ok(integration) => self.integration = Some(integration),
            Err(DesktopIntegrationError::MprisBusNameUnavailable) => {
                warn!("MPRIS base bus уже занят; backend отключён без fallback/retry")
            }
            Err(error) => {
                warn!(error = %error, "MPRIS backend отключён; playback/UI продолжают работу")
            }
        }
    }

    pub(super) fn drain_commands(&self) -> Vec<DesktopCommand> {
        let mut commands = Vec::with_capacity(DESKTOP_COMMAND_CAPACITY);
        while let Ok(command) = self.command_rx.try_recv() {
            commands.push(command);
        }
        commands
    }

    pub(super) const fn effective_volume(&self) -> EffectiveVolume {
        self.effective_volume
    }

    pub(super) fn set_effective_volume(&mut self, volume: EffectiveVolume) -> bool {
        if self.effective_volume == volume {
            return false;
        }
        self.effective_volume = volume;
        self.pending_player_volume = Some(volume);
        true
    }

    /// Фиксирует desktop identity только на границе successful install.
    ///
    /// Новый player instance или tombstone той же lineage не меняю
    /// MPRIS path. `None` означает pending/failure/suspend и тоже сохраняет
    /// последнюю успешно установленную identity.
    fn observe_installed_track(
        &mut self,
        active_lineage: Option<u64>,
        item_id: Option<NonZeroU64>,
    ) {
        let Some(lineage) = active_lineage else {
            return;
        };
        if self.active_lineage == Some(lineage) {
            return;
        }
        self.active_lineage = Some(lineage);
        self.active_track_key = Some(
            item_id.map_or(DesktopTrackKey::ExternalMedia { lineage }, |item_id| {
                DesktopTrackKey::PlaylistItem { lineage, item_id }
            }),
        );
    }

    pub(super) fn publish_from_player(
        &mut self,
        player_snapshot: &PlayerSnapshot,
        runtime: &mut PlaylistRuntime,
    ) {
        let active = runtime
            .playlist_controller()
            .and_then(|controller| controller.active_media());
        let active_instance = active.map(|identity| identity.media_instance_id());
        let active_lineage =
            active.map(|identity| identity.lineage_id().expose_value_for_correlation());
        let item_id = active
            .and_then(|identity| identity.item_id())
            .and_then(|item_id| NonZeroU64::new(item_id.expose_value_for_persistence()));
        self.observe_installed_track(active_lineage, item_id);

        let has_binding = active.is_some() && player_snapshot.media_instance_id == active_instance;
        if has_binding
            && let Ok(player_volume) = EffectiveVolume::from_player(player_snapshot.volume)
        {
            if self.pending_player_volume == Some(player_volume) {
                self.pending_player_volume = None;
            } else if self.pending_player_volume.is_none() {
                self.effective_volume = player_volume;
            }
        }
        let navigation = runtime
            .playlist_transport_ui_model(player_snapshot.timeline.current_position.as_duration());
        let stopped = runtime.playlist_controller().is_some_and(|controller| {
            controller.transport_disposition() == AppTransportDisposition::Stopped
        });
        let playback_status = if stopped {
            DesktopPlaybackStatus::Stopped
        } else {
            map_playback_status(player_snapshot.playback_state)
        };
        let duration = player_snapshot
            .timeline
            .duration
            .or_else(|| player_snapshot.duration.map(MediaDuration::from_duration));
        let mut metadata = DesktopMetadata {
            track_key: self.active_track_key,
            title: player_snapshot.media_title.clone(),
            source_label: player_snapshot.source_label.clone(),
            duration,
        };
        if metadata.track_key == self.last_snapshot.metadata.track_key {
            metadata.title = metadata
                .title
                .or_else(|| self.last_snapshot.metadata.title.clone());
            metadata.source_label = metadata
                .source_label
                .or_else(|| self.last_snapshot.metadata.source_label.clone());
            metadata.duration = metadata.duration.or(self.last_snapshot.metadata.duration);
        }
        let (loop_status, shuffle) =
            runtime
                .playlist_controller()
                .map_or((DesktopLoopStatus::None, false), |controller| {
                    (
                        match controller.repeat_mode() {
                            RepeatMode::StopAtEnd => DesktopLoopStatus::None,
                            RepeatMode::RepeatOne => DesktopLoopStatus::Track,
                            RepeatMode::RepeatQueue => DesktopLoopStatus::Playlist,
                        },
                        controller.queue().shuffle_enabled(),
                    )
                });
        self.revision = self.revision.saturating_add(1);
        let view = DesktopSnapshotView {
            revision: DesktopSnapshotRevision::new(self.revision),
            playback_status,
            metadata,
            position: player_snapshot.timeline.current_position,
            capabilities: DesktopCapabilities {
                can_go_next: has_binding
                    && navigation.next != NavigationControlAvailability::Disabled,
                can_go_previous: has_binding
                    && navigation.previous != NavigationControlAvailability::Disabled,
                can_play: has_binding,
                can_pause: has_binding,
                can_seek: has_binding && player_snapshot.timeline.seekable,
            },
            loop_status,
            shuffle,
            volume: self.effective_volume,
            seeked: None,
        };
        self.publish(view);
    }

    pub(super) fn publish_detached(&mut self) {
        self.revision = self.revision.saturating_add(1);
        let mut view = self.last_snapshot.clone();
        view.revision = DesktopSnapshotRevision::new(self.revision);
        view.playback_status = DesktopPlaybackStatus::Stopped;
        view.capabilities = DesktopCapabilities::default();
        view.position = MediaTime::ZERO;
        view.volume = self.effective_volume;
        view.seeked = None;
        self.publish(view);
    }

    fn publish(&mut self, view: DesktopSnapshotView) {
        self.last_snapshot = view.clone();
        if let Some(integration) = &self.integration {
            if let Err(error) = integration.publish_snapshot(view) {
                warn!(error = %error, "Не удалось опубликовать committed desktop snapshot");
            }
            for event in integration.drain_events() {
                debug!(?event, "Desktop integration event");
            }
        }
    }

    fn publish_seeked(&mut self, request_id: TimelineSeekRequestId, position: MediaTime) {
        self.last_seek_outcome = Some(DesktopTimelineSeekOutcome::Applied {
            request_id,
            position,
        });
        self.revision = self.revision.saturating_add(1);
        let mut view = self.last_snapshot.clone();
        view.revision = DesktopSnapshotRevision::new(self.revision);
        view.position = position;
        view.seeked = Some(DesktopSeeked {
            request_id,
            position,
        });
        self.publish(view);
    }

    fn record_seek_outcome(&mut self, outcome: DesktopTimelineSeekOutcome) {
        self.last_seek_outcome = Some(outcome);
    }

    pub(super) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> Option<DesktopIntegrationShutdownOutcome> {
        self.integration
            .as_mut()
            .map(|integration| integration.shutdown_until(deadline.expires_at()))
    }
}

fn map_playback_status(state: PlaybackState) -> DesktopPlaybackStatus {
    match state {
        PlaybackState::Playing
        | PlaybackState::Buffering
        | PlaybackState::Seeking
        | PlaybackState::Draining => DesktopPlaybackStatus::Playing,
        PlaybackState::Paused => DesktopPlaybackStatus::Paused,
        PlaybackState::Idle
        | PlaybackState::Opening
        | PlaybackState::Scrubbing
        | PlaybackState::Ended
        | PlaybackState::Stopped
        | PlaybackState::Failed => DesktopPlaybackStatus::Stopped,
    }
}

impl PlaylistRuntime {
    pub(crate) fn record_desktop_seek_outcome(&mut self, outcome: DesktopTimelineSeekOutcome) {
        if let Some(owner) = self.desktop_transport.as_mut() {
            owner.record_seek_outcome(outcome);
        }
    }

    pub(crate) fn desktop_track_matches(&self, track_key: DesktopTrackKey) -> bool {
        self.desktop_transport
            .as_ref()
            .is_some_and(|owner| owner.active_track_matches(track_key))
    }

    pub(crate) fn start_desktop_transport(&mut self, initial_volume: f32) {
        if let Some(owner) = self.desktop_transport.as_mut() {
            owner.start(initial_volume);
        }
    }

    pub(crate) fn drain_desktop_commands(&self) -> Vec<DesktopCommand> {
        self.desktop_transport
            .as_ref()
            .map_or_else(Vec::new, DesktopTransportOwner::drain_commands)
    }

    pub(crate) fn desktop_effective_volume(&self) -> EffectiveVolume {
        self.desktop_transport
            .as_ref()
            .map(DesktopTransportOwner::effective_volume)
            .unwrap_or_else(|| EffectiveVolume::from_player(1.0).expect("constant volume"))
    }

    pub(crate) fn set_desktop_effective_volume(&mut self, volume: EffectiveVolume) -> bool {
        self.desktop_transport
            .as_mut()
            .is_some_and(|owner| owner.set_effective_volume(volume))
    }

    pub(crate) fn publish_desktop_snapshot(&mut self, snapshot: &PlayerSnapshot) {
        let Some(mut owner) = self.desktop_transport.take() else {
            return;
        };
        owner.publish_from_player(snapshot, self);
        self.desktop_transport = Some(owner);
    }

    /// Публикует Seeked только после matching player-core Applied outcome.
    pub(crate) fn publish_desktop_seeked(
        &mut self,
        request_id: TimelineSeekRequestId,
        position: MediaTime,
    ) {
        if let Some(owner) = self.desktop_transport.as_mut() {
            owner.publish_seeked(request_id, position);
        }
    }

    pub(crate) fn publish_detached_desktop_snapshot(&mut self) {
        if let Some(owner) = self.desktop_transport.as_mut() {
            owner.publish_detached();
        }
    }

    pub(crate) fn shutdown_desktop_transport_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> Option<DesktopIntegrationShutdownOutcome> {
        self.desktop_transport
            .as_mut()
            .and_then(|owner| owner.shutdown_until(deadline))
    }

    pub(crate) fn record_desktop_playback_intent(
        &mut self,
        intent: StablePlaybackIntent,
    ) -> Option<ControllerStableIntentDispatch> {
        self.controller
            .as_mut()?
            .record_stable_transport_intent(intent, TransportActionOrigin::Mpris)
    }

    pub(crate) fn toggle_desktop_playback_intent(
        &mut self,
    ) -> Option<ControllerStableIntentDispatch> {
        self.controller
            .as_mut()?
            .toggle_stable_transport_intent(TransportActionOrigin::Mpris)
    }

    pub(crate) fn request_desktop_stop(
        &mut self,
    ) -> Option<Result<player_core::ExactMediaTransportRequest, TransportGuardOutcome>> {
        self.controller
            .as_mut()?
            .neutral_stop(TransportActionOrigin::Mpris)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_wake::AppWakeOwner;

    fn owner() -> DesktopTransportOwner {
        DesktopTransportOwner::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime))
    }

    #[test]
    fn effective_volume_survives_detached_snapshot_without_player_binding() {
        let mut owner = owner();
        let volume = EffectiveVolume::from_player(0.25).expect("valid volume");
        assert!(owner.set_effective_volume(volume));
        owner.publish_detached();
        assert_eq!(owner.effective_volume(), volume);
        assert_eq!(owner.last_snapshot.volume, volume);
        assert_eq!(
            owner.last_snapshot.capabilities,
            DesktopCapabilities::default()
        );
    }

    #[test]
    fn seeked_receipt_is_one_revisioned_snapshot_without_dynamic_capability_change() {
        let mut owner = owner();
        let request_id = TimelineSeekRequestId::new(NonZeroU64::new(7).expect("non-zero"));
        let position = MediaTime::from_secs(3);
        owner.publish_seeked(request_id, position);
        assert_eq!(owner.last_snapshot.position, position);
        assert_eq!(
            owner.last_snapshot.seeked,
            Some(DesktopSeeked {
                request_id,
                position
            })
        );
    }

    #[test]
    fn track_path_identity_changes_only_with_successful_new_lineage() {
        let mut owner = owner();
        let first_item_id = NonZeroU64::new(11).expect("non-zero item id");
        let replacement_item_id = NonZeroU64::new(22).expect("non-zero item id");

        owner.observe_installed_track(Some(7), Some(first_item_id));
        let first_key = owner.active_track_key;

        // Same-lineage reinstall и tombstone не меняю exported path.
        owner.observe_installed_track(Some(7), Some(replacement_item_id));
        assert_eq!(owner.active_track_key, first_key);
        owner.observe_installed_track(Some(7), None);
        assert_eq!(owner.active_track_key, first_key);

        // Pending/failure/suspend не стирают последний successful install.
        owner.observe_installed_track(None, None);
        assert_eq!(owner.active_track_key, first_key);

        owner.observe_installed_track(Some(8), Some(replacement_item_id));
        assert_eq!(
            owner.active_track_key,
            Some(DesktopTrackKey::PlaylistItem {
                lineage: 8,
                item_id: replacement_item_id,
            })
        );
    }

    #[test]
    fn non_applied_seek_outcome_does_not_publish_false_seeked_signal() {
        let mut owner = owner();
        let request_id = TimelineSeekRequestId::new(NonZeroU64::new(9).expect("non-zero"));
        let revision_before = owner.last_snapshot.revision;

        owner.record_seek_outcome(DesktopTimelineSeekOutcome::StaleTrack { request_id });

        assert_eq!(owner.last_snapshot.revision, revision_before);
        assert_eq!(owner.last_snapshot.seeked, None);
        assert_eq!(
            owner.last_seek_outcome,
            Some(DesktopTimelineSeekOutcome::StaleTrack { request_id })
        );
    }
}
