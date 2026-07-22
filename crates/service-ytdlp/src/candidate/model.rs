use std::fmt;

use web_media_core::{
    CandidateDescriptor, CandidateIdentity, ExtractionGeneration, SemanticIdentity, SourceIdentity,
    StaticCompatibilityRejection,
};

use crate::metadata::YtDlpPlaylistMetadata;

use super::request_material::{
    YtDlpRequestMaterial, YtDlpRequestMaterialSummary, YtDlpRequestMaterialViolation,
};

/// Роль request component-а без позиционной семантики.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpCandidateComponentRole {
    /// Один resource содержит video и audio.
    Muxed,
    /// Video-only resource.
    Video,
    /// Audio-only resource.
    Audio,
}

/// Safe summary одного component request-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YtDlpCandidateComponentRequestSummary {
    /// Semantic role component-а.
    pub role: YtDlpCandidateComponentRole,
    /// Non-secret material shape.
    pub material: YtDlpRequestMaterialSummary,
}

/// Service-owned component request с explicit role.
#[derive(Clone, PartialEq)]
pub(super) struct YtDlpCandidateComponentRequest {
    /// Semantic role component-а.
    pub(super) role: YtDlpCandidateComponentRole,
    /// Transient request material.
    pub(super) material: YtDlpRequestMaterial,
}

/// Источник normalized candidate внутри snapshot-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpCandidateOrigin {
    /// Строка public `formats[]` inventory.
    Inventory {
        /// Нулевой ordinal сохраняет видимость duplicate/rejected rows.
        ordinal: usize,
    },
    /// Корневой selected result.
    Selected {
        /// Один resource либо validated compound merge.
        shape: YtDlpSelectedCandidateShape,
    },
}

/// Shape selected result без чтения `requested_formats` как inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpSelectedCandidateShape {
    /// Корневой selected format является единственным component-ом.
    Single,
    /// Ровно одна video-only и одна audio-only requested component.
    Compound,
}

/// Typed candidate-level rejection, которая не удаляет строку из inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YtDlpCandidateNormalizationRejection {
    /// Neutral static profile rejection.
    Static(StaticCompatibilityRejection),
    /// Snapshot-local `format_id` отсутствует или не проходит S00 bound.
    InvalidFormatIdentity,
    /// Повторный `format_id` не может создать вторую exact identity.
    DuplicateFormatIdentity,
    /// Format hints не описывают ровно muxed/video-only/audio-only shape.
    InvalidStreamLayout,
    /// Request material требует неподдерживаемую или несериализуемую семантику.
    RequestMaterial(YtDlpRequestMaterialViolation),
    /// `requested_formats` не является exact video-only + audio-only merge.
    InvalidCompoundComponents,
}

/// Visible rejected row с optional exact identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YtDlpRejectedCandidate {
    /// Положение/роль исходной строки.
    origin: YtDlpCandidateOrigin,
    /// Exact identity доступна, если `format_id` прошёл bounds; duplicate row
    /// сохраняет её для correlation, но остаётся неселектируемым rejection-ом.
    identity: Option<CandidateIdentity>,
    /// Safe typed причина.
    reason: YtDlpCandidateNormalizationRejection,
}

impl YtDlpRejectedCandidate {
    /// Возвращает исходную роль строки.
    pub const fn origin(&self) -> YtDlpCandidateOrigin {
        self.origin
    }

    /// Возвращает exact snapshot identity, если её удалось построить.
    pub const fn identity(&self) -> Option<&CandidateIdentity> {
        self.identity.as_ref()
    }

    /// Возвращает typed rejection.
    pub const fn reason(&self) -> &YtDlpCandidateNormalizationRejection {
        &self.reason
    }

    /// Создаёт visible rejection внутри owner-controlled mapping-а.
    pub(super) const fn new(
        origin: YtDlpCandidateOrigin,
        identity: Option<CandidateIdentity>,
        reason: YtDlpCandidateNormalizationRejection,
    ) -> Self {
        Self {
            origin,
            identity,
            reason,
        }
    }
}

