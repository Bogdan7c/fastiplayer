//! Typed app/controller actions для URL append и generalized confirmation apply.

use playlist_core::{CachedPlaylistMetadata, PlaylistItemDraft, PlaylistMediaKind};
use playlist_core::{LocalSourceFingerprint, PlaylistItemId, PlaylistMetadataPatch};

use super::controller::ControllerCappedAppendOutcome;
use super::replacement_confirmation::{PendingConfirmationTarget, PlaylistConfirmationResolution};
use super::{AdmittedQueueReplacementIntent, PlaylistRuntime};
use crate::media_open::SafeMediaLabel;
use crate::url_service_adapter::{StartupUrlClassification, classify_startup_url};

/// Pure URL append validation никогда не содержит исходную secret-bearing строку.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UrlAppendValidationError {
    NotUrl,
    Unsupported { safe_error: String },
    LocatorMapping,
    MetadataMapping,
    RuntimeShuttingDown,
    LoadDecisionPending,
    ConfirmationIdentityExhausted,
    CommitRejected,
}

/// URL append либо committed сразу, либо ожидает exact D15 decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UrlAppendActionOutcome {
    Appended { item_count: usize },
    NoCapacity,
    DeferredUntilStartupInstallResolution,
    AwaitingSensitivePersistenceDecision,
}

/// Generalized Confirm/Cancel result сохраняет replacement и append paths раздельно.
#[derive(Debug)]
pub(crate) enum PlaylistConfirmationApplyOutcome {
    QueueReplacementConfirmed(AdmittedQueueReplacementIntent),
    UrlAppended { item_count: usize },
    UrlNoCapacity,
    DeferredUntilStartupInstallResolution,
    Cancelled,
    Stale,
    CommitRejected,
}

/// Post-Installed cache update не смешивает stale/no-change/error outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstalledMetadataCacheOutcome {
    Applied,
    NoChangeOrStale,
    DescriptorHasNoMetadata,
    ItemNotCommitted,
    Rejected,
}

impl PlaylistRuntime {
    /// Play/open success переносит descriptor cache только после exact Installed caller barrier.
    pub(crate) fn record_successful_item_open_metadata(
        &mut self,
        item_id: PlaylistItemId,
        descriptor: &crate::media_open::PreparedMediaDescriptor,
    ) -> InstalledMetadataCacheOutcome {
        let Some(cache) = descriptor.playlist_cache_update() else {
            return InstalledMetadataCacheOutcome::DescriptorHasNoMetadata;
        };
        let Some(controller) = self.controller.as_mut() else {
            return InstalledMetadataCacheOutcome::ItemNotCommitted;
        };
        let Some(item) = controller.queue().item(item_id) else {
            return InstalledMetadataCacheOutcome::ItemNotCommitted;
        };
        let locator = item.locator().clone();
        let expected_fingerprint = item.local_fingerprint();
        let fallback = item.cached_metadata().fallback_display_name().to_owned();
        let Ok(metadata) = super::discovery::cached_metadata(
            &fallback,
            cache.media_kind,
            cache.duration.map(media_core::MediaDuration::from_duration),
            &cache.metadata,
        ) else {
            return InstalledMetadataCacheOutcome::Rejected;
        };
        let patch = match cache.fingerprint {
            Some(fingerprint) => PlaylistMetadataPatch::refreshed_local(
                item_id,
                locator,
                expected_fingerprint,
                LocalSourceFingerprint::new(
                    fingerprint.file_size_bytes(),
                    fingerprint.modified_at(),
                ),
                metadata,
            ),
            None => PlaylistMetadataPatch::new(item_id, locator, expected_fingerprint, metadata),
        };
        let dirty_before = controller.dirty_revision();
        let Ok(outcome) = controller.apply_metadata_patches(vec![patch]) else {
            return InstalledMetadataCacheOutcome::Rejected;
        };
        if outcome.domain.changed_metadata() {
            self.publish_controller_mutation_if_dirty(dirty_before);
            InstalledMetadataCacheOutcome::Applied
        } else {
            InstalledMetadataCacheOutcome::NoChangeOrStale
        }
    }

