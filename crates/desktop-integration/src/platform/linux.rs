use std::borrow::Cow;
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use tracing::debug;
use zbus::blocking::{Connection, connection};
use zbus::names::InterfaceName;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue};
use zbus::{fdo, interface};

use crate::{
    DesktopBackendKind, DesktopCommand, DesktopCommandRequestId, DesktopCommandSink,
    DesktopIntegrationError, DesktopIntegrationEvent, DesktopIntegrationResult, DesktopLoopStatus,
    DesktopSnapshotChange, DesktopSnapshotView, DesktopTimelineSeekOutcome, DesktopTransportAction,
    EffectiveVolume, LatestSnapshotHandle, LatestSnapshotSource, TimelineSeekRequestId,
};

use super::{BackendControlCommand, BackendHandle};

mod snapshot_properties;
mod track_identity;

use snapshot_properties::{full_dynamic_player_properties, mpris_metadata_values};
use track_identity::{encode_track_key, media_time_to_mpris_microseconds};

/// Единственное process base name: fallback suffix и late retry запрещены D78.
const MPRIS_BUS_NAME: &str = "org.mpris.MediaPlayer2.rustiplayer";
const MPRIS_OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

/// Запускает thread и синхронно подтверждает первоначальный non-queued bus claim.
pub(crate) fn spawn(
    command_sink: Arc<dyn DesktopCommandSink>,
    snapshot_source: LatestSnapshotHandle,
    event_tx: Sender<DesktopIntegrationEvent>,
) -> DesktopIntegrationResult<BackendHandle> {
    let (control_tx, control_rx) = unbounded();
    let (startup_tx, startup_rx) = bounded(1);
    let thread_sink = Arc::clone(&command_sink);
    let join_handle = thread::Builder::new()
        .name("desktop-mpris".to_string())
        .spawn(move || {
            run_mpris_thread(
                thread_sink,
                snapshot_source,
                event_tx,
                control_rx,
                startup_tx,
            )
        })
        .map_err(|error| DesktopIntegrationError::ThreadSpawn(error.to_string()))?;

    match startup_rx.recv() {
        Ok(Ok(())) => Ok(BackendHandle::threaded(
            command_sink,
            control_tx,
            join_handle,
        )),
        Ok(Err(error)) => {
            let _ = join_handle.join();
            Err(error)
        }
        Err(_) => {
            let _ = join_handle.join();
            Err(DesktopIntegrationError::BackendChannelDisconnected)
        }
    }
}

fn run_mpris_thread(
    command_sink: Arc<dyn DesktopCommandSink>,
    snapshot_source: LatestSnapshotHandle,
    event_tx: Sender<DesktopIntegrationEvent>,
    control_rx: Receiver<BackendControlCommand>,
    startup_tx: Sender<DesktopIntegrationResult<()>>,
) {
    let backend = DesktopBackendKind::LinuxMpris;
    let runtime = match MprisRuntime::new(command_sink, snapshot_source, event_tx.clone()) {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = startup_tx.send(Err(error.clone()));
            emit_event(
                &event_tx,
                DesktopIntegrationEvent::BackendError { backend, error },
            );
            emit_event(
                &event_tx,
                DesktopIntegrationEvent::BackendStopped { backend },
            );
            return;
        }
    };
    if startup_tx.send(Ok(())).is_err() {
        return;
    }
    emit_event(
        &event_tx,
        DesktopIntegrationEvent::BackendStarted { backend },
    );
    runtime.run(control_rx);
    emit_event(
        &event_tx,
        DesktopIntegrationEvent::BackendStopped { backend },
    );
}

struct MprisRuntime {
    connection: Connection,
    snapshot_source: LatestSnapshotHandle,
    event_tx: Sender<DesktopIntegrationEvent>,
}

impl MprisRuntime {
    fn new(
        command_sink: Arc<dyn DesktopCommandSink>,
        snapshot_source: LatestSnapshotHandle,
        event_tx: Sender<DesktopIntegrationEvent>,
    ) -> DesktopIntegrationResult<Self> {
        let player_interface = MprisPlayerInterface::new(command_sink, snapshot_source.clone());
        let builder = connection::Builder::session().map_err(map_zbus_error)?;
        let connection = build_mpris_connection(builder, player_interface)?;
        Ok(Self {
            connection,
            snapshot_source,
            event_tx,
        })
    }