/// Accepted normalized candidate вместе с transient component material.
#[derive(Clone, PartialEq)]
pub struct YtDlpNormalizedCandidate {
    /// Service-neutral descriptor.
    descriptor: CandidateDescriptor,
    /// Один request resource для single result, два — только для compound merge.
    pub(super) component_requests: Box<[YtDlpCandidateComponentRequest]>,
}

impl YtDlpNormalizedCandidate {
    /// Возвращает neutral descriptor.
    pub const fn descriptor(&self) -> &CandidateDescriptor {
        &self.descriptor
    }

    /// Возвращает число реальных request components без Cartesian expansion.
    pub const fn component_count(&self) -> usize {
        self.component_requests.len()
    }

    /// Итерирует только safe summaries request components.
    pub fn component_request_summaries(
        &self,
    ) -> impl ExactSizeIterator<Item = YtDlpCandidateComponentRequestSummary> + '_ {
        self.component_requests
            .iter()
            .map(|component| YtDlpCandidateComponentRequestSummary {
                role: component.role,
                material: component.material.summary(),
            })
    }

    /// Собирает accepted candidate после всех owner-side checks.
    pub(super) fn new(
        descriptor: CandidateDescriptor,
        component_requests: Vec<(YtDlpCandidateComponentRole, YtDlpRequestMaterial)>,
    ) -> Self {
        debug_assert!(matches!(component_requests.len(), 1 | 2));
        Self {
            descriptor,
            component_requests: component_requests
                .into_iter()
                .map(|(role, material)| YtDlpCandidateComponentRequest { role, material })
                .collect(),
        }
    }
}

impl fmt::Debug for YtDlpNormalizedCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let summaries: Vec<_> = self.component_request_summaries().collect();
        formatter
            .debug_struct("YtDlpNormalizedCandidate")
            .field("descriptor", &self.descriptor)
            .field("component_request_summaries", &summaries)
            .finish_non_exhaustive()
    }
}

/// Одна visible inventory/selected entry без публикации storage representation.
#[derive(Debug, Clone, PartialEq)]
pub struct YtDlpCandidateEntry {
    /// Закрытый accepted/rejected storage.
    kind: YtDlpCandidateEntryKind,
}

/// Внутреннее представление entry может безопасно использовать indirection.
#[derive(Debug, Clone, PartialEq)]
enum YtDlpCandidateEntryKind {
    /// Candidate прошёл static normalization.
    Accepted(Box<YtDlpNormalizedCandidate>),
    /// Candidate сохранён как typed rejection.
    Rejected(YtDlpRejectedCandidate),
}

impl YtDlpCandidateEntry {
    /// Возвращает accepted candidate без потери rejected row.
    pub fn accepted(&self) -> Option<&YtDlpNormalizedCandidate> {
        match &self.kind {
            YtDlpCandidateEntryKind::Accepted(candidate) => Some(candidate.as_ref()),
            YtDlpCandidateEntryKind::Rejected(_) => None,
        }
    }

    /// Возвращает rejection без parsing enum через caller-side match.
    pub const fn rejected(&self) -> Option<&YtDlpRejectedCandidate> {
        match &self.kind {
            YtDlpCandidateEntryKind::Accepted(_) => None,
            YtDlpCandidateEntryKind::Rejected(rejection) => Some(rejection),
        }
    }

    /// Создаёт accepted entry внутри normalization owner-а.
    pub(super) fn accepted_entry(candidate: YtDlpNormalizedCandidate) -> Self {
        Self {
            kind: YtDlpCandidateEntryKind::Accepted(Box::new(candidate)),
        }
    }

    /// Создаёт rejected entry внутри normalization owner-а.
    pub(super) const fn rejected_entry(rejection: YtDlpRejectedCandidate) -> Self {
        Self {
            kind: YtDlpCandidateEntryKind::Rejected(rejection),
        }
    }
}

