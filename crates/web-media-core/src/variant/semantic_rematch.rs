//! Semantic-only request и fresh-catalog rematch.

use super::catalog_impl::validate_semantic_scope;
use super::*;

/// Semantic-only rematch request для strong-reopen boundary.
///
/// Request намеренно хранит только refresh-stable identities. Snapshot-local
/// parent identity, extraction generation, exact component identities и
/// component catalog generation в этом типе отсутствуют.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentVariantSemanticSelectionRequest {
    /// Независимо rematch-ит ровно один video и один audio variant.
    VideoAndAudio {
        /// Refresh-stable video identity.
        video: ComponentVariantSemanticIdentity,
        /// Refresh-stable audio identity.
        audio: ComponentVariantSemanticIdentity,
    },
    /// Rematch-ит ровно один video variant.
    VideoOnly {
        /// Refresh-stable video identity.
        video: ComponentVariantSemanticIdentity,
    },
    /// Rematch-ит ровно один audio variant.
    AudioOnly {
        /// Refresh-stable audio identity.
        audio: ComponentVariantSemanticIdentity,
    },
}

impl ComponentVariantCatalog {
    /// Rematch-ит semantic-only request в exact rows этого свежего catalog.
    ///
    /// Оба axis `VideoAndAudio` разрешаются независимо; метод не выбирает
    /// default/fallback и не материализует пары `Video × Audio`.
    pub fn rematch_semantic(
        &self,
        request: ComponentVariantSemanticSelectionRequest,
    ) -> Result<ComponentVariantSelection, ComponentVariantError> {
        match (self, request) {
            (
                Self::VideoAndAudio { .. },
                ComponentVariantSemanticSelectionRequest::VideoAndAudio { video, audio },
            ) => Ok(ComponentVariantSelection::VideoAndAudio {
                video: Box::new(self.find_video_semantic(&video)?.clone()),
                audio: Box::new(self.find_audio_semantic(&audio)?.clone()),
            }),
            (
                Self::VideoOnly { .. },
                ComponentVariantSemanticSelectionRequest::VideoOnly { video },
            ) => Ok(ComponentVariantSelection::VideoOnly {
                video: Box::new(self.find_video_semantic(&video)?.clone()),
            }),
            (
                Self::AudioOnly { .. },
                ComponentVariantSemanticSelectionRequest::AudioOnly { audio },
            ) => Ok(ComponentVariantSelection::AudioOnly {
                audio: Box::new(self.find_audio_semantic(&audio)?.clone()),
            }),
            _ => Err(ComponentVariantError::LayoutMismatch),
        }
    }

    /// Находит свежую exact video row по refresh-stable semantic identity.
    fn find_video_semantic(
        &self,
        requested: &ComponentVariantSemanticIdentity,
    ) -> Result<&VideoComponentVariant, ComponentVariantError> {
        validate_semantic_scope(self.identity(), requested, ComponentKind::Video)?;
        self.required_video_variants()?
            .iter()
            .find(|variant| variant.semantic_identity() == requested)
            .ok_or(ComponentVariantError::MissingSemanticVariant {
                component: ComponentKind::Video,
            })
    }

    /// Находит свежую exact audio row по refresh-stable semantic identity.
    fn find_audio_semantic(
        &self,
        requested: &ComponentVariantSemanticIdentity,
    ) -> Result<&AudioComponentVariant, ComponentVariantError> {
        validate_semantic_scope(self.identity(), requested, ComponentKind::Audio)?;
        self.required_audio_variants()?
            .iter()
            .find(|variant| variant.semantic_identity() == requested)
            .ok_or(ComponentVariantError::MissingSemanticVariant {
                component: ComponentKind::Audio,
            })
    }
}

impl ComponentVariantSelection {
    /// Создаёт semantic-only request для strong reopen через свежий catalog.
    ///
    /// Метод клонирует только refresh-stable component identities и не
    /// переносит exact parent/component identity или generation.
    pub fn semantic_rematch_request(&self) -> ComponentVariantSemanticSelectionRequest {
        match self {
            Self::VideoAndAudio { video, audio } => {
                ComponentVariantSemanticSelectionRequest::VideoAndAudio {
                    video: video.semantic_identity().clone(),
                    audio: audio.semantic_identity().clone(),
                }
            }
            Self::VideoOnly { video } => ComponentVariantSemanticSelectionRequest::VideoOnly {
                video: video.semantic_identity().clone(),
            },
            Self::AudioOnly { audio } => ComponentVariantSemanticSelectionRequest::AudioOnly {
                audio: audio.semantic_identity().clone(),
            },
        }
    }
}
