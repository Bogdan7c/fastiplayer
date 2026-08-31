use std::fmt;
use std::sync::Arc;

use player_core::MediaInstanceId;
use playlist_core::PlaylistItemId;
use web_media_core::ExactSelectionIdentity;

use crate::playlist_runtime::PlaylistRuntimeBinding;
use crate::web_media_stream_model::WebMediaStreamGeneration;

use super::attachment::WebMediaCatalogAttachment;
use super::model::{WebMediaCatalog, WebMediaCatalogSafeError, WebMediaCatalogState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebMediaCatalogScope {
    Item(PlaylistItemId),
    Detached,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct WebMediaCatalogCorrelation {
    pub(crate) scope: WebMediaCatalogScope,
    pub(crate) parent: Option<ExactSelectionIdentity>,
    pub(crate) media_instance: MediaInstanceId,
    pub(crate) binding: PlaylistRuntimeBinding,
    pub(crate) parent_generation: Option<WebMediaStreamGeneration>,
}

impl fmt::Debug for WebMediaCatalogCorrelation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebMediaCatalogCorrelation")
            .field("scope", &self.scope)
            .field("parent", &self.parent.as_ref().map(|_| "<exact-selection>"))
            .field("media_instance", &self.media_instance)
            .field("binding", &self.binding)
            .field("parent_generation", &self.parent_generation)
            .finish()
    }
}

pub(crate) struct WebMediaCatalogCoordinator {
    next_generation: u64,
    active_correlation: Option<WebMediaCatalogCorrelation>,
    visible: WebMediaCatalogState,
}

impl WebMediaCatalogCoordinator {
    pub(crate) const fn new() -> Self {
        Self {
            next_generation: 0,
            active_correlation: None,
            visible: WebMediaCatalogState::Inactive,
        }
    }

    pub(crate) fn ensure(
        &mut self,
        correlation: WebMediaCatalogCorrelation,
        attachment: WebMediaCatalogAttachment,
    ) {
        if attachment.parent() != correlation.parent.as_ref() {
            self.visible = WebMediaCatalogState::Failed {
                parent_generation: correlation.parent_generation,
                error: WebMediaCatalogSafeError::AttachmentMismatch,
            };
            return;
        }
        if self.active_correlation.as_ref() == Some(&correlation) {
            return;
        }

        self.next_generation = self.next_generation.saturating_add(1);
        self.active_correlation = Some(correlation.clone());
        self.visible = WebMediaCatalog::new(
            self.next_generation,
            correlation.parent_generation,
            attachment.choices(),
            attachment.active(),
        )
        .map(Arc::new)
        .map(WebMediaCatalogState::Ready)
        .unwrap_or(WebMediaCatalogState::Failed {
            parent_generation: correlation.parent_generation,
            error: WebMediaCatalogSafeError::InvalidCatalog,
        });
    }

    pub(crate) fn clear(&mut self) {
        self.active_correlation = None;
        self.visible = WebMediaCatalogState::Inactive;
    }