/// Immutable normalized extraction snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct YtDlpCandidateSnapshot {
    /// Source lineage даже для пустого/rejected-only inventory.
    source: SourceIdentity,
    /// Extraction generation для stale fencing.
    generation: ExtractionGeneration,
    /// Playlist metadata из этого же immutable extraction snapshot-а.
    playlist_metadata: YtDlpPlaylistMetadata,
    /// Один entry на каждую исходную `formats[]` row.
    inventory: Box<[YtDlpCandidateEntry]>,
    /// Корневой selected result, не смешанный с inventory.
    selected: Option<YtDlpCandidateEntry>,
}

impl YtDlpCandidateSnapshot {
    /// Возвращает source lineage snapshot-а.
    pub const fn source(&self) -> SourceIdentity {
        self.source
    }

    /// Возвращает immutable generation.
    pub const fn generation(&self) -> ExtractionGeneration {
        self.generation
    }

    /// Возвращает metadata, согласованную с candidate inventory и generation.
    pub const fn playlist_metadata(&self) -> &YtDlpPlaylistMetadata {
        &self.playlist_metadata
    }

    /// Возвращает полный visible inventory, включая rejection rows.
    pub const fn inventory(&self) -> &[YtDlpCandidateEntry] {
        &self.inventory
    }

    /// Возвращает отдельно normalized selected result.
    pub const fn selected(&self) -> Option<&YtDlpCandidateEntry> {
        self.selected.as_ref()
    }

    /// Создаёт process-local selection token из accepted inventory candidate-а.
    pub fn selection_for(
        &self,
        candidate: &YtDlpNormalizedCandidate,
    ) -> Result<YtDlpCandidateSelection, YtDlpCandidateSelectionError> {
        if candidate.descriptor().identity().source() != self.source {
            return Err(YtDlpCandidateSelectionError::ForeignSource);
        }
        if candidate.descriptor().identity().generation() != self.generation {
            return Err(YtDlpCandidateSelectionError::ForeignGeneration);
        }
        let belongs_to_snapshot = self
            .accepted_candidates()
            .any(|snapshot_candidate| snapshot_candidate.descriptor() == candidate.descriptor());
        if !belongs_to_snapshot {
            return Err(YtDlpCandidateSelectionError::CandidateNotInInventory);
        }
        Ok(YtDlpCandidateSelection {
            descriptor: candidate.descriptor.clone(),
        })
    }

    /// Сопоставляет Exact selection с текущим либо повторно извлечённым snapshot-ом.
    pub fn rematch_exact(
        &self,
        selection: &YtDlpCandidateSelection,
    ) -> Result<YtDlpCandidateMatch<'_>, YtDlpCandidateRematchError> {
        let selected_identity = selection.descriptor.identity();
        let selected_semantic = selection.descriptor.semantic_identity();
        if self.source != selected_identity.source() {
            return Err(YtDlpCandidateRematchError::SourceMismatch);
        }

        if self.generation == selected_identity.generation() {
            let Some(candidate) = self
                .accepted_candidates()
                .find(|candidate| candidate.descriptor().identity() == selected_identity)
            else {
                return Err(YtDlpCandidateRematchError::StaleExactIdentity);
            };
            if candidate.descriptor().semantic_identity() != selected_semantic
                || candidate.descriptor().layout() != selection.descriptor.layout()
            {
                return Err(YtDlpCandidateRematchError::ExactAttributesChanged);
            }
            return Ok(YtDlpCandidateMatch {
                kind: YtDlpCandidateMatchKind::Exact,
                candidate,
            });
        }

