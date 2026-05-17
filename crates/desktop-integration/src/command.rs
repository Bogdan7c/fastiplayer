use std::sync::Arc;
use std::time::Duration;

use media_core::{MediaDuration, MediaTime, TimelineRange};
use player_core::{PlayerCommand, PlayerCommandSender, PlayerSnapshot, SeekRequest};

use crate::{DesktopIntegrationError, DesktopIntegrationResult};

/// Stable MPRIS track id для single-current-track модели rustiplayer.
pub const MPRIS_CURRENT_TRACK_ID: &str = "/org/mpris/MediaPlayer2/track/current";

/// Sink команд, который скрывает конкретный worker sender от backend-а.
pub trait DesktopCommandSink: Send + Sync + 'static {
    /// Отправляет команду в player worker boundary.
    fn send_desktop_command(&self, command: PlayerCommand) -> DesktopIntegrationResult<()>;
}

impl DesktopCommandSink for PlayerCommandSender {
    /// Адаптирует existing `PlayerCommandSender` без расширения `player-core`.
    fn send_desktop_command(&self, command: PlayerCommand) -> DesktopIntegrationResult<()> {
        self.try_send(command)
            .map_err(|error| DesktopIntegrationError::CommandSendFailed(error.to_string()))
    }
}

impl DesktopCommandSink for Arc<dyn DesktopCommandSink> {
    /// Позволяет backend-ам хранить type-erased command sink.
    fn send_desktop_command(&self, command: PlayerCommand) -> DesktopIntegrationResult<()> {
        self.as_ref().send_desktop_command(command)
    }
}

/// MPRIS SetPosition request без зависимости neutral layer от zbus ObjectPath.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MprisSetPositionRequest {
    /// Track id, пришедший от MPRIS клиента.
    pub track_id: String,

    /// Absolute target position в microseconds по MPRIS spec.
    pub position_microseconds: i64,
}

/// Поддерживаемые MPRIS player commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MprisCommand {
    /// MPRIS `Play`.
    Play,

    /// MPRIS `Pause`.
    Pause,

    /// MPRIS `PlayPause`.
    PlayPause,

    /// MPRIS `Stop`; в rustiplayer это pause + seek zero, не close media.
    Stop,

    /// MPRIS `Seek` с signed offset в microseconds.
    Seek { offset_microseconds: i64 },

    /// MPRIS `SetPosition` с проверкой track id.
    SetPosition(MprisSetPositionRequest),
}

/// Преобразует MPRIS command в один или несколько neutral `PlayerCommand`.
#[must_use]
pub fn map_mpris_command_to_player_commands(
    command: MprisCommand,
    snapshot: &PlayerSnapshot,
) -> Vec<PlayerCommand> {
    match command {
        MprisCommand::Play => vec![PlayerCommand::Play],
        MprisCommand::Pause => vec![PlayerCommand::Pause],
        MprisCommand::PlayPause => vec![PlayerCommand::TogglePlayback],
        MprisCommand::Stop => stop_commands(),
        MprisCommand::Seek {
            offset_microseconds,
        } => seek_offset_commands(snapshot, offset_microseconds),
        MprisCommand::SetPosition(request) => set_position_commands(snapshot, request),
    }
}

/// Возвращает stop semantics, совместимые с MPRIS и текущим `player-core`.
#[must_use]
pub fn stop_commands() -> Vec<PlayerCommand> {
    vec![
        PlayerCommand::Pause,
        PlayerCommand::Seek(SeekRequest::absolute(MediaTime::ZERO)),
    ]
}

/// Строит absolute seek из signed MPRIS offset и текущего snapshot.
#[must_use]
fn seek_offset_commands(snapshot: &PlayerSnapshot, offset_microseconds: i64) -> Vec<PlayerCommand> {
    if !snapshot.timeline.seekable {
        return Vec::new();
    }

    let current_position = snapshot.timeline.current_position;
    let target_position = resolve_signed_offset(current_position, offset_microseconds);
    let clamped_position = clamp_to_seekable_window(snapshot, target_position);

    vec![PlayerCommand::Seek(SeekRequest::absolute(clamped_position))]
}

/// Строит absolute seek для MPRIS SetPosition, если track id относится к текущему media.
#[must_use]
fn set_position_commands(
    snapshot: &PlayerSnapshot,
    request: MprisSetPositionRequest,
) -> Vec<PlayerCommand> {
    if !snapshot.timeline.seekable || request.track_id != MPRIS_CURRENT_TRACK_ID {
        return Vec::new();
    }

    let target_position = media_time_from_mpris_microseconds(request.position_microseconds);
    let clamped_position = clamp_to_seekable_window(snapshot, target_position);

    vec![PlayerCommand::Seek(SeekRequest::absolute(clamped_position))]
}

/// Применяет signed microseconds offset без underflow.
#[must_use]
fn resolve_signed_offset(position: MediaTime, offset_microseconds: i64) -> MediaTime {
    if offset_microseconds >= 0 {
        position.saturating_add(MediaDuration::from_duration(Duration::from_micros(
            offset_microseconds as u64,
        )))
    } else {
        position.saturating_sub(MediaDuration::from_duration(Duration::from_micros(
            offset_microseconds.unsigned_abs(),
        )))
    }
}

/// Конвертирует MPRIS absolute microseconds в non-negative media time.
#[must_use]
fn media_time_from_mpris_microseconds(position_microseconds: i64) -> MediaTime {
    if position_microseconds <= 0 {
        return MediaTime::ZERO;
    }

    MediaTime::from_duration(Duration::from_micros(position_microseconds as u64))
}

