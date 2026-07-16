use std::io::{BufRead, BufReader};
use std::num::NonZeroU64;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use media_core::{MediaDuration, MediaTime};
use zbus::zvariant::ObjectPath;

use super::track_identity::media_duration_to_mpris_microseconds;
use super::*;
use crate::{
    DesktopCapabilities, DesktopMetadata, DesktopPlaybackStatus, DesktopSnapshotRevision,
    DesktopTrackKey,
};

fn view() -> DesktopSnapshotView {
    DesktopSnapshotView {
        revision: DesktopSnapshotRevision::new(1),
        playback_status: DesktopPlaybackStatus::Playing,
        metadata: DesktopMetadata::default(),
        position: MediaTime::from_secs(42),
        capabilities: DesktopCapabilities {
            can_go_next: true,
            can_go_previous: true,
            can_play: true,
            can_pause: true,
            can_seek: true,
        },
        loop_status: DesktopLoopStatus::Playlist,
        shuffle: true,
        volume: EffectiveVolume::from_player(0.5).expect("volume"),
        seeked: None,
    }
}

#[derive(Default)]
struct RecordingSink {
    commands: Mutex<Vec<DesktopCommand>>,
    backpressure: bool,
}

impl DesktopCommandSink for RecordingSink {
    fn send_desktop_command(&self, command: DesktopCommand) -> DesktopIntegrationResult<()> {
        if self.backpressure {
            return Err(DesktopIntegrationError::CommandBackpressure);
        }
        self.commands.lock().expect("command log").push(command);
        Ok(())
    }
}

fn interface(snapshot: DesktopSnapshotView, sink: Arc<RecordingSink>) -> MprisPlayerInterface {
    MprisPlayerInterface::new(sink, LatestSnapshotHandle::new(snapshot))
}

struct PrivateSessionBus {
    address: String,
    child: Child,
}

impl PrivateSessionBus {
    fn spawn() -> Self {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("hermetic dbus-daemon must start");
        let stdout = child.stdout.take().expect("dbus address pipe");
        let address = BufReader::new(stdout)
            .lines()
            .next()
            .expect("dbus address line")
            .expect("valid dbus address");
        Self { address, child }
    }
}

impl Drop for PrivateSessionBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn claim_on_private_bus(address: &str) -> DesktopIntegrationResult<Connection> {
    let sink = Arc::new(RecordingSink::default());
    let player = interface(view(), sink);
    let builder = connection::Builder::address(address).map_err(map_zbus_error)?;
    build_mpris_connection(builder, player)
}

#[test]
fn track_paths_are_valid_non_reserved_and_distinguish_duplicate_rows() {
    let first = DesktopTrackKey::PlaylistItem {
        lineage: 7,
        item_id: NonZeroU64::new(11).unwrap(),
    };
    let second = DesktopTrackKey::PlaylistItem {
        lineage: 7,
        item_id: NonZeroU64::new(12).unwrap(),
    };
    let first_path = encode_track_key(first).expect("valid path");
    let second_path = encode_track_key(second).expect("valid path");
    assert!(first_path.starts_with("/com/rustiplayer/Track/"));
    assert_ne!(first_path, second_path);
    assert!(ObjectPath::try_from(first_path).is_ok());
}

#[test]
fn external_track_path_uses_lineage_only() {
    let key = DesktopTrackKey::ExternalMedia { lineage: 17 };
    assert_eq!(
        encode_track_key(key).expect("valid path"),
        "/com/rustiplayer/Track/x0000000000000011"
    );
}

#[test]
fn full_dynamic_properties_exclude_position_rate_and_can_control() {
    let properties = full_dynamic_player_properties(&view()).expect("properties");
    for name in [
        "PlaybackStatus",
        "Metadata",
        "LoopStatus",
        "Shuffle",
        "Volume",
        "CanGoNext",
        "CanGoPrevious",
        "CanPlay",
        "CanPause",
        "CanSeek",
    ] {
        assert!(properties.contains_key(name));
    }
    assert!(!properties.contains_key("Position"));
    assert!(!properties.contains_key("Rate"));
    assert!(!properties.contains_key("MinimumRate"));
    assert!(!properties.contains_key("MaximumRate"));
    assert!(!properties.contains_key("CanControl"));
}

#[test]
fn mpris_duration_conversion_saturates_to_i64() {
    assert_eq!(
        media_duration_to_mpris_microseconds(MediaDuration::from_secs(2)),
        2_000_000
    );
}

#[test]
fn false_capabilities_do_not_attempt_enqueue_and_play_pause_returns_error() {
    let sink = Arc::new(RecordingSink::default());
    let mut snapshot = view();
    snapshot.capabilities = DesktopCapabilities::default();
    let interface = interface(snapshot, Arc::clone(&sink));

    assert!(interface.next().is_ok());
    assert!(interface.previous().is_ok());
    assert!(interface.play().is_ok());
    assert!(interface.pause().is_ok());
    assert!(interface.seek(1).is_ok());
    assert!(interface.play_pause().is_err());
    assert!(sink.commands.lock().expect("command log").is_empty());
}