    fn run(&self, control_rx: Receiver<BackendControlCommand>) {
        while let Ok(command) = control_rx.recv() {
            match command {
                BackendControlCommand::SnapshotChanged(change) => {
                    self.emit_snapshot_notifications(change)
                }
                BackendControlCommand::Shutdown => break,
            }
        }
    }

    fn emit_snapshot_notifications(&self, change: DesktopSnapshotChange) {
        let view = match self.snapshot_source.latest_snapshot() {
            Ok(view) => view,
            Err(error) => return self.emit_backend_error(error),
        };
        let interface_ref = match self
            .connection
            .object_server()
            .interface::<_, MprisPlayerInterface>(MPRIS_OBJECT_PATH)
        {
            Ok(interface_ref) => interface_ref,
            Err(error) => return self.emit_backend_error(map_zbus_error(error)),
        };

        if change.dynamic_properties_changed {
            let properties = match full_dynamic_player_properties(&view) {
                Ok(properties) => properties,
                Err(error) => return self.emit_backend_error(error),
            };
            let interface_name = match InterfaceName::try_from(MPRIS_PLAYER_INTERFACE) {
                Ok(name) => name,
                Err(error) => {
                    return self
                        .emit_backend_error(DesktopIntegrationError::Backend(error.to_string()));
                }
            };
            let result = zbus::block_on(fdo::Properties::properties_changed(
                interface_ref.signal_emitter(),
                interface_name,
                properties,
                Cow::Borrowed(&[] as &[&str]),
            ));
            if let Err(error) = result {
                return self.emit_backend_error(map_zbus_error(error));
            }
            emit_event(
                &self.event_tx,
                DesktopIntegrationEvent::SnapshotPropertiesChanged {
                    backend: DesktopBackendKind::LinuxMpris,
                    change,
                },
            );
        }

        if let Some(seeked) = change.seeked
            && let Err(error) = zbus::block_on(MprisPlayerInterface::seeked(
                interface_ref.signal_emitter(),
                media_time_to_mpris_microseconds(seeked.position),
            ))
        {
            self.emit_backend_error(map_zbus_error(error));
        }
    }

    fn emit_backend_error(&self, error: DesktopIntegrationError) {
        emit_event(
            &self.event_tx,
            DesktopIntegrationEvent::BackendError {
                backend: DesktopBackendKind::LinuxMpris,
                error,
            },
        );
    }
}

fn build_mpris_connection(
    builder: connection::Builder<'_>,
    player_interface: MprisPlayerInterface,
) -> DesktopIntegrationResult<Connection> {
    builder
        .name(MPRIS_BUS_NAME)
        .map_err(map_zbus_error)?
        // zbus 5.15 defaults both flags to true; D78 requires an explicit non-replacing claim.
        .allow_name_replacements(false)
        .replace_existing_names(false)
        .serve_at(MPRIS_OBJECT_PATH, MprisRootInterface)
        .map_err(map_zbus_error)?
        .serve_at(MPRIS_OBJECT_PATH, player_interface)
        .map_err(map_zbus_error)?
        .build()
        .map_err(map_zbus_error)
}

/// Корневой MPRIS interface не владеет transport state.
struct MprisRootInterface;