    /// Pure classifier/normalizer + D15 gate; network/open здесь запрещены.
    pub(crate) fn append_playlist_url(
        &mut self,
        input: &str,
    ) -> Result<UrlAppendActionOutcome, UrlAppendValidationError> {
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(UrlAppendValidationError::RuntimeShuttingDown);
        }
        self.supersede_startup_media_apply();
        let locator = match classify_startup_url(input) {
            StartupUrlClassification::NotUrl => return Err(UrlAppendValidationError::NotUrl),
            StartupUrlClassification::Unsupported { safe_error } => {
                return Err(UrlAppendValidationError::Unsupported { safe_error });
            }
            StartupUrlClassification::Supported(locator) => locator,
        };
        let requires_ack = locator.requires_sensitive_persistence_acknowledgement();
        let safe_label = SafeMediaLabel::from_service_safe_label(locator.safe_label());
        let cached_metadata =
            CachedPlaylistMetadata::new(locator.safe_label(), PlaylistMediaKind::Unknown);
        let playlist_locator = locator
            .to_playlist_locator()
            .map_err(|_| UrlAppendValidationError::LocatorMapping)?;
        let draft = PlaylistItemDraft::url(playlist_locator, cached_metadata);
        // D25: любое Add снимает active sibling discovery, сохраняя уже committed batches.
        self.discovery.cancel_sibling_for_add();