#[test]
fn true_capability_preserves_typed_enqueue_backpressure() {
    let sink = Arc::new(RecordingSink {
        commands: Mutex::new(Vec::new()),
        backpressure: true,
    });
    let interface = interface(view(), sink);
    assert!(matches!(interface.next(), Err(fdo::Error::Failed(_))));
}

#[test]
fn fixed_rate_setter_only_enqueues_pause_for_zero() {
    let sink = Arc::new(RecordingSink::default());
    let interface = interface(view(), Arc::clone(&sink));
    assert!(interface.set_rate(1.0).is_ok());
    assert!(interface.set_rate(2.0).is_ok());
    assert!(interface.set_rate(f64::NAN).is_err());
    assert!(interface.set_rate(f64::INFINITY).is_err());
    assert!(interface.set_rate(0.0).is_ok());

    let commands = sink.commands.lock().expect("command log");
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        commands[0].action,
        DesktopTransportAction::SetRatePause
    ));
}

#[test]
fn zero_rate_with_false_can_pause_is_noop_without_enqueue() {
    let sink = Arc::new(RecordingSink::default());
    let mut snapshot = view();
    snapshot.capabilities.can_pause = false;
    let interface = interface(snapshot, Arc::clone(&sink));
    assert!(interface.set_rate(0.0).is_ok());
    assert!(sink.commands.lock().expect("command log").is_empty());
}

#[test]
fn stale_and_invalid_set_position_are_noop_before_backpressured_enqueue() {
    let sink = Arc::new(RecordingSink {
        commands: Mutex::new(Vec::new()),
        backpressure: true,
    });
    let mut snapshot = view();
    snapshot.metadata = DesktopMetadata {
        track_key: Some(DesktopTrackKey::ExternalMedia { lineage: 17 }),
        title: None,
        source_label: None,
        duration: Some(MediaDuration::from_secs(60)),
    };
    let interface = interface(snapshot, Arc::clone(&sink));

    let stale_path =
        ObjectPath::try_from("/com/rustiplayer/Track/x0000000000000018").expect("valid stale path");
    let active_path = ObjectPath::try_from("/com/rustiplayer/Track/x0000000000000011")
        .expect("valid active path");
    assert!(interface.set_position(stale_path, 1_000_000).is_ok());
    assert!(interface.set_position(active_path.clone(), -1).is_ok());
    assert!(interface.set_position(active_path, 60_000_001).is_ok());
    assert!(sink.commands.lock().expect("command log").is_empty());
}

#[test]
fn occupied_bus_name_is_typed_separately_from_backend_failure() {
    assert!(matches!(
        map_zbus_error(zbus::Error::NameTaken),
        DesktopIntegrationError::MprisBusNameUnavailable
    ));
    assert!(matches!(
        map_zbus_error(zbus::Error::Failure("connection failed".to_string())),
        DesktopIntegrationError::Backend(_)
    ));
}

#[test]
fn bus_claim_uses_fixed_base_name_without_replacement_or_fallback() {
    let source = include_str!("../linux.rs");
    assert_eq!(MPRIS_BUS_NAME, "org.mpris.MediaPlayer2.rustiplayer");
    assert!(source.contains(".name(MPRIS_BUS_NAME)"));
    assert!(source.contains(".allow_name_replacements(false)"));
    assert!(source.contains(".replace_existing_names(false)"));
}

#[test]
fn occupied_private_bus_claim_is_nonqueued_and_never_acquires_after_release() {
    let bus = PrivateSessionBus::spawn();
    let first = claim_on_private_bus(&bus.address).expect("first fixed-name owner");
    let (result_tx, result_rx) = mpsc::channel();
    let contender_address = bus.address.clone();
    let contender = std::thread::spawn(move || {
        let result = claim_on_private_bus(&contender_address);
        result_tx.send(result).expect("claim result receiver");
    });

    let occupied = match result_rx.recv_timeout(Duration::from_secs(1)) {
        Ok(result) => result,
        Err(error) => {
            drop(first);
            contender.join().expect("queued contender thread");
            panic!("occupied claim queued instead of returning immediately: {error}");
        }
    };
    assert!(matches!(
        occupied,
        Err(DesktopIntegrationError::MprisBusNameUnavailable)
    ));
    contender.join().expect("nonqueued contender thread");

    drop(first);
    let explicit_retry = claim_on_private_bus(&bus.address)
        .expect("only a new explicit claim may acquire the released name");
    drop(explicit_retry);
}