        let mut semantic_matches = self.accepted_candidates().filter(|candidate| {
            candidate.descriptor().semantic_identity() == selected_semantic
                && candidate.descriptor().layout() == selection.descriptor.layout()
        });
        let Some(candidate) = semantic_matches.next() else {
            return Err(YtDlpCandidateRematchError::StaleExactIdentity);
        };
        if semantic_matches.next().is_some() {
            return Err(YtDlpCandidateRematchError::AmbiguousSemanticIdentity);
        }
        Ok(YtDlpCandidateMatch {
            kind: YtDlpCandidateMatchKind::SemanticRematch,
            candidate,
        })
    }

    /// Публикует snapshot после полного mapping-а.
    pub(super) fn new(
        source: SourceIdentity,
        generation: ExtractionGeneration,
        playlist_metadata: YtDlpPlaylistMetadata,
        inventory: Vec<YtDlpCandidateEntry>,
        selected: Option<YtDlpCandidateEntry>,
    ) -> Self {
        Self {
            source,
            generation,
            playlist_metadata,
            inventory: inventory.into_boxed_slice(),
            selected,
        }
    }

    /// Итерирует selected result перед inventory, сохраняя richer request material при duplicate ID.
    pub fn accepted_candidates(&self) -> impl Iterator<Item = &YtDlpNormalizedCandidate> {
        self.selected
            .as_ref()
            .and_then(YtDlpCandidateEntry::accepted)
            .into_iter()
            .chain(
                self.inventory
                    .iter()
                    .filter_map(YtDlpCandidateEntry::accepted),
            )
    }
}

/// Process-local exact selection хранит ID, generation и semantic attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YtDlpCandidateSelection {
    /// Полный neutral descriptor нужен для validated semantic rematch.
    descriptor: CandidateDescriptor,
}

/// Typed отказ создания selection token-а из foreign/non-inventory candidate-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum YtDlpCandidateSelectionError {
    /// Candidate принадлежит другой source lineage.
    #[error("candidate принадлежит другой source lineage")]
    ForeignSource,
    /// Candidate принадлежит другому extraction generation.
    #[error("candidate принадлежит другой extraction generation")]
    ForeignGeneration,
    /// Candidate не найден среди accepted rows snapshot-а.
    #[error("candidate отсутствует среди accepted snapshot rows")]
    CandidateNotInInventory,
}

impl YtDlpCandidateSelection {
    /// Возвращает snapshot-local identity.
    pub const fn exact_identity(&self) -> &CandidateIdentity {
        self.descriptor.identity()
    }

    /// Возвращает refresh-stable semantic identity.
    pub const fn semantic_identity(&self) -> &SemanticIdentity {
        self.descriptor.semantic_identity()
    }
}

/// Успешный вид сопоставления selection-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpCandidateMatchKind {
    /// Exact ID найден в том же generation.
    Exact,
    /// После re-extraction найден единственный semantic+attribute match.
    SemanticRematch,
}

/// Borrowed успешный match без копирования request material.
#[derive(Debug, Clone, Copy)]
pub struct YtDlpCandidateMatch<'snapshot> {
    /// Вид сопоставления.
    kind: YtDlpCandidateMatchKind,
    /// Candidate из нового snapshot-а.
    candidate: &'snapshot YtDlpNormalizedCandidate,
}

impl<'snapshot> YtDlpCandidateMatch<'snapshot> {
    /// Возвращает вид match-а.
    pub const fn kind(self) -> YtDlpCandidateMatchKind {
        self.kind
    }

    /// Возвращает matched candidate.
    pub const fn candidate(self) -> &'snapshot YtDlpNormalizedCandidate {
        self.candidate
    }
}

/// Typed Exact/rematch failure без format identities в diagnostic payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum YtDlpCandidateRematchError {
    /// Snapshot принадлежит другой source lineage.
    #[error("snapshot принадлежит другой source lineage")]
    SourceMismatch,
    /// Exact ID устарел и semantic match отсутствует.
    #[error("exact identity устарела и semantic match отсутствует")]
    StaleExactIdentity,
    /// Same-generation ID найден, но semantic attributes изменились.
    #[error("same-generation candidate изменил semantic attributes")]
    ExactAttributesChanged,
    /// Semantic identity не уникальна после attribute validation.
    #[error("semantic identity неоднозначна после attribute validation")]
    AmbiguousSemanticIdentity,
}
