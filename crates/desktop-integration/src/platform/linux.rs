use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use media_core::{MediaDuration, MediaTime};
use player_core::PlayerCommand;
use tracing::debug;
use zbus::blocking::{Connection, connection};
use zbus::names::InterfaceName;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};
use zbus::{fdo, interface};

use crate::{
    DesktopBackendKind, DesktopCommandSink, DesktopIntegrationError, DesktopIntegrationEvent,
    DesktopIntegrationResult, DesktopMetadata, DesktopSnapshotChange, DesktopSnapshotView,
    LatestSnapshotHandle, LatestSnapshotSource, MprisCommand, MprisSetPositionRequest,
    map_mpris_command_to_player_commands,
};

use super::{BackendControlCommand, BackendHandle};

const MPRIS_BUS_NAME: &str = "org.mpris.MediaPlayer2.rustiplayer";
const MPRIS_OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

/// Запускает Linux MPRIS backend на отдельном blocking thread.
pub(crate) fn spawn(
    command_sink: Arc<dyn DesktopCommandSink>,
    snapshot_source: LatestSnapshotHandle,
    event_tx: Sender<DesktopIntegrationEvent>,
) -> DesktopIntegrationResult<BackendHandle> {
    let (control_tx, control_rx) = unbounded();
    let thread_command_sink = Arc::clone(&command_sink);
    let join_handle = thread::Builder::new()
        .name("desktop-mpris".to_string())
        .spawn(move || {
            run_mpris_thread(thread_command_sink, snapshot_source, event_tx, control_rx);
        })
        .map_err(|error| DesktopIntegrationError::ThreadSpawn(error.to_string()))?;

    Ok(BackendHandle::threaded(
        command_sink,
        control_tx,
        join_handle,
    ))
}

