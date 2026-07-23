use std::num::NonZeroU64;

use media_core::MediaTime;

use crate::{DesktopControlRevision, DesktopIntegrationResult};

/// Нейтральный идентификатор desktop-команды, не связанный с D-Bus serial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DesktopCommandRequestId(NonZeroU64);

impl DesktopCommandRequestId {
    /// Создаёт request id из app/backend-owned монотонного счётчика.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    /// Возвращает непрозрачное числовое значение для correlation и тестов.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Нейтральный request id timeline seek; `player-core` использует тот же смысл без D-Bus типов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimelineSeekRequestId(NonZeroU64);

impl TimelineSeekRequestId {
    /// Создаёт correlation id из монотонного desktop request id.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    /// Возвращает непрозрачное числовое значение.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Neutral terminal/preflight outcome без D-Bus и player error типов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopTimelineSeekOutcome {
    Applied {
        request_id: TimelineSeekRequestId,
        position: MediaTime,
    },
    InvalidRange {
        request_id: TimelineSeekRequestId,
    },
    StaleTrack {
        request_id: TimelineSeekRequestId,
    },
    StaleInstance {
        request_id: TimelineSeekRequestId,
    },
    NotSeekable {
        request_id: TimelineSeekRequestId,
    },
    Expired {
        request_id: TimelineSeekRequestId,
    },
    Failed {
        request_id: TimelineSeekRequestId,
    },
    BeyondEnd {
        request_id: TimelineSeekRequestId,
    },
    ArithmeticOverflow {
        request_id: TimelineSeekRequestId,
    },
}

impl DesktopTimelineSeekOutcome {
    #[must_use]
    pub const fn request_id(self) -> TimelineSeekRequestId {
        match self {
            Self::Applied { request_id, .. }
            | Self::InvalidRange { request_id }
            | Self::StaleTrack { request_id }
            | Self::StaleInstance { request_id }
            | Self::NotSeekable { request_id }
            | Self::Expired { request_id }
            | Self::Failed { request_id }
            | Self::BeyondEnd { request_id }
            | Self::ArithmeticOverflow { request_id } => request_id,
        }
    }
}

/// App-owned identity активного media без locator/title и без zbus типов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DesktopTrackKey {
    /// Установленный playlist item: lineage отличает новый allocator lifecycle.
    PlaylistItem { lineage: u64, item_id: NonZeroU64 },

    /// Установленное media вне committed playlist row.
    ExternalMedia { lineage: u64 },
}

/// Три значения writable MPRIS `LoopStatus` в neutral vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopLoopStatus {
    /// Не повторять после конца.
    None,

    /// Повторять текущий track.
    Track,

    /// Повторять очередь.
    Playlist,
}

impl DesktopLoopStatus {
    /// Возвращает точное spec spelling для Linux adapter-а.
    #[must_use]
    pub const fn as_mpris_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Track => "Track",
            Self::Playlist => "Playlist",
        }
    }

    /// Проверяет входной setter без передачи строки в app/controller.
    #[must_use]
    pub fn from_mpris_str(value: &str) -> Option<Self> {
        match value {
            "None" => Some(Self::None),
            "Track" => Some(Self::Track),
            "Playlist" => Some(Self::Playlist),
            _ => None,
        }
    }
}

/// Нормализованная process-lifetime громкость приложения.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectiveVolume(f32);

impl EffectiveVolume {
    /// Нормализует MPRIS volume: negative -> 0, above-one -> 1, non-finite invalid.
    pub fn from_mpris(value: f64) -> Result<Self, EffectiveVolumeError> {
        if !value.is_finite() {
            return Err(EffectiveVolumeError::NonFinite);
        }
        let normalized = value.clamp(0.0, 1.0);
        let checked = normalized as f32;
        if !checked.is_finite() {
            return Err(EffectiveVolumeError::Conversion);
        }
        Ok(Self(checked))
    }

    /// Создаёт значение из уже валидированного app config/player representation.
    pub fn from_player(value: f32) -> Result<Self, EffectiveVolumeError> {
        if !value.is_finite() {
            return Err(EffectiveVolumeError::NonFinite);
        }
        Ok(Self(value.clamp(0.0, 1.0)))
    }

    /// Возвращает значение для player boundary.
    #[must_use]
    pub const fn as_player(self) -> f32 {
        self.0
    }

    /// Возвращает значение для MPRIS property.
    #[must_use]
    pub fn as_mpris(self) -> f64 {
        f64::from(self.0)
    }
}

/// Причина отказа checked volume normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveVolumeError {
    /// NaN или бесконечность не имеют spec-safe effective value.
    NonFinite,

    /// Checked conversion в player representation не удалась.
    Conversion,
}

/// Нейтральные desktop actions; playback policy исполняет только app UI thread.
#[derive(Debug, Clone, PartialEq)]
pub enum DesktopTransportAction {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    SetLoopStatus(DesktopLoopStatus),
    SetShuffle(bool),
    Seek {
        request_id: TimelineSeekRequestId,
        offset_microseconds: i64,
    },
    SetPosition {
        request_id: TimelineSeekRequestId,
        track_key: DesktopTrackKey,
        position_microseconds: i64,
    },
    SetVolume(EffectiveVolume),
    /// V1 fixed-rate contract передаёт только нулевую скорость как Pause intent.
    SetRatePause,
}

/// Одна bounded mailbox command с correlation.
#[derive(Debug, Clone, PartialEq)]
pub struct DesktopCommand {
    pub request_id: DesktopCommandRequestId,
    /// Binding snapshot-а, из которого backend разрешил player-dependent action.
    pub observed_control_revision: Option<DesktopControlRevision>,
    pub action: DesktopTransportAction,
}

/// Process-lifetime app sink; backend не знает player worker/channel.
pub trait DesktopCommandSink: Send + Sync + 'static {
    /// Принимает одну neutral command либо возвращает typed backpressure/disconnect.
    fn send_desktop_command(&self, command: DesktopCommand) -> DesktopIntegrationResult<()>;
}

impl DesktopCommandSink for std::sync::Arc<dyn DesktopCommandSink> {
    fn send_desktop_command(&self, command: DesktopCommand) -> DesktopIntegrationResult<()> {
        self.as_ref().send_desktop_command(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_volume_normalizes_spec_edges_before_checked_conversion() {
        assert_eq!(
            EffectiveVolume::from_mpris(-4.0)
                .expect("negative is clamped")
                .as_player(),
            0.0
        );
        assert_eq!(
            EffectiveVolume::from_mpris(4.0)
                .expect("above one is best-fit")
                .as_player(),
            1.0
        );
        assert_eq!(
            EffectiveVolume::from_mpris(f64::NAN),
            Err(EffectiveVolumeError::NonFinite)
        );
        assert_eq!(
            EffectiveVolume::from_mpris(f64::INFINITY),
            Err(EffectiveVolumeError::NonFinite)
        );
    }

    #[test]
    fn loop_status_parser_accepts_only_spec_values() {
        assert_eq!(
            DesktopLoopStatus::from_mpris_str("None"),
            Some(DesktopLoopStatus::None)
        );
        assert_eq!(
            DesktopLoopStatus::from_mpris_str("Track"),
            Some(DesktopLoopStatus::Track)
        );
        assert_eq!(
            DesktopLoopStatus::from_mpris_str("Playlist"),
            Some(DesktopLoopStatus::Playlist)
        );
        assert_eq!(DesktopLoopStatus::from_mpris_str("Queue"), None);
    }
}
