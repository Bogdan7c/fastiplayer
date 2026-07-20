//! Typed app/controller actions для URL append и generalized confirmation apply.

use playlist_core::{CachedPlaylistMetadata, PlaylistItemDraft, PlaylistMediaKind};
use playlist_core::{LocalSourceFingerprint, PlaylistItemId, PlaylistMetadataPatch};

use super::controller::ControllerCappedAppendOutcome;
use super::replacement_confirmation::{PendingConfirmationTarget, PlaylistConfirmationResolution};
use super::{AdmittedQueueReplacementIntent, PlaylistRuntime};
use crate::media_open::SafeMediaLabel;
use crate::url_service_adapter::{
    PlaylistUrlMetadataSource, StartupUrlClassification, classify_startup_url,
};

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
    Import(super::import_transaction::PlaylistImportContinueOutcome),
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

struct CommittedUrlAppend {
    outcome: UrlAppendActionOutcome,
    item_ids: Vec<PlaylistItemId>,
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

    /// Синхронно выполняет только classifier/commit; enrichment уходит в bounded worker.
    pub(crate) fn append_playlist_url(
        &mut self,
        input: &str,
        yt_dlp_config: &rustiplayer_config::YtDlpConfig,
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
        let metadata_source = locator.playlist_metadata_source();
        let safe_label = SafeMediaLabel::from_service_safe_label(locator.safe_label());
        let cached_metadata =
            CachedPlaylistMetadata::new(locator.safe_label(), PlaylistMediaKind::Unknown);
        let playlist_locator = locator
            .to_playlist_locator()
            .map_err(|_| UrlAppendValidationError::LocatorMapping)?;
        let draft = PlaylistItemDraft::url(playlist_locator, cached_metadata);
        // D25: любое Add снимает active sibling discovery, сохраняя уже committed batches.
        self.discovery.cancel_sibling_for_add();
        self.import_transaction.cancel();

        if requires_ack {
            self.replacement_confirmation
                .replace_with_sensitive_url_append(safe_label, draft)
                .map_err(|_| UrlAppendValidationError::ConfirmationIdentityExhausted)?;
            return Ok(UrlAppendActionOutcome::AwaitingSensitivePersistenceDecision);
        }
        self.replacement_confirmation.cancel();
        let committed = self.commit_url_append(draft)?;
        if let Some(metadata_source) = metadata_source {
            self.request_committed_url_metadata(
                &committed.item_ids,
                metadata_source,
                yt_dlp_config,
            );
        }
        Ok(committed.outcome)
    }