/// Инициализирует zbus connection и держит thread живым до shutdown.
fn run_mpris_thread(
    command_sink: Arc<dyn DesktopCommandSink>,
    snapshot_source: LatestSnapshotHandle,
    event_tx: Sender<DesktopIntegrationEvent>,
    control_rx: Receiver<BackendControlCommand>,
) {
    let backend = DesktopBackendKind::LinuxMpris;
    let runtime = match MprisRuntime::new(command_sink, snapshot_source, event_tx.clone()) {
        Ok(runtime) => runtime,
        Err(error) => {
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

/// Runtime Linux backend-а с live D-Bus connection.
struct MprisRuntime {
    /// Blocking zbus connection владеет registered object server.
    connection: Connection,

    /// Snapshot source нужен для property-change values вне D-Bus handler-ов.
    snapshot_source: LatestSnapshotHandle,

    /// Event channel наружу в app shell.
    event_tx: Sender<DesktopIntegrationEvent>,
}

impl MprisRuntime {
    /// Регистрирует root/player MPRIS interfaces на session bus.
    fn new(
        command_sink: Arc<dyn DesktopCommandSink>,
        snapshot_source: LatestSnapshotHandle,
        event_tx: Sender<DesktopIntegrationEvent>,
    ) -> DesktopIntegrationResult<Self> {
        let root_interface = MprisRootInterface;
        let player_interface =
            MprisPlayerInterface::new(command_sink, snapshot_source.clone(), event_tx.clone());
        let connection = connection::Builder::session()
            .map_err(map_zbus_error)?
            .name(MPRIS_BUS_NAME)
            .map_err(map_zbus_error)?
            .serve_at(MPRIS_OBJECT_PATH, root_interface)
            .map_err(map_zbus_error)?
            .serve_at(MPRIS_OBJECT_PATH, player_interface)
            .map_err(map_zbus_error)?
            .build()
            .map_err(map_zbus_error)?;

        Ok(Self {
            connection,
            snapshot_source,
            event_tx,
        })
    }

    /// Обрабатывает control channel; D-Bus calls обслуживает zbus object server.
    fn run(&self, control_rx: Receiver<BackendControlCommand>) {
        while let Ok(command) = control_rx.recv() {
            match command {
                BackendControlCommand::SnapshotChanged(change) => {
                    self.emit_snapshot_property_changes(change);
                }
                BackendControlCommand::Shutdown => break,
            }
        }
    }

    /// Публикует `PropertiesChanged` только для низкочастотных MPRIS properties.
    fn emit_snapshot_property_changes(&self, change: DesktopSnapshotChange) {
        let snapshot = match self.snapshot_source.latest_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.emit_backend_error(error);
                return;
            }
        };
        let view = DesktopSnapshotView::from_player_snapshot(&snapshot);
        let changed_properties = match changed_player_properties(&view, change) {
            Ok(changed_properties) => changed_properties,
            Err(error) => {
                self.emit_backend_error(error);
                return;
            }
        };

        if changed_properties.is_empty() {
            return;
        }

        let interface_name = match InterfaceName::try_from(MPRIS_PLAYER_INTERFACE) {
            Ok(interface_name) => interface_name,
            Err(error) => {
                self.emit_backend_error(DesktopIntegrationError::Backend(error.to_string()));
                return;
            }
        };
        let interface_ref = match self
            .connection
            .object_server()
            .interface::<_, MprisPlayerInterface>(MPRIS_OBJECT_PATH)
        {
            Ok(interface_ref) => interface_ref,
            Err(error) => {
                self.emit_backend_error(map_zbus_error(error));
                return;
            }
        };
        let emit_result = zbus::block_on(fdo::Properties::properties_changed(
            interface_ref.signal_emitter(),
            interface_name,
            changed_properties,
            Cow::Borrowed(&[] as &[&str]),
        ));

        match emit_result {
            Ok(()) => emit_event(
                &self.event_tx,
                DesktopIntegrationEvent::SnapshotPropertiesChanged {
                    backend: DesktopBackendKind::LinuxMpris,
                    change,
                },
            ),
            Err(error) => self.emit_backend_error(map_zbus_error(error)),
        }
    }

    /// Отправляет backend error как event и tracing warning.
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

/// Root interface `org.mpris.MediaPlayer2`.
struct MprisRootInterface;

#[interface(name = "org.mpris.MediaPlayer2")]
impl MprisRootInterface {
    /// Просьбу поднять окно пока не обрабатываем: window lifecycle остаётся в app shell.
    fn raise(&self) {}

    /// MPRIS Quit не закрывает app, потому что desktop backend не владеет lifecycle.
    fn quit(&self) {}

    /// Rustiplayer не поддерживает remote quit через MPRIS.
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }

    /// Fullscreen управляется window/input layer, не MPRIS backend-ом.
    #[zbus(property)]
    fn fullscreen(&self) -> bool {
        false
    }

    /// Setter нужен для spec-compatible property, но backend не меняет window state.
    #[zbus(property)]
    fn set_fullscreen(&self, _fullscreen: bool) {}

    /// MPRIS backend не умеет менять fullscreen сам.
    #[zbus(property)]
    fn can_set_fullscreen(&self) -> bool {
        false
    }

    /// Raise сейчас no-op, поэтому явно сообщаем false.
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }

    /// TrackList interface не реализован в этой сессии.
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    /// Имя приложения для desktop media widgets.
    #[zbus(property)]
    fn identity(&self) -> &str {
        "Rustiplayer"
    }

    /// Desktop entry id без `.desktop`, как ожидает MPRIS.
    #[zbus(property)]
    fn desktop_entry(&self) -> &str {
        "rustiplayer"
    }

    /// URI schemes, которые может открыть shell/source layer.
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<&str> {
        vec!["file", "http", "https"]
    }

    /// MIME types текущего production path.
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<&str> {
        vec!["video/webm", "video/x-matroska"]
    }
}

/// Player interface `org.mpris.MediaPlayer2.Player`.
struct MprisPlayerInterface {
    /// Worker command boundary.
    command_sink: Arc<dyn DesktopCommandSink>,

    /// Latest snapshot boundary.
    snapshot_source: LatestSnapshotHandle,

    /// Event channel наружу.
    event_tx: Sender<DesktopIntegrationEvent>,
}