#[interface(name = "org.mpris.MediaPlayer2")]
impl MprisRootInterface {
    fn raise(&self) {}
    fn quit(&self) {}
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn fullscreen(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn set_fullscreen(&self, _fullscreen: bool) {}
    #[zbus(property)]
    fn can_set_fullscreen(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn identity(&self) -> &str {
        "Rustiplayer"
    }
    #[zbus(property)]
    fn desktop_entry(&self) -> &str {
        "rustiplayer"
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<&str> {
        vec!["file", "http", "https"]
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<&str> {
        vec!["video/webm", "video/x-matroska", "video/mp4"]
    }
}

struct MprisPlayerInterface {
    command_sink: Arc<dyn DesktopCommandSink>,
    snapshot_source: LatestSnapshotHandle,
    next_request_id: AtomicU64,
}

impl MprisPlayerInterface {
    fn new(
        command_sink: Arc<dyn DesktopCommandSink>,
        snapshot_source: LatestSnapshotHandle,
    ) -> Self {
        Self {
            command_sink,
            snapshot_source,
            next_request_id: AtomicU64::new(1),
        }
    }

    fn desktop_view(&self) -> fdo::Result<DesktopSnapshotView> {
        self.snapshot_source.latest_snapshot().map_err(to_fdo_error)
    }

    fn next_request_id(&self) -> fdo::Result<DesktopCommandRequestId> {
        let value = self
            .next_request_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| fdo::Error::Failed("desktop request id exhausted".to_string()))?;
        let non_zero = NonZeroU64::new(value)
            .ok_or_else(|| fdo::Error::Failed("desktop request id exhausted".to_string()))?;
        Ok(DesktopCommandRequestId::new(non_zero))
    }

    fn send_action(&self, action: DesktopTransportAction) -> fdo::Result<()> {
        let request_id = self.next_request_id()?;
        self.command_sink
            .send_desktop_command(DesktopCommand { request_id, action })
            .map_err(to_fdo_error)
    }

    fn seek_request_id(request_id: DesktopCommandRequestId) -> fdo::Result<TimelineSeekRequestId> {
        NonZeroU64::new(request_id.get())
            .map(TimelineSeekRequestId::new)
            .ok_or_else(|| fdo::Error::Failed("seek request id exhausted".to_string()))
    }

    fn send_seek(&self, offset_microseconds: i64) -> fdo::Result<()> {
        let request_id = self.next_request_id()?;
        self.command_sink
            .send_desktop_command(DesktopCommand {
                request_id,
                action: DesktopTransportAction::Seek {
                    request_id: Self::seek_request_id(request_id)?,
                    offset_microseconds,
                },
            })
            .map_err(to_fdo_error)
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl MprisPlayerInterface {
    fn next(&self) -> fdo::Result<()> {
        if !self.desktop_view()?.capabilities.can_go_next {
            return Ok(());
        }
        self.send_action(DesktopTransportAction::Next)
    }
    fn previous(&self) -> fdo::Result<()> {
        if !self.desktop_view()?.capabilities.can_go_previous {
            return Ok(());
        }
        self.send_action(DesktopTransportAction::Previous)
    }
    fn pause(&self) -> fdo::Result<()> {
        if !self.desktop_view()?.capabilities.can_pause {
            return Ok(());
        }
        self.send_action(DesktopTransportAction::Pause)
    }
    fn play_pause(&self) -> fdo::Result<()> {
        if !self.desktop_view()?.capabilities.can_pause {
            return Err(fdo::Error::Failed(
                "PlayPause is unavailable while CanPause is false".to_string(),
            ));
        }
        self.send_action(DesktopTransportAction::PlayPause)
    }
    fn stop(&self) -> fdo::Result<()> {
        self.send_action(DesktopTransportAction::Stop)
    }
    fn play(&self) -> fdo::Result<()> {
        if !self.desktop_view()?.capabilities.can_play {
            return Ok(());
        }
        self.send_action(DesktopTransportAction::Play)
    }
    fn seek(&self, offset_microseconds: i64) -> fdo::Result<()> {
        if !self.desktop_view()?.capabilities.can_seek {
            return Ok(());
        }
        self.send_seek(offset_microseconds)
    }
    fn set_position(
        &self,
        track_id: ObjectPath<'_>,
        position_microseconds: i64,
    ) -> fdo::Result<()> {
        let view = self.desktop_view()?;
        if !view.capabilities.can_seek {
            return Ok(());
        }
        let request_id = Self::seek_request_id(self.next_request_id()?)?;
        let Some(track_key) = view.metadata.track_key else {
            let _outcome = DesktopTimelineSeekOutcome::StaleTrack { request_id };
            return Ok(());
        };
        if encode_track_key(track_key).map_err(to_fdo_error)? != track_id.as_str() {
            let _outcome = DesktopTimelineSeekOutcome::StaleTrack { request_id };
            return Ok(());
        }
        if position_microseconds < 0 {
            let _outcome = DesktopTimelineSeekOutcome::InvalidRange { request_id };
            return Ok(());
        }
        if let Some(duration) = view.metadata.duration
            && u128::try_from(position_microseconds).unwrap_or(u128::MAX)
                > duration.as_duration().as_micros()
        {
            let _outcome = DesktopTimelineSeekOutcome::InvalidRange { request_id };
            return Ok(());
        }
        let command_request_id = DesktopCommandRequestId::new(
            NonZeroU64::new(request_id.get()).expect("seek request IDs are non-zero"),
        );
        self.command_sink
            .send_desktop_command(DesktopCommand {
                request_id: command_request_id,
                action: DesktopTransportAction::SetPosition {
                    request_id,
                    track_key,
                    position_microseconds,
                },
            })
            .map_err(to_fdo_error)
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property(emits_changed_signal = "false"))]
    fn loop_status(&self) -> fdo::Result<&'static str> {
        Ok(self.desktop_view()?.loop_status.as_mpris_str())
    }
    #[zbus(property)]
    fn set_loop_status(&self, value: &str) -> fdo::Result<()> {
        let status = DesktopLoopStatus::from_mpris_str(value)
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("invalid LoopStatus: {value}")))?;
        self.send_action(DesktopTransportAction::SetLoopStatus(status))
    }
    #[zbus(property(emits_changed_signal = "const"))]
    fn rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn set_rate(&self, rate: f64) -> fdo::Result<()> {
        if !rate.is_finite() {
            return Err(fdo::Error::InvalidArgs("Rate must be finite".to_string()));
        }
        if rate == 0.0 {
            if !self.desktop_view()?.capabilities.can_pause {
                return Ok(());
            }
            return self.send_action(DesktopTransportAction::SetRatePause);
        }
        Ok(())
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn shuffle(&self) -> fdo::Result<bool> {
        Ok(self.desktop_view()?.shuffle)
    }
    #[zbus(property)]
    fn set_shuffle(&self, shuffle: bool) -> fdo::Result<()> {
        self.send_action(DesktopTransportAction::SetShuffle(shuffle))
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn metadata(&self) -> fdo::Result<HashMap<String, OwnedValue>> {
        mpris_metadata_values(&self.desktop_view()?.metadata).map_err(to_fdo_error)
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn volume(&self) -> fdo::Result<f64> {
        Ok(self.desktop_view()?.volume.as_mpris())
    }
    #[zbus(property)]
    fn set_volume(&self, volume: f64) -> fdo::Result<()> {
        let effective = EffectiveVolume::from_mpris(volume)
            .map_err(|_| fdo::Error::InvalidArgs("Volume must be finite".to_string()))?;
        self.send_action(DesktopTransportAction::SetVolume(effective))
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> fdo::Result<i64> {
        Ok(media_time_to_mpris_microseconds(
            self.desktop_view()?.position,
        ))
    }
    #[zbus(property(emits_changed_signal = "const"))]
    fn minimum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property(emits_changed_signal = "const"))]
    fn maximum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn can_go_next(&self) -> fdo::Result<bool> {
        Ok(self.desktop_view()?.capabilities.can_go_next)
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn can_go_previous(&self) -> fdo::Result<bool> {
        Ok(self.desktop_view()?.capabilities.can_go_previous)
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn can_play(&self) -> fdo::Result<bool> {
        Ok(self.desktop_view()?.capabilities.can_play)
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn can_pause(&self) -> fdo::Result<bool> {
        Ok(self.desktop_view()?.capabilities.can_pause)
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn can_seek(&self) -> fdo::Result<bool> {
        Ok(self.desktop_view()?.capabilities.can_seek)
    }
    #[zbus(property(emits_changed_signal = "const"))]
    fn can_control(&self) -> bool {
        true
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn playback_status(&self) -> fdo::Result<&'static str> {
        Ok(self.desktop_view()?.playback_status.as_mpris_str())
    }
}

fn map_zbus_error(error: zbus::Error) -> DesktopIntegrationError {
    if matches!(error, zbus::Error::NameTaken) {
        DesktopIntegrationError::MprisBusNameUnavailable
    } else {
        DesktopIntegrationError::Backend(error.to_string())
    }
}

fn to_fdo_error(error: DesktopIntegrationError) -> fdo::Error {
    fdo::Error::Failed(error.to_string())
}

fn emit_event(event_tx: &Sender<DesktopIntegrationEvent>, event: DesktopIntegrationEvent) {
    if let Err(error) = event_tx.send(event) {
        debug!(error = %error, "Desktop integration event receiver is closed");
    }
}

#[cfg(test)]
mod tests;
