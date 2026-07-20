//! Чистые app-owned adapters между neutral discovery records и playlist domain.

use std::ops::Bound::{Excluded, Unbounded};

use media_core::MediaDuration;
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, LocalSourceFingerprint, PlaylistItemDraft,
    PlaylistMediaKind, RepeatMode, StableInsertionAnchor,
};
use playlist_discovery::{
    AdmissionDirection, DirectoryManifest, DiscoveryRecord, DiscoveryRecordKey, LocalMediaKind,
    ManifestCandidateKey, SiblingFilter,
};

use super::ActiveDiscoveryScope;

pub(super) fn sibling_filter(
    filter: rustiplayer_config::PlaylistSiblingMediaFilter,
) -> SiblingFilter {
    match filter {
        rustiplayer_config::PlaylistSiblingMediaFilter::VideoOnly => SiblingFilter::VideoOnly,
        rustiplayer_config::PlaylistSiblingMediaFilter::AllMedia => SiblingFilter::AllMedia,
        rustiplayer_config::PlaylistSiblingMediaFilter::AudioOnly => SiblingFilter::AudioOnly,
        rustiplayer_config::PlaylistSiblingMediaFilter::SameAsOpened => SiblingFilter::SameAsOpened,
    }
}

pub(super) fn batch_matches(
    active: &ActiveDiscoveryScope,
    batch: &playlist_discovery::AdmittedBatch,
) -> bool {
    batch.job_id() == active.job.id()
        && batch.request_revision() == active.request_revision
        && batch.policy_revision() == Some(active.policy_revision)
        && matches!(
            batch.direction(),
            AdmissionDirection::Before | AdmissionDirection::After
        )
}

pub(super) fn insertion_anchor(
    active: &ActiveDiscoveryScope,
    records: &[DiscoveryRecord],
) -> Option<StableInsertionAnchor> {
    let maximum_key = records
        .iter()
        .filter_map(|record| match record.key() {
            DiscoveryRecordKey::Manifest(key) => Some(key),
            DiscoveryRecordKey::Batch(_) => None,
        })
        .max()?;
    let next_committed = active
        .committed_ids_by_key
        .range((Excluded(maximum_key), Unbounded))
        .next()
        .map(|(_, item_id)| *item_id);
    Some(match next_committed {
        Some(item_id) => {
            StableInsertionAnchor::before(playlist_core::PlaylistEntryId::Single(item_id))
        }
        None => StableInsertionAnchor::at_end(),
    })
}

pub(crate) fn target_draft_from_prepared(
    prepared: &crate::media_open::PreparedLocalOpenResult,
) -> Result<PlaylistItemDraft, playlist_core::CachedMetadataError> {
    Ok(PlaylistItemDraft::local(
        LocalLocator::Native(prepared.source_path.clone()),
        Some(LocalSourceFingerprint::new(
            prepared.fingerprint.file_size_bytes(),
            prepared.fingerprint.modified_at(),
        )),
        cached_metadata(
            &prepared.safe_label.to_string(),
            prepared.media_kind,
            prepared.duration.map(MediaDuration::from_duration),
            &prepared.metadata,
        )?,
    ))
}

pub(super) fn draft_from_record(
    record: &DiscoveryRecord,
) -> Result<PlaylistItemDraft, playlist_core::CachedMetadataError> {
    let media = record.media();
    let metadata = cached_metadata(
        media.display_filename(),
        media.media_kind(),
        media.duration(),
        media.metadata(),
    )?;
    Ok(PlaylistItemDraft::local(
        LocalLocator::Native(record.original_locator().to_path_buf()),
        Some(LocalSourceFingerprint::new(
            media.fingerprint().file_size_bytes(),
            media.fingerprint().modified_at(),
        )),
        metadata,
    ))
}

pub(crate) fn cached_metadata(
    fallback_name: &str,
    kind: LocalMediaKind,
    duration: Option<MediaDuration>,
    tags: &media_core::MediaTagMetadata,
) -> Result<CachedPlaylistMetadata, playlist_core::CachedMetadataError> {
    CachedPlaylistMetadata::new(fallback_name, playlist_media_kind(kind))
        .with_duration(duration)
        .with_title(tags.title.clone())
        .with_artists(tags.artists.clone())
        .map(|metadata| {
            metadata.with_album(tags.album.clone()).with_sequence(
                tags.disc_number,
                tags.track_number,
                tags.tv_season_number,
                tags.tv_episode_number,
            )
        })
}

const fn playlist_media_kind(kind: LocalMediaKind) -> PlaylistMediaKind {
    match kind {
        LocalMediaKind::AudioOnly => PlaylistMediaKind::Audio,
        LocalMediaKind::VideoContaining => PlaylistMediaKind::Video,
    }
}

pub(super) fn manifest_priority_hint(
    manifest: &DirectoryManifest,
    target_key: ManifestCandidateKey,
    controller: &super::super::controller::PlaylistController,
) -> Vec<ManifestCandidateKey> {
    let mut before = manifest
        .records()
        .iter()
        .map(|record| record.candidate_key())
        .filter(|key| *key < target_key)
        .collect::<Vec<_>>();
    let after = manifest
        .records()
        .iter()
        .map(|record| record.candidate_key())
        .filter(|key| *key > target_key)
        .collect::<Vec<_>>();
    before.reverse();
    if controller.queue().shuffle_enabled() {
        let mut interleaved = Vec::with_capacity(before.len() + after.len());
        let mut before = before.into_iter();
        let mut after = after.into_iter();
        loop {
            match (after.next(), before.next()) {
                (None, None) => break,
                (after_key, before_key) => {
                    interleaved.extend(after_key);
                    interleaved.extend(before_key);
                }
            }
        }
        return interleaved;
    }
    let mut ordered = after;
    if matches!(controller.repeat_mode(), RepeatMode::RepeatQueue) {
        ordered.extend(before);
    }
    ordered
}