impl MprisPlayerInterface {
    /// Собирает player interface dependencies.
    fn new(
        command_sink: Arc<dyn DesktopCommandSink>,
        snapshot_source: LatestSnapshotHandle,
        event_tx: Sender<DesktopIntegrationEvent>,
    ) -> Self {
        Self {
            command_sink,
            snapshot_source,
            event_tx,
        }
    }

    /// Отправляет mapped commands в worker boundary.
    fn send_mpris_command(&self, command: MprisCommand) -> fdo::Result<Option<MediaTime>> {
        let snapshot = self
            .snapshot_source
            .latest_snapshot()
            .map_err(to_fdo_error)?;
        let commands = map_mpris_command_to_player_commands(command, &snapshot);
        let seek_target = commands.iter().find_map(extract_absolute_seek_target);

        for player_command in commands {
            self.command_sink
                .send_desktop_command(player_command)
                .map_err(to_fdo_error)?;
        }

        Ok(seek_target)
    }

    /// Возвращает latest desktop snapshot view для property getter-ов.
    fn desktop_view(&self) -> fdo::Result<DesktopSnapshotView> {
        self.snapshot_source
            .latest_snapshot()
            .map(|snapshot| DesktopSnapshotView::from_player_snapshot(&snapshot))
            .map_err(to_fdo_error)
    }

    /// Отправляет `Seeked` и event после принятого seek command.
    fn emit_seeked(
        &self,
        emitter: SignalEmitter<'_>,
        target_position: Option<MediaTime>,
    ) -> fdo::Result<()> {
        let Some(target_position) = target_position else {
            return Ok(());
        };
        let position_microseconds = media_time_to_mpris_microseconds(target_position);

        zbus::block_on(Self::seeked(&emitter, position_microseconds)).map_err(fdo::Error::ZBus)?;
        emit_event(
            &self.event_tx,
            DesktopIntegrationEvent::Seeked {
                backend: DesktopBackendKind::LinuxMpris,
                position: target_position,
            },
        );

        Ok(())
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl MprisPlayerInterface {
    /// Next не входит в текущий player command scope.
    fn next(&self) {}

    /// Previous не входит в текущий player command scope.
    fn previous(&self) {}

    /// MPRIS `Pause` -> `PlayerCommand::Pause`.
    fn pause(&self) -> fdo::Result<()> {
        self.send_mpris_command(MprisCommand::Pause).map(|_| ())
    }

    /// MPRIS `PlayPause` -> `PlayerCommand::TogglePlayback`.
    fn play_pause(&self) -> fdo::Result<()> {
        self.send_mpris_command(MprisCommand::PlayPause).map(|_| ())
    }

    /// MPRIS `Stop` -> pause + seek zero, media stays open.
    fn stop(&self) -> fdo::Result<()> {
        self.send_mpris_command(MprisCommand::Stop).map(|_| ())
    }

    /// MPRIS `Play` -> `PlayerCommand::Play`.
    fn play(&self) -> fdo::Result<()> {
        self.send_mpris_command(MprisCommand::Play).map(|_| ())
    }

    /// MPRIS `Seek` приходит как signed offset в microseconds.
    fn seek(
        &self,
        offset_microseconds: i64,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        let target_position = self.send_mpris_command(MprisCommand::Seek {
            offset_microseconds,
        })?;

        self.emit_seeked(emitter, target_position)
    }

    /// MPRIS `SetPosition` приходит как absolute microseconds и track id.
    fn set_position(
        &self,
        track_id: ObjectPath<'_>,
        position_microseconds: i64,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        let target_position =
            self.send_mpris_command(MprisCommand::SetPosition(MprisSetPositionRequest {
                track_id: track_id.to_string(),
                position_microseconds,
            }))?;

        self.emit_seeked(emitter, target_position)
    }

    /// Signal MPRIS `Seeked`.
    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    /// LoopStatus пока фиксирован: loop-mode нет в player contract.
    #[zbus(property)]
    fn loop_status(&self) -> &str {
        "None"
    }

    /// Setter оставлен no-op для compatibility с MPRIS clients.
    #[zbus(property)]
    fn set_loop_status(&self, _loop_status: &str) {}

    /// Rate фиксирован на normal speed.
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    /// Setter оставлен no-op до появления playback-rate contract.
    #[zbus(property)]
    fn set_rate(&self, _rate: f64) {}

    /// Shuffle не применим к single-media player.
    #[zbus(property)]
    fn shuffle(&self) -> bool {
        false
    }

    /// Setter оставлен no-op до появления playlist model.
    #[zbus(property)]
    fn set_shuffle(&self, _shuffle: bool) {}

    /// Metadata включает track id, title и duration как `mpris:length`.
    #[zbus(property)]
    fn metadata(&self) -> fdo::Result<HashMap<String, OwnedValue>> {
        let view = self.desktop_view()?;
        mpris_metadata_values(&view.metadata).map_err(to_fdo_error)
    }

    /// Volume берётся из player snapshot.
    #[zbus(property)]
    fn volume(&self) -> fdo::Result<f64> {
        let snapshot = self
            .snapshot_source
            .latest_snapshot()
            .map_err(to_fdo_error)?;

        Ok(f64::from(snapshot.volume))
    }

    /// Volume setter проходит через worker command boundary.
    #[zbus(property)]
    fn set_volume(&self, volume: f64) -> fdo::Result<()> {
        if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
            return Err(fdo::Error::InvalidArgs(format!(
                "MPRIS volume must be within 0.0..=1.0, got {volume}"
            )));
        }

        self.command_sink
            .send_desktop_command(PlayerCommand::SetVolume(volume as f32))
            .map_err(to_fdo_error)
    }

    /// Position отдаётся по запросу клиента, но не публикуется high-rate PropertiesChanged.
    #[zbus(property)]
    fn position(&self) -> fdo::Result<i64> {
        let view = self.desktop_view()?;

        Ok(media_time_to_mpris_microseconds(view.position))
    }

    /// MinimumRate фиксирован, потому что rate control не реализован.
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    /// MaximumRate фиксирован, потому что rate control не реализован.
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    /// Next не поддержан текущим player command scope.
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        false
    }