    pub(crate) fn state(&self) -> WebMediaCatalogState {
        self.visible.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use web_media_core::{
        CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
        SourceIdentity,
    };

    use super::*;
    use crate::web_media_catalog::{WebMediaCatalogChoice, WebMediaMode, WebMediaSelectionTarget};

    fn exact_identity(source: u64, generation: u64, key: &str) -> ExactSelectionIdentity {
        let source = SourceIdentity::new(source);
        ExactSelectionIdentity::new(
            CandidateIdentity::new(
                source,
                ExtractionGeneration::new(generation),
                CandidateFormatIdentity::new(key).unwrap(),
            ),
            SemanticIdentity::new(source, key).unwrap(),
        )
        .unwrap()
    }

    fn correlation(
        parent: ExactSelectionIdentity,
        item: u64,
        media_instance: u64,
        generation: u64,
    ) -> WebMediaCatalogCorrelation {
        WebMediaCatalogCorrelation {
            scope: WebMediaCatalogScope::Item(
                PlaylistItemId::from_persistence_value(item).unwrap(),
            ),
            parent: Some(parent),
            media_instance: MediaInstanceId::from_non_zero(
                NonZeroU64::new(media_instance).unwrap(),
            ),
            binding: PlaylistRuntimeBinding::for_test(1, generation),
            parent_generation: Some(WebMediaStreamGeneration::for_test(1, generation)),
        }
    }

    fn attachment(parent: ExactSelectionIdentity, target: u64) -> WebMediaCatalogAttachment {
        let active = WebMediaSelectionTarget::Fixture(target);
        WebMediaCatalogAttachment::new(
            parent,
            vec![WebMediaCatalogChoice {
                mode: WebMediaMode::AudioOnly,
                video: None,
                rank: web_media_playback_plan::OpaqueAlternativeRank::parent(0),
                target: active.clone(),
            }],
            active,
        )
        .unwrap()
    }

    #[test]
    fn ensure_publishes_declared_catalog_immediately_and_replaces_stale_correlation() {
        let mut coordinator = WebMediaCatalogCoordinator::new();
        let first_parent = exact_identity(1, 1, "first");
        coordinator.ensure(
            correlation(first_parent.clone(), 1, 1, 1),
            attachment(first_parent, 1),
        );
        let WebMediaCatalogState::Ready(first) = coordinator.state() else {
            panic!("first declared catalog должен публиковаться синхронно");
        };
        assert_eq!(first.generation(), 1);

        let second_parent = exact_identity(1, 2, "second");
        coordinator.ensure(
            correlation(second_parent.clone(), 1, 2, 2),
            attachment(second_parent, 2),
        );
        let WebMediaCatalogState::Ready(second) = coordinator.state() else {
            panic!("latest declared catalog должен быть Ready в том же вызове");
        };
        assert_eq!(second.generation(), 2);
        assert_eq!(
            second.active_choice().target,
            WebMediaSelectionTarget::Fixture(2)
        );
    }

    #[test]
    fn ensure_rejects_attachment_from_another_parent() {
        let mut coordinator = WebMediaCatalogCoordinator::new();
        let expected_parent = exact_identity(2, 1, "expected");
        let attached_parent = exact_identity(2, 1, "attached");

        coordinator.ensure(
            correlation(expected_parent, 2, 3, 1),
            attachment(attached_parent, 3),
        );

        assert!(matches!(
            coordinator.state(),
            WebMediaCatalogState::Failed {
                error: WebMediaCatalogSafeError::AttachmentMismatch,
                ..
            }
        ));
    }

    #[test]
    fn repeated_same_correlation_keeps_original_catalog_and_generation() {
        let mut coordinator = WebMediaCatalogCoordinator::new();
        let parent = exact_identity(3, 1, "same");
        let correlation = correlation(parent.clone(), 3, 4, 1);
        coordinator.ensure(correlation.clone(), attachment(parent.clone(), 1));

        coordinator.ensure(correlation, attachment(parent, 2));

        let WebMediaCatalogState::Ready(catalog) = coordinator.state() else {
            panic!("same correlation должна сохранять Ready catalog");
        };
        assert_eq!(catalog.generation(), 1);
        assert_eq!(
            catalog.active_choice().target,
            WebMediaSelectionTarget::Fixture(1)
        );
    }

    #[test]
    fn direct_and_native_installed_rows_publish_without_fake_parent_generation() {
        let mut coordinator = WebMediaCatalogCoordinator::new();
        let correlation =
            |media_instance: u64, binding_generation: u64| WebMediaCatalogCorrelation {
                scope: WebMediaCatalogScope::Detached,
                parent: None,
                media_instance: MediaInstanceId::from_non_zero(
                    NonZeroU64::new(media_instance).unwrap(),
                ),
                binding: PlaylistRuntimeBinding::for_test(7, binding_generation),
                parent_generation: None,
            };

        coordinator.ensure(
            correlation(70, 1),
            WebMediaCatalogAttachment::installed_only(),
        );
        let WebMediaCatalogState::Ready(direct) = coordinator.state() else {
            panic!("direct installed-only row must publish synchronously");
        };
        assert_eq!(direct.generation(), 1);
        assert_eq!(direct.parent_generation(), None);
        assert_eq!(
            direct.active_choice().target,
            WebMediaSelectionTarget::InstalledOnly
        );

        coordinator.ensure(
            correlation(71, 2),
            WebMediaCatalogAttachment::installed_only(),
        );
        let WebMediaCatalogState::Ready(native_hls) = coordinator.state() else {
            panic!("native HLS installed-only row must replace stale direct correlation");
        };
        assert_eq!(native_hls.generation(), 2);
        assert_eq!(native_hls.parent_generation(), None);
    }
}
