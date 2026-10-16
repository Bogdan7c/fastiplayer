// Fastiplayer additive patch: публикация `TrackEntry/Name` как per-track metadata tag.
//
// Symphonia 0.6 `Track` не имеет поля названия, поэтому патч передаёт его так же, как
// isomp4-патч передаёт display orientation: ревизия метаданных с per-track raw tag.
// `track_id` тега равен номеру дорожки (= `Track::id`).

use symphonia_core::meta::{MetadataBuilder, MetadataRevision, PerTrackMetadataBuilder, Tag};

use crate::FASTIPLAYER_TRACK_NAME_TAG;
use crate::segment::MKV_METADATA_INFO;

/// Строит ревизию с названиями дорожек; `None`, если названий нет ни у одной дорожки.
pub(crate) fn track_names_revision(
    track_names: &[(u64, String)],
) -> Option<MetadataRevision> {
    if track_names.is_empty() {
        return None;
    }

    let mut builder = MetadataBuilder::new(MKV_METADATA_INFO);
    for (track_number, name) in track_names {
        let mut track_builder = PerTrackMetadataBuilder::new(*track_number);
        track_builder.add_tag(Tag::new_from_parts(FASTIPLAYER_TRACK_NAME_TAG, name.as_str(), None));
        builder.add_track(track_builder.build());
    }
    Some(builder.build())
}
