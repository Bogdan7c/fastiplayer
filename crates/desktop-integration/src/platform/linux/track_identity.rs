//! Единственный MPRIS object-path codec для neutral app-owned track identity.

use media_core::{MediaDuration, MediaTime};
use zbus::zvariant::ObjectPath;

use crate::{DesktopIntegrationError, DesktopIntegrationResult, DesktopTrackKey};

pub(super) fn encode_track_key(key: DesktopTrackKey) -> DesktopIntegrationResult<String> {
    let path = match key {
        DesktopTrackKey::PlaylistItem { lineage, item_id } => format!(
            "/com/rustiplayer/Track/q{lineage:016x}_i{:016x}",
            item_id.get()
        ),
        DesktopTrackKey::ExternalMedia { lineage } => {
            format!("/com/rustiplayer/Track/x{lineage:016x}")
        }
    };
    ObjectPath::try_from(path.as_str())
        .map_err(|error| DesktopIntegrationError::Backend(error.to_string()))?;
    Ok(path)
}

pub(super) fn media_time_to_mpris_microseconds(position: MediaTime) -> i64 {
    duration_to_mpris_microseconds(position.as_duration())
}

pub(super) fn media_duration_to_mpris_microseconds(duration: MediaDuration) -> i64 {
    duration_to_mpris_microseconds(duration.as_duration())
}

fn duration_to_mpris_microseconds(duration: std::time::Duration) -> i64 {
    duration.as_micros().min(i64::MAX as u128) as i64
}