    /// Previous не поддержан текущим player command scope.
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        false
    }

    /// Play command доступен через worker boundary.
    #[zbus(property)]
    fn can_play(&self) -> fdo::Result<bool> {
        let view = self.desktop_view()?;

        Ok(view.has_media())
    }

    /// Pause command доступен через worker boundary.
    #[zbus(property)]
    fn can_pause(&self) -> fdo::Result<bool> {
        let view = self.desktop_view()?;

        Ok(view.has_media())
    }

    /// Seek доступен только когда timeline сообщает seekable.
    #[zbus(property)]
    fn can_seek(&self) -> fdo::Result<bool> {
        let view = self.desktop_view()?;

        Ok(view.can_seek)
    }

    /// Desktop controls доступны, пока backend жив.
    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }

    /// PlaybackStatus маппится в три значения MPRIS.
    #[zbus(property)]
    fn playback_status(&self) -> fdo::Result<&'static str> {
        let view = self.desktop_view()?;

        Ok(view.playback_status.as_mpris_str())
    }
}

/// Собирает changed properties для `org.freedesktop.DBus.Properties.PropertiesChanged`.
fn changed_player_properties(
    view: &DesktopSnapshotView,
    change: DesktopSnapshotChange,
) -> DesktopIntegrationResult<HashMap<&'static str, Value<'static>>> {
    let mut changed_properties = HashMap::new();

    if change.playback_status_changed {
        changed_properties.insert(
            "PlaybackStatus",
            Value::from(view.playback_status.as_mpris_str().to_string()),
        );
    }

    if change.metadata_changed {
        changed_properties.insert(
            "Metadata",
            Value::from(mpris_metadata_values(&view.metadata)?),
        );
        changed_properties.insert("CanPlay", Value::from(view.has_media()));
        changed_properties.insert("CanPause", Value::from(view.has_media()));
    }

    if change.can_seek_changed {
        changed_properties.insert("CanSeek", Value::from(view.can_seek));
    }

    Ok(changed_properties)
}

