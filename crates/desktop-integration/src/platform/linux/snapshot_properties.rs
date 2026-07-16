//! Полная committed MPRIS property projection без backend lifecycle логики.

use std::collections::HashMap;

use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::{
    DesktopIntegrationError, DesktopIntegrationResult, DesktopMetadata, DesktopSnapshotView,
};

use super::track_identity::{encode_track_key, media_duration_to_mpris_microseconds};

pub(super) fn full_dynamic_player_properties(
    view: &DesktopSnapshotView,
) -> DesktopIntegrationResult<HashMap<&'static str, Value<'static>>> {
    let mut values = HashMap::new();
    values.insert(
        "PlaybackStatus",
        Value::from(view.playback_status.as_mpris_str().to_string()),
    );
    values.insert(
        "Metadata",
        Value::from(mpris_metadata_values(&view.metadata)?),
    );
    values.insert(
        "LoopStatus",
        Value::from(view.loop_status.as_mpris_str().to_string()),
    );
    values.insert("Shuffle", Value::from(view.shuffle));
    values.insert("Volume", Value::from(view.volume.as_mpris()));
    values.insert("CanGoNext", Value::from(view.capabilities.can_go_next));
    values.insert(
        "CanGoPrevious",
        Value::from(view.capabilities.can_go_previous),
    );
    values.insert("CanPlay", Value::from(view.capabilities.can_play));
    values.insert("CanPause", Value::from(view.capabilities.can_pause));
    values.insert("CanSeek", Value::from(view.capabilities.can_seek));
    Ok(values)
}

pub(super) fn mpris_metadata_values(
    metadata: &DesktopMetadata,
) -> DesktopIntegrationResult<HashMap<String, OwnedValue>> {
    let mut values = HashMap::new();
    if let Some(track_key) = metadata.track_key {
        let path = encode_track_key(track_key)?;
        let object_path = ObjectPath::try_from(path)
            .map_err(|error| DesktopIntegrationError::Backend(error.to_string()))?;
        values.insert("mpris:trackid".to_string(), OwnedValue::from(object_path));
    }
    if let Some(title) = &metadata.title {
        values.insert(
            "xesam:title".to_string(),
            owned_value_from_string(title.clone())?,
        );
    }
    if let Some(duration) = metadata.duration {
        values.insert(
            "mpris:length".to_string(),
            OwnedValue::from(media_duration_to_mpris_microseconds(duration)),
        );
    }
    Ok(values)
}

fn owned_value_from_string(value: String) -> DesktopIntegrationResult<OwnedValue> {
    OwnedValue::try_from(Value::from(value))
        .map_err(|error| DesktopIntegrationError::Backend(error.to_string()))
}