        if requires_ack {
            self.replacement_confirmation
                .replace_with_sensitive_url_append(safe_label, draft)
                .map_err(|_| UrlAppendValidationError::ConfirmationIdentityExhausted)?;
            return Ok(UrlAppendActionOutcome::AwaitingSensitivePersistenceDecision);
        }
        self.replacement_confirmation.cancel();
        self.commit_url_append(draft)
    }

    /// Matching generalized response consumes secret-bearing intent exactly once.
    pub(crate) fn respond_to_playlist_confirmation(
        &mut self,
        action: super::PlaylistConfirmationAction,
    ) -> PlaylistConfirmationApplyOutcome {
        match self.replacement_confirmation.respond_generalized(action) {
            PlaylistConfirmationResolution::Cancelled => {
                PlaylistConfirmationApplyOutcome::Cancelled
            }
            PlaylistConfirmationResolution::Stale => PlaylistConfirmationApplyOutcome::Stale,
            PlaylistConfirmationResolution::Confirmed(
                PendingConfirmationTarget::QueueReplacement(target),
            ) => PlaylistConfirmationApplyOutcome::QueueReplacementConfirmed(target.admit()),
            PlaylistConfirmationResolution::Confirmed(
                PendingConfirmationTarget::SensitiveUrlAppend(draft),
            ) => match self.commit_url_append(*draft) {
                Ok(UrlAppendActionOutcome::Appended { item_count }) => {
                    PlaylistConfirmationApplyOutcome::UrlAppended { item_count }
                }
                Ok(UrlAppendActionOutcome::NoCapacity) => {
                    PlaylistConfirmationApplyOutcome::UrlNoCapacity
                }
                Ok(UrlAppendActionOutcome::DeferredUntilStartupInstallResolution) => {
                    PlaylistConfirmationApplyOutcome::DeferredUntilStartupInstallResolution
                }
                Ok(UrlAppendActionOutcome::AwaitingSensitivePersistenceDecision) => {
                    unreachable!("confirmed draft never re-enters acknowledgement")
                }
                Err(_) => PlaylistConfirmationApplyOutcome::CommitRejected,
            },
        }
    }

    fn commit_url_append(
        &mut self,
        draft: PlaylistItemDraft,
    ) -> Result<UrlAppendActionOutcome, UrlAppendValidationError> {
        if self.controller.as_ref().is_none() {
            self.record_startup_prepared_add(vec![draft])
                .map_err(|_| UrlAppendValidationError::CommitRejected)?;
            return Ok(UrlAppendActionOutcome::DeferredUntilStartupInstallResolution);
        }
        let startup_install_linearizing = self.startup_action_retention_is_active()
            && self.controller.as_ref().is_some_and(|controller| {
                controller.install_phase().is_some_and(|phase| {
                    phase != super::controller::ControllerInstallPhase::AwaitingReady
                })
            });
        if startup_install_linearizing {
            self.retain_startup_prepared_add(vec![draft])
                .map_err(|_| UrlAppendValidationError::CommitRejected)?;
            return Ok(UrlAppendActionOutcome::DeferredUntilStartupInstallResolution);
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or(UrlAppendValidationError::LoadDecisionPending)?;
        let dirty_before = controller.dirty_revision();
        let ControllerCappedAppendOutcome { item_ids, .. } = controller
            .append_capped_tail(vec![draft])
            .map_err(|_| UrlAppendValidationError::CommitRejected)?;
        if item_ids.is_empty() {
            return Ok(UrlAppendActionOutcome::NoCapacity);
        }
        self.publish_controller_mutation_if_dirty(dirty_before);
        Ok(UrlAppendActionOutcome::Appended {
            item_count: item_ids.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use super::*;
    use crate::app_wake::{AppWakeOwner, AppWakePort};
    use crate::playlist_runtime::controller::ControllerAppendOutcome;
    use crate::playlist_runtime::{
        PlaylistConfirmationAction, QueueReplacementConfirmationDecision,
    };

    fn runtime() -> PlaylistRuntime {
        let mut runtime =
            PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
        runtime.resolve_missing_state_for_test();
        runtime
    }

    #[test]
    fn pure_url_append_allows_duplicates_without_network_or_playback_side_effects() {
        let mut runtime = runtime();
        let first = runtime
            .append_playlist_url("https://media.example.test/video2.mp4")
            .expect("pure direct classification");
        let second = runtime
            .append_playlist_url("https://media.example.test/video2.mp4")
            .expect("duplicate remains valid");
        assert_eq!(first, UrlAppendActionOutcome::Appended { item_count: 1 });
        assert_eq!(second, UrlAppendActionOutcome::Appended { item_count: 1 });
        assert_eq!(runtime.controller.queue().len(), 2);
        assert!(runtime.controller.active_media().is_none());
    }

    #[test]
    fn sensitive_url_requires_exact_process_lifetime_decision_and_never_uses_old_ui_accessor() {
        let mut runtime = runtime();
        let dirty_before = runtime.controller.dirty_revision();
        let raw = "https://user:password@media.example.test/video.mp4?token=secret";
        assert_eq!(
            runtime
                .append_playlist_url(raw)
                .expect("pure classification"),
            UrlAppendActionOutcome::AwaitingSensitivePersistenceDecision
        );
        assert_eq!(runtime.controller.queue().len(), 0);
        assert_eq!(runtime.controller.dirty_revision(), dirty_before);
        assert!(runtime.pending_queue_replacement_confirmation().is_none());
        let model = runtime
            .pending_playlist_confirmation()
            .expect("sensitive decision is process-lifetime");
        assert!(!model.reasons().queue_replacement());
        assert!(model.reasons().sensitive_url_persistence());
        assert!(!format!("{model:?}").contains("token=secret"));
        runtime.append_playlist_url(raw).expect("new exact intent");
        assert!(matches!(
            runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
                intent_id: model.intent_id(),
                decision: QueueReplacementConfirmationDecision::Confirm,
            }),
            PlaylistConfirmationApplyOutcome::Stale
        ));
        let current = runtime
            .pending_playlist_confirmation()
            .expect("current model");
        assert!(matches!(
            runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
                intent_id: current.intent_id(),
                decision: QueueReplacementConfirmationDecision::Confirm,
            }),
            PlaylistConfirmationApplyOutcome::UrlAppended { item_count: 1 }
        ));
        assert_eq!(runtime.controller.queue().len(), 1);
        assert!(runtime.pending_playlist_confirmation().is_none());
    }

    #[test]
    fn sensitive_cancel_and_supersede_are_typed_noops_and_errors_are_redacted() {
        let mut runtime = runtime();
        let raw = "https://user:password@media.example.test/video.mp4?token=secret";
        runtime.append_playlist_url(raw).expect("pending decision");
        let model = runtime.pending_playlist_confirmation().expect("model");
        assert!(matches!(
            runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
                intent_id: model.intent_id(),
                decision: QueueReplacementConfirmationDecision::Cancel,
            }),
            PlaylistConfirmationApplyOutcome::Cancelled
        ));
        assert_eq!(runtime.controller.queue().len(), 0);

        runtime
            .append_playlist_url(raw)
            .expect("second pending decision");
        let stale = runtime.pending_playlist_confirmation().expect("model");
        runtime
            .append_playlist_url("https://media.example.test/other.mp4")
            .expect("new action commits and supersedes pending slot");
        assert!(matches!(
            runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
                intent_id: stale.intent_id(),
                decision: QueueReplacementConfirmationDecision::Confirm,
            }),
            PlaylistConfirmationApplyOutcome::Stale
        ));
        let malformed = "https://user:password@[invalid]/video.mp4?token=secret";
        let error = runtime
            .append_playlist_url(malformed)
            .expect_err("invalid URL rejected");
        let formatted = format!("{error:?}");
        for secret in ["password", "token=secret"] {
            assert!(!formatted.contains(secret));
        }
    }

    #[test]
    fn exact_installed_descriptor_updates_local_cache_after_mismatch_without_queue_replacement() {
        let mut runtime = runtime();
        let initial = LocalSourceFingerprint::new(10, SystemTime::UNIX_EPOCH);
        let draft = PlaylistItemDraft::local(
            playlist_core::LocalLocator::Native(PathBuf::from("movie.mkv")),
            Some(initial),
            CachedPlaylistMetadata::new("movie.mkv", PlaylistMediaKind::Video),
        );
        let ControllerAppendOutcome::Added { item_ids, .. } = runtime
            .controller
            .append(vec![draft])
            .expect("fixture append")
        else {
            panic!("one fixture item must be added");
        };
        let actual = playlist_discovery::LocalMediaFingerprint::new(
            20,
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1),
        );
        let descriptor = crate::media_open::PreparedMediaDescriptor::Local {
            media_kind: playlist_discovery::LocalMediaKind::VideoContaining,
            tracks: Vec::new(),
            duration: None,
            metadata: media_core::MediaTagMetadata {
                title: Some("Actual title".to_owned()),
                artists: Vec::new(),
                album: None,
                disc_number: None,
                track_number: None,
                tv_season_number: None,
                tv_episode_number: None,
            },
            fingerprint: actual,
            source: crate::media_open::ActiveMediaSource::LocalFile(PathBuf::from("movie.mkv")),
            safe_label: crate::media_open::SafeMediaLabel::from_service_safe_label("movie.mkv"),
            fingerprint_validation: crate::media_open::LocalFingerprintValidation::CacheMismatch,
        };
        assert_eq!(
            runtime.record_successful_item_open_metadata(item_ids[0], &descriptor),
            InstalledMetadataCacheOutcome::Applied
        );
        let item = runtime
            .controller
            .queue()
            .item(item_ids[0])
            .expect("row retained");
        assert_eq!(
            item.local_fingerprint()
                .map(|value| value.file_size_bytes()),
            Some(20)
        );
        assert_eq!(item.cached_metadata().title(), Some("Actual title"));
        assert_eq!(runtime.controller.queue().len(), 1);
    }
}