/// Ограничивает seek target известным seekable window, если оно есть.
#[must_use]
fn clamp_to_seekable_window(snapshot: &PlayerSnapshot, target_position: MediaTime) -> MediaTime {
    snapshot
        .timeline
        .seekable_range
        .unwrap_or_else(|| {
            let end = snapshot
                .timeline
                .duration
                .map(|duration| MediaTime::from_duration(duration.as_duration()))
                .unwrap_or(MediaTime::MAX);
            TimelineRange::from_bounds_saturating(MediaTime::ZERO, end)
        })
        .clamp(target_position)
}

#[cfg(test)]
mod tests {
    use media_core::{MediaDuration, MediaTime, TimelineSnapshot};
    use player_core::{PlaybackState, SeekTarget};

    use super::*;

    fn seekable_snapshot(position: MediaTime, duration: MediaDuration) -> PlayerSnapshot {
        let mut snapshot = PlayerSnapshot::empty();
        snapshot.source_label = Some("movie.webm".to_string());
        snapshot.playback_state = PlaybackState::Paused;
        snapshot.timeline = TimelineSnapshot::seekable_vod(duration);
        snapshot.timeline.current_position = position;
        snapshot.current_position = position.as_duration();
        snapshot.duration = Some(duration.as_duration());
        snapshot
    }

    #[test]
    fn maps_play_pause_and_toggle_methods() {
        let snapshot = PlayerSnapshot::empty();

        assert_eq!(
            map_mpris_command_to_player_commands(MprisCommand::Play, &snapshot),
            vec![PlayerCommand::Play]
        );
        assert_eq!(
            map_mpris_command_to_player_commands(MprisCommand::Pause, &snapshot),
            vec![PlayerCommand::Pause]
        );
        assert_eq!(
            map_mpris_command_to_player_commands(MprisCommand::PlayPause, &snapshot),
            vec![PlayerCommand::TogglePlayback]
        );
    }

    #[test]
    fn stop_maps_to_pause_and_seek_zero_without_player_stop() {
        let snapshot = seekable_snapshot(MediaTime::from_secs(42), MediaDuration::from_secs(100));

        let commands = map_mpris_command_to_player_commands(MprisCommand::Stop, &snapshot);

        assert_eq!(commands, stop_commands());
        assert!(!commands.contains(&PlayerCommand::Stop));
    }

    #[test]
    fn mpris_mapping_never_emits_scrub_commands() {
        let snapshot = seekable_snapshot(MediaTime::from_secs(10), MediaDuration::from_secs(100));
        let commands_to_check = [
            MprisCommand::Play,
            MprisCommand::Pause,
            MprisCommand::PlayPause,
            MprisCommand::Stop,
            MprisCommand::Seek {
                offset_microseconds: 5_000_000,
            },
            MprisCommand::SetPosition(MprisSetPositionRequest {
                track_id: MPRIS_CURRENT_TRACK_ID.to_string(),
                position_microseconds: 20_000_000,
            }),
        ];

        for mpris_command in commands_to_check {
            let player_commands = map_mpris_command_to_player_commands(mpris_command, &snapshot);

            assert!(
                player_commands.iter().all(|player_command| !matches!(
                    player_command,
                    PlayerCommand::BeginScrub
                        | PlayerCommand::UpdateScrub(_)
                        | PlayerCommand::PreviewScrub(_)
                        | PlayerCommand::EndScrub { .. }
                )),
                "MPRIS mapping must not emit scrub commands: {player_commands:?}"
            );
        }
    }

    #[test]
    fn seek_maps_signed_offset_to_absolute_target() {
        let snapshot = seekable_snapshot(MediaTime::from_secs(10), MediaDuration::from_secs(100));

        let commands = map_mpris_command_to_player_commands(
            MprisCommand::Seek {
                offset_microseconds: 5_000_000,
            },
            &snapshot,
        );

        assert_eq!(
            commands,
            vec![PlayerCommand::Seek(SeekRequest {
                target: SeekTarget::Absolute(MediaTime::from_secs(15)),
                mode: player_core::SeekMode::Accurate,
            })]
        );
    }

    #[test]
    fn seek_saturates_negative_offset_to_zero() {
        let snapshot = seekable_snapshot(MediaTime::from_secs(3), MediaDuration::from_secs(100));

        let commands = map_mpris_command_to_player_commands(
            MprisCommand::Seek {
                offset_microseconds: -9_000_000,
            },
            &snapshot,
        );

        assert_eq!(
            commands,
            vec![PlayerCommand::Seek(SeekRequest::absolute(MediaTime::ZERO))]
        );
    }

    #[test]
    fn set_position_ignores_stale_track_id() {
        let snapshot = seekable_snapshot(MediaTime::from_secs(0), MediaDuration::from_secs(100));

        let commands = map_mpris_command_to_player_commands(
            MprisCommand::SetPosition(MprisSetPositionRequest {
                track_id: "/org/mpris/MediaPlayer2/track/old".to_string(),
                position_microseconds: 20_000_000,
            }),
            &snapshot,
        );

        assert!(commands.is_empty());
    }

    #[test]
    fn set_position_maps_current_track_to_absolute_seek() {
        let snapshot = seekable_snapshot(MediaTime::from_secs(0), MediaDuration::from_secs(100));

        let commands = map_mpris_command_to_player_commands(
            MprisCommand::SetPosition(MprisSetPositionRequest {
                track_id: MPRIS_CURRENT_TRACK_ID.to_string(),
                position_microseconds: 20_000_000,
            }),
            &snapshot,
        );

        assert_eq!(
            commands,
            vec![PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(20)
            ))]
        );
    }
}