/// Конвертирует neutral metadata в MPRIS `a{sv}`.
fn mpris_metadata_values(
    metadata: &DesktopMetadata,
) -> DesktopIntegrationResult<HashMap<String, OwnedValue>> {
    let mut metadata_values = HashMap::new();

    if let Some(track_id) = &metadata.track_id {
        let object_path = ObjectPath::try_from(track_id.as_str())
            .map_err(|error| DesktopIntegrationError::Backend(error.to_string()))?;
        metadata_values.insert("mpris:trackid".to_string(), OwnedValue::from(object_path));
    }

    if let Some(title) = &metadata.title {
        metadata_values.insert(
            "xesam:title".to_string(),
            owned_value_from_string(title.clone())?,
        );
    }

    if let Some(duration) = metadata.duration {
        metadata_values.insert(
            "mpris:length".to_string(),
            OwnedValue::from(media_duration_to_mpris_microseconds(duration)),
        );
    }

    Ok(metadata_values)
}

/// Упаковывает String в zvariant OwnedValue с typed error propagation.
fn owned_value_from_string(value: String) -> DesktopIntegrationResult<OwnedValue> {
    OwnedValue::try_from(Value::from(value))
        .map_err(|error| DesktopIntegrationError::Backend(error.to_string()))
}

/// Достаёт absolute seek target из neutral command.
fn extract_absolute_seek_target(command: &PlayerCommand) -> Option<MediaTime> {
    let PlayerCommand::Seek(request) = command else {
        return None;
    };
    let player_core::SeekTarget::Absolute(position) = request.target else {
        return None;
    };

    Some(position)
}

/// Конвертирует media time в MPRIS signed microseconds.
fn media_time_to_mpris_microseconds(position: MediaTime) -> i64 {
    duration_to_mpris_microseconds(position.as_duration())
}

/// Конвертирует media duration в MPRIS signed microseconds.
fn media_duration_to_mpris_microseconds(duration: MediaDuration) -> i64 {
    duration_to_mpris_microseconds(duration.as_duration())
}

/// Saturating conversion из Rust Duration в MPRIS `x`.
fn duration_to_mpris_microseconds(duration: std::time::Duration) -> i64 {
    duration.as_micros().min(i64::MAX as u128) as i64
}

/// Преобразует zbus error в neutral backend error.
fn map_zbus_error(error: zbus::Error) -> DesktopIntegrationError {
    DesktopIntegrationError::Backend(error.to_string())
}

/// Преобразует neutral error в D-Bus FDO error.
fn to_fdo_error(error: DesktopIntegrationError) -> fdo::Error {
    fdo::Error::Failed(error.to_string())
}

/// Отправляет event и логирует закрытый receiver вместо silent ignore.
fn emit_event(event_tx: &Sender<DesktopIntegrationEvent>, event: DesktopIntegrationEvent) {
    if let Err(error) = event_tx.send(event) {
        debug!(error = %error, "Desktop integration event receiver is closed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DesktopPlaybackStatus, MPRIS_CURRENT_TRACK_ID};
    use media_core::MediaDuration;

    #[test]
    fn mpris_duration_conversion_saturates_to_i64() {
        assert_eq!(
            media_duration_to_mpris_microseconds(MediaDuration::from_secs(2)),
            2_000_000
        );
    }

    #[test]
    fn changed_properties_do_not_include_position() {
        let view = DesktopSnapshotView {
            playback_status: DesktopPlaybackStatus::Playing,
            metadata: DesktopMetadata {
                track_id: Some(MPRIS_CURRENT_TRACK_ID.to_string()),
                title: Some("Sample".to_string()),
                source_label: Some("sample.webm".to_string()),
                duration: Some(MediaDuration::from_secs(1)),
            },
            position: MediaTime::from_secs(42),
            can_seek: true,
        };
        let change = DesktopSnapshotChange {
            playback_status_changed: true,
            metadata_changed: false,
            can_seek_changed: true,
        };

        let properties = changed_player_properties(&view, change).unwrap();

        assert!(properties.contains_key("PlaybackStatus"));
        assert!(properties.contains_key("CanSeek"));
        assert!(!properties.contains_key("Position"));
    }
}