    /// Matching generalized response consumes secret-bearing intent exactly once.
    pub(crate) fn respond_to_playlist_confirmation(
        &mut self,
        action: super::PlaylistConfirmationAction,
    ) -> PlaylistConfirmationApplyOutcome {
        match self.replacement_confirmation.respond_generalized(action) {
            PlaylistConfirmationResolution::Cancelled => {
                self.import_transaction.cancel();
                PlaylistConfirmationApplyOutcome::Cancelled
            }
            PlaylistConfirmationResolution::Stale => PlaylistConfirmationApplyOutcome::Stale,
            PlaylistConfirmationResolution::Confirmed(
                PendingConfirmationTarget::QueueReplacement(target),
            ) => PlaylistConfirmationApplyOutcome::QueueReplacementConfirmed(target.admit()),
            PlaylistConfirmationResolution::Confirmed(
                PendingConfirmationTarget::SensitiveUrlAppend(draft),
            ) => match self
                .commit_url_append(*draft)
                .map(|committed| committed.outcome)
            {
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
            PlaylistConfirmationResolution::Confirmed(PendingConfirmationTarget::Import(
                continuation,
            )) => PlaylistConfirmationApplyOutcome::Import(
                self.confirm_staged_playlist_import(continuation),
            ),
        }
    }

    fn commit_url_append(
        &mut self,
        draft: PlaylistItemDraft,
    ) -> Result<CommittedUrlAppend, UrlAppendValidationError> {
        if self.controller.as_ref().is_none() {
            self.record_startup_prepared_add(vec![draft])
                .map_err(|_| UrlAppendValidationError::CommitRejected)?;
            return Ok(CommittedUrlAppend {
                outcome: UrlAppendActionOutcome::DeferredUntilStartupInstallResolution,
                item_ids: Vec::new(),
            });
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
            return Ok(CommittedUrlAppend {
                outcome: UrlAppendActionOutcome::DeferredUntilStartupInstallResolution,
                item_ids: Vec::new(),
            });
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
            return Ok(CommittedUrlAppend {
                outcome: UrlAppendActionOutcome::NoCapacity,
                item_ids,
            });
        }
        self.publish_controller_mutation_if_dirty(dirty_before);
        Ok(CommittedUrlAppend {
            outcome: UrlAppendActionOutcome::Appended {
                item_count: item_ids.len(),
            },
            item_ids,
        })
    }

    /// После exact queue commit связывает service result только с выданными Item IDs.
    fn request_committed_url_metadata(
        &mut self,
        item_ids: &[PlaylistItemId],
        metadata_source: PlaylistUrlMetadataSource,
        yt_dlp_config: &rustiplayer_config::YtDlpConfig,
    ) {
        let PlaylistUrlMetadataSource::YtDlp(yt_dlp_locator) = metadata_source;
        let Some(controller) = self.controller.as_ref() else {
            return;
        };
        let demands = item_ids
            .iter()
            .filter_map(|item_id| {
                let item = controller.queue().item(*item_id)?;
                Some(super::discovery::YtDlpMetadataDemand::new(
                    *item_id,
                    item.locator().clone(),
                    yt_dlp_locator.clone(),
                    yt_dlp_config.clone(),
                ))
            })
            .collect();
        let _request_outcome = self.discovery.request_yt_dlp_metadata(demands);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::app_wake::{AppWakeOwner, AppWakePort};
    use crate::playlist_runtime::controller::ControllerAppendOutcome;
    use crate::playlist_runtime::discovery::{YtDlpMetadataResolver, YtDlpMetadataTaskOutcome};
    use crate::playlist_runtime::{
        PlaylistConfirmationAction, QueueReplacementConfirmationDecision,
    };

    fn runtime() -> PlaylistRuntime {
        let mut runtime =
            PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
        runtime.resolve_missing_state_for_test();
        runtime
    }

    struct ImmediateYtDlpMetadataResolver;

    impl YtDlpMetadataResolver for ImmediateYtDlpMetadataResolver {
        fn resolve(
            &self,
            _locator: &service_ytdlp::YtDlpMediaLocator,
            _yt_dlp_config: &rustiplayer_config::YtDlpConfig,
            cancellation: &bounded_work_executor::CancellationToken,
        ) -> YtDlpMetadataTaskOutcome {
            if cancellation.is_cancelled() {
                YtDlpMetadataTaskOutcome::Cancelled
            } else {
                YtDlpMetadataTaskOutcome::Resolved {
                    title: Some("Настоящее название из yt-dlp".to_string()),
                    duration: Some(Duration::from_secs(77)),
                }
            }
        }
    }

    #[test]
    fn pure_url_append_allows_duplicates_without_network_or_playback_side_effects() {
        let mut runtime = runtime();
        let first = runtime
            .append_playlist_url(
                "https://media.example.test/video2.mp4",
                &rustiplayer_config::YtDlpConfig::default(),
            )
            .expect("pure direct classification");
        let second = runtime
            .append_playlist_url(
                "https://media.example.test/video2.mp4",
                &rustiplayer_config::YtDlpConfig::default(),
            )
            .expect("duplicate remains valid");
        assert_eq!(first, UrlAppendActionOutcome::Appended { item_count: 1 });
        assert_eq!(second, UrlAppendActionOutcome::Appended { item_count: 1 });
        assert_eq!(runtime.controller.queue().top_level_entry_count(), 2);
        assert!(runtime.controller.active_media().is_none());
    }

    #[test]
    fn yt_dlp_url_append_enriches_committed_row_without_playback() {
        let mut runtime = runtime();
        runtime
            .discovery
            .replace_yt_dlp_metadata_resolver_for_test(Arc::new(ImmediateYtDlpMetadataResolver));
        let yt_dlp_config = rustiplayer_config::YtDlpConfig {
            enabled: true,
            ..rustiplayer_config::YtDlpConfig::default()
        };

        assert_eq!(
            runtime
                .append_playlist_url(
                    "https://media.example.test/watch/title-test?token=exact",
                    &yt_dlp_config,
                )
                .expect("YtDlp URL append"),
            UrlAppendActionOutcome::Appended { item_count: 1 }
        );
        assert!(runtime.controller.active_media().is_none());

        for _ in 0..200 {
            let _visible_change = runtime.drain_playlist_discovery();
            if runtime
                .controller
                .queue()
                .iter_playable_items()
                .next()
                .expect("append должен оставить одну playable строку")
                .cached_metadata()
                .title()
                .is_some()
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }

        let item = runtime
            .controller
            .queue()
            .iter_playable_items()
            .next()
            .expect("append должен оставить одну playable строку");
        assert_eq!(
            item.cached_metadata().title(),
            Some("Настоящее название из yt-dlp")
        );
        assert_eq!(
            item.cached_metadata().duration(),
            Some(media_core::MediaDuration::from_duration(
                Duration::from_secs(77)
            ))
        );
        assert!(runtime.controller.active_media().is_none());
    }

    #[test]
    fn sensitive_url_requires_exact_process_lifetime_decision_and_never_uses_old_ui_accessor() {
        let mut runtime = runtime();
        let dirty_before = runtime.controller.dirty_revision();
        let raw = "https://user:password@media.example.test/video.mp4?token=secret";
        assert_eq!(
            runtime
                .append_playlist_url(raw, &rustiplayer_config::YtDlpConfig::default())
                .expect("pure classification"),
            UrlAppendActionOutcome::AwaitingSensitivePersistenceDecision
        );
        assert_eq!(runtime.controller.queue().top_level_entry_count(), 0);
        assert_eq!(runtime.controller.dirty_revision(), dirty_before);
        assert!(runtime.pending_queue_replacement_confirmation().is_none());
        let model = runtime
            .pending_playlist_confirmation()
            .expect("sensitive decision is process-lifetime");
        assert!(!model.reasons().queue_replacement());
        assert!(model.reasons().sensitive_url_persistence());
        assert!(!format!("{model:?}").contains("token=secret"));
        runtime
            .append_playlist_url(raw, &rustiplayer_config::YtDlpConfig::default())
            .expect("new exact intent");
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
        assert_eq!(runtime.controller.queue().top_level_entry_count(), 1);
        assert!(runtime.pending_playlist_confirmation().is_none());
    }

    #[test]
    fn sensitive_cancel_and_supersede_are_typed_noops_and_errors_are_redacted() {
        let mut runtime = runtime();
        let raw = "https://user:password@media.example.test/video.mp4?token=secret";
        runtime
            .append_playlist_url(raw, &rustiplayer_config::YtDlpConfig::default())
            .expect("pending decision");
        let model = runtime.pending_playlist_confirmation().expect("model");
        assert!(matches!(
            runtime.respond_to_playlist_confirmation(PlaylistConfirmationAction {
                intent_id: model.intent_id(),
                decision: QueueReplacementConfirmationDecision::Cancel,
            }),
            PlaylistConfirmationApplyOutcome::Cancelled
        ));
        assert_eq!(runtime.controller.queue().top_level_entry_count(), 0);

        runtime
            .append_playlist_url(raw, &rustiplayer_config::YtDlpConfig::default())
            .expect("second pending decision");
        let stale = runtime.pending_playlist_confirmation().expect("model");
        runtime
            .append_playlist_url(
                "https://media.example.test/other.mp4",
                &rustiplayer_config::YtDlpConfig::default(),
            )
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
            .append_playlist_url(malformed, &rustiplayer_config::YtDlpConfig::default())
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
        assert_eq!(runtime.controller.queue().top_level_entry_count(), 1);
    }
}
