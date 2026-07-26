//! Validation, lookup и immutable replacement для component catalog.

use super::*;

impl ComponentVariantCatalog {
    /// Проверяет scope, axes, bounds и unique exact/semantic identities.
    pub fn new(
        identity: ComponentVariantCatalogIdentity,
        limit: ComponentVariantCatalogLimit,
        entries: ComponentVariantCatalogEntries,
    ) -> Result<Self, ComponentVariantError> {
        match entries {
            ComponentVariantCatalogEntries::Topology {
                video,
                audio,
                compatibility,
                coupled,
                video_only,
                audio_only,
            } => {
                validate_catalog_entry_limit(video.len(), audio.len(), coupled.len(), limit)?;
                validate_video_variants(&identity, &video)?;
                validate_audio_variants(&identity, &audio)?;
                validate_unique_identities(&video, &audio)?;
                validate_coupled_variants(&identity, &coupled)?;
                let compatibility =
                    validate_compatibility(&identity, &video, &audio, compatibility)?;
                validate_standalone_references(
                    &identity,
                    &video,
                    &audio,
                    &video_only,
                    &audio_only,
                )?;
                if compatibility.logical_edge_count() == 0
                    && coupled.is_empty()
                    && video_only.is_empty()
                    && audio_only.is_empty()
                {
                    return Err(ComponentVariantError::NoSelectablePresentation);
                }
                Ok(Self::Topology {
                    identity,
                    video: video.into_boxed_slice(),
                    audio: audio.into_boxed_slice(),
                    compatibility,
                    coupled: coupled.into_boxed_slice(),
                    video_only: video_only.into_boxed_slice(),
                    audio_only: audio_only.into_boxed_slice(),
                })
            }
            ComponentVariantCatalogEntries::VideoAndAudio { video, audio } => {
                require_non_empty_axis(video.len(), ComponentKind::Video)?;
                require_non_empty_axis(audio.len(), ComponentKind::Audio)?;
                validate_catalog_limit(video.len(), audio.len(), limit)?;
                validate_video_variants(&identity, &video)?;
                validate_audio_variants(&identity, &audio)?;
                validate_unique_identities(&video, &audio)?;
                Ok(Self::VideoAndAudio {
                    identity,
                    video: video.into_boxed_slice(),
                    audio: audio.into_boxed_slice(),
                })
            }
            ComponentVariantCatalogEntries::VideoOnly { video } => {
                require_non_empty_axis(video.len(), ComponentKind::Video)?;
                validate_catalog_limit(video.len(), 0, limit)?;
                validate_video_variants(&identity, &video)?;
                validate_unique_identities(&video, &[])?;
                Ok(Self::VideoOnly {
                    identity,
                    video: video.into_boxed_slice(),
                })
            }
            ComponentVariantCatalogEntries::AudioOnly { audio } => {
                require_non_empty_axis(audio.len(), ComponentKind::Audio)?;
                validate_catalog_limit(0, audio.len(), limit)?;
                validate_audio_variants(&identity, &audio)?;
                validate_unique_identities(&[], &audio)?;
                Ok(Self::AudioOnly {
                    identity,
                    audio: audio.into_boxed_slice(),
                })
            }
        }
    }

    /// Возвращает catalog identity независимо от layout shape.
    pub const fn identity(&self) -> &ComponentVariantCatalogIdentity {
        match self {
            Self::Topology { identity, .. }
            | Self::VideoAndAudio { identity, .. }
            | Self::VideoOnly { identity, .. }
            | Self::AudioOnly { identity, .. } => identity,
        }
    }

    /// Возвращает required video slice либо typed сообщает отсутствие axis.
    pub fn required_video_variants(
        &self,
    ) -> Result<&[VideoComponentVariant], ComponentVariantError> {
        match self {
            Self::Topology { video, .. } => require_axis_slice(video, ComponentKind::Video),
            Self::VideoAndAudio { video, .. } | Self::VideoOnly { video, .. } => Ok(video),
            Self::AudioOnly { .. } => Err(ComponentVariantError::MissingRequiredAxis {
                component: ComponentKind::Video,
            }),
        }
    }

    /// Возвращает required audio slice либо typed сообщает отсутствие axis.
    pub fn required_audio_variants(
        &self,
    ) -> Result<&[AudioComponentVariant], ComponentVariantError> {
        match self {
            Self::Topology { audio, .. } => require_axis_slice(audio, ComponentKind::Audio),
            Self::VideoAndAudio { audio, .. } | Self::AudioOnly { audio, .. } => Ok(audio),
            Self::VideoOnly { .. } => Err(ComponentVariantError::MissingRequiredAxis {
                component: ComponentKind::Audio,
            }),
        }
    }

    /// Возвращает реальную storage cardinality `V + A`, не Cartesian `V × A`.
    pub const fn stored_variant_count(&self) -> usize {
        match self {
            Self::Topology {
                video,
                audio,
                coupled,
                ..
            } => video.len() + audio.len() + coupled.len(),
            Self::VideoAndAudio { video, audio, .. } => video.len() + audio.len(),
            Self::VideoOnly { video, .. } => video.len(),
            Self::AudioOnly { audio, .. } => audio.len(),
        }
    }

    /// Выбирает exact rows с обязательным совпадением layout shape.
    pub fn select_exact(
        &self,
        request: ComponentVariantSelectionRequest,
    ) -> Result<ComponentVariantSelection, ComponentVariantError> {
        match (self, request) {
            (
                Self::Topology { compatibility, .. },
                ComponentVariantSelectionRequest::VideoAndAudio { video, audio },
            ) => {
                let video = self.find_video_exact(&video)?;
                let audio = self.find_audio_exact(&audio)?;
                if !compatibility.allows(video.exact_identity(), audio.exact_identity()) {
                    return Err(ComponentVariantError::IncompatibleComponentPair);
                }
                Ok(ComponentVariantSelection::VideoAndAudio {
                    video: Box::new(video.clone()),
                    audio: Box::new(audio.clone()),
                })
            }
            (
                Self::Topology { video_only, .. },
                ComponentVariantSelectionRequest::VideoOnly { video },
            ) => {
                let video = self.find_video_exact(&video)?;
                if !video_only.contains(video.exact_identity()) {
                    return Err(ComponentVariantError::IncompatibleComponentPair);
                }
                Ok(ComponentVariantSelection::VideoOnly {
                    video: Box::new(video.clone()),
                })
            }
            (
                Self::Topology { audio_only, .. },
                ComponentVariantSelectionRequest::AudioOnly { audio },
            ) => {
                let audio = self.find_audio_exact(&audio)?;
                if !audio_only.contains(audio.exact_identity()) {
                    return Err(ComponentVariantError::IncompatibleComponentPair);
                }
                Ok(ComponentVariantSelection::AudioOnly {
                    audio: Box::new(audio.clone()),
                })
            }
            (Self::Topology { .. }, ComponentVariantSelectionRequest::Coupled { presentation }) => {
                Ok(ComponentVariantSelection::Coupled {
                    presentation: Box::new(self.find_coupled_exact(&presentation)?.clone()),
                })
            }
            (
                Self::VideoAndAudio { .. },
                ComponentVariantSelectionRequest::VideoAndAudio { video, audio },
            ) => Ok(ComponentVariantSelection::VideoAndAudio {
                video: Box::new(self.find_video_exact(&video)?.clone()),
                audio: Box::new(self.find_audio_exact(&audio)?.clone()),
            }),
            (Self::VideoOnly { .. }, ComponentVariantSelectionRequest::VideoOnly { video }) => {
                Ok(ComponentVariantSelection::VideoOnly {
                    video: Box::new(self.find_video_exact(&video)?.clone()),
                })
            }
            (Self::AudioOnly { .. }, ComponentVariantSelectionRequest::AudioOnly { audio }) => {
                Ok(ComponentVariantSelection::AudioOnly {
                    audio: Box::new(self.find_audio_exact(&audio)?.clone()),
                })
            }
            _ => Err(ComponentVariantError::LayoutMismatch),
        }
    }

    /// Возвращает first-best video row только по existing preferred-height policy.
    ///
    /// При равном rank сохраняется первый catalog row: новый product tie-break
    /// намеренно не вводится.
    pub fn preferred_video_variant(
        &self,
        policy: PreferredHeightPolicy,
    ) -> Result<&VideoComponentVariant, ComponentVariantError> {
        let variants = self.required_video_variants()?;
        let mut preferred = &variants[0];
        for candidate in &variants[1..] {
            if policy.compare(candidate.track().height(), preferred.track().height())
                == Ordering::Less
            {
                preferred = candidate;
            }
        }
        Ok(preferred)
    }

    /// Находит exact video row после source/parent/generation/axis validation.
    fn find_video_exact(
        &self,
        requested: &ComponentVariantExactIdentity,
    ) -> Result<&VideoComponentVariant, ComponentVariantError> {
        validate_exact_scope(self.identity(), requested, ComponentKind::Video)?;
        self.required_video_variants()?
            .iter()
            .find(|variant| variant.exact_identity() == requested)
            .ok_or(ComponentVariantError::MissingVariant {
                component: ComponentKind::Video,
            })
    }

    /// Находит exact audio row после source/parent/generation/axis validation.
    fn find_audio_exact(
        &self,
        requested: &ComponentVariantExactIdentity,
    ) -> Result<&AudioComponentVariant, ComponentVariantError> {
        validate_exact_scope(self.identity(), requested, ComponentKind::Audio)?;
        self.required_audio_variants()?
            .iter()
            .find(|variant| variant.exact_identity() == requested)
            .ok_or(ComponentVariantError::MissingVariant {
                component: ComponentKind::Audio,
            })
    }

    /// Возвращает complete coupled/muxed rows.
    pub fn coupled_presentations(&self) -> &[CoupledComponentVariant] {
        match self {
            Self::Topology { coupled, .. } => coupled,
            _ => &[],
        }
    }

    /// Возвращает logical compatibility relation.
    pub fn compatibility(&self) -> Option<&ComponentVariantCompatibility> {
        match self {
            Self::Topology { compatibility, .. } => Some(compatibility),
            Self::VideoAndAudio { .. } => None,
            Self::VideoOnly { .. } | Self::AudioOnly { .. } => None,
        }
    }

    /// Проверяет, опубликована ли video row как standalone selection.
    pub fn is_video_only_selectable(&self, identity: &ComponentVariantExactIdentity) -> bool {
        match self {
            Self::Topology { video_only, .. } => video_only.contains(identity),
            Self::VideoOnly { video, .. } => video
                .iter()
                .any(|variant| variant.exact_identity() == identity),
            _ => false,
        }
    }

    /// Проверяет, опубликована ли audio row как standalone selection.
    pub fn is_audio_only_selectable(&self, identity: &ComponentVariantExactIdentity) -> bool {
        match self {
            Self::Topology { audio_only, .. } => audio_only.contains(identity),
            Self::AudioOnly { audio, .. } => audio
                .iter()
                .any(|variant| variant.exact_identity() == identity),
            _ => false,
        }
    }

    fn find_coupled_exact(
        &self,
        requested: &CoupledVariantExactIdentity,
    ) -> Result<&CoupledComponentVariant, ComponentVariantError> {
        validate_coupled_exact_scope(self.identity(), requested)?;
        self.coupled_presentations()
            .iter()
            .find(|variant| variant.exact_identity() == requested)
            .ok_or(ComponentVariantError::MissingCoupledPresentation)
    }
}

impl ComponentVariantSelection {
    /// Возвращает новый selection с другим video и неизменённым audio.
    pub fn replace_video(
        &self,
        catalog: &ComponentVariantCatalog,
        requested: &ComponentVariantExactIdentity,
    ) -> Result<Self, ComponentVariantError> {
        validate_selection_catalog_scope(self, catalog)?;
        let replacement = Box::new(catalog.find_video_exact(requested)?.clone());
        match (self, catalog) {
            (
                Self::VideoAndAudio { audio, .. },
                ComponentVariantCatalog::Topology { compatibility, .. },
            ) => {
                if !compatibility.allows(replacement.exact_identity(), audio.exact_identity()) {
                    return Err(ComponentVariantError::IncompatibleComponentPair);
                }
                Ok(Self::VideoAndAudio {
                    video: replacement,
                    audio: audio.clone(),
                })
            }
            (Self::VideoOnly { .. }, ComponentVariantCatalog::Topology { video_only, .. }) => {
                if !video_only.contains(replacement.exact_identity()) {
                    return Err(ComponentVariantError::IncompatibleComponentPair);
                }
                Ok(Self::VideoOnly { video: replacement })
            }
            (Self::VideoAndAudio { audio, .. }, ComponentVariantCatalog::VideoAndAudio { .. }) => {
                Ok(Self::VideoAndAudio {
                    video: replacement,
                    audio: audio.clone(),
                })
            }
            (Self::VideoOnly { .. }, ComponentVariantCatalog::VideoOnly { .. }) => {
                Ok(Self::VideoOnly { video: replacement })
            }
            (Self::AudioOnly { .. }, _) => Err(ComponentVariantError::MissingRequiredAxis {
                component: ComponentKind::Video,
            }),
            _ => Err(ComponentVariantError::LayoutMismatch),
        }
    }

    /// Возвращает новый selection с другим audio и неизменённым video.
    pub fn replace_audio(
        &self,
        catalog: &ComponentVariantCatalog,
        requested: &ComponentVariantExactIdentity,
    ) -> Result<Self, ComponentVariantError> {
        validate_selection_catalog_scope(self, catalog)?;
        let replacement = Box::new(catalog.find_audio_exact(requested)?.clone());
        match (self, catalog) {
            (
                Self::VideoAndAudio { video, .. },
                ComponentVariantCatalog::Topology { compatibility, .. },
            ) => {
                if !compatibility.allows(video.exact_identity(), replacement.exact_identity()) {
                    return Err(ComponentVariantError::IncompatibleComponentPair);
                }
                Ok(Self::VideoAndAudio {
                    video: video.clone(),
                    audio: replacement,
                })
            }
            (Self::AudioOnly { .. }, ComponentVariantCatalog::Topology { audio_only, .. }) => {
                if !audio_only.contains(replacement.exact_identity()) {
                    return Err(ComponentVariantError::IncompatibleComponentPair);
                }
                Ok(Self::AudioOnly { audio: replacement })
            }
            (Self::VideoAndAudio { video, .. }, ComponentVariantCatalog::VideoAndAudio { .. }) => {
                Ok(Self::VideoAndAudio {
                    video: video.clone(),
                    audio: replacement,
                })
            }
            (Self::AudioOnly { .. }, ComponentVariantCatalog::AudioOnly { .. }) => {
                Ok(Self::AudioOnly { audio: replacement })
            }
            (Self::VideoOnly { .. }, _) => Err(ComponentVariantError::MissingRequiredAxis {
                component: ComponentKind::Audio,
            }),
            _ => Err(ComponentVariantError::LayoutMismatch),
        }
    }

    /// Возвращает catalog identity, доказанную всеми rows selection.
    fn catalog_identity(&self) -> &ComponentVariantCatalogIdentity {
        match self {
            Self::VideoAndAudio { video, .. } | Self::VideoOnly { video } => {
                video.exact_identity().catalog()
            }
            Self::AudioOnly { audio } => audio.exact_identity().catalog(),
            Self::Coupled { presentation } => presentation.exact_identity().catalog(),
        }
    }
}

/// Проверяет required non-empty axis.
fn require_non_empty_axis(
    entries: usize,
    component: ComponentKind,
) -> Result<(), ComponentVariantError> {
    if entries == 0 {
        return Err(ComponentVariantError::MissingRequiredAxis { component });
    }
    Ok(())
}

fn require_axis_slice<T>(
    entries: &[T],
    component: ComponentKind,
) -> Result<&[T], ComponentVariantError> {
    require_non_empty_axis(entries.len(), component)?;
    Ok(entries)
}

/// Проверяет суммарную storage cardinality без умножения axes.
fn validate_catalog_limit(
    video_entries: usize,
    audio_entries: usize,
    limit: ComponentVariantCatalogLimit,
) -> Result<(), ComponentVariantError> {
    let provided_entries = video_entries.checked_add(audio_entries).ok_or(
        ComponentVariantError::CatalogLimitExceeded {
            provided_entries: usize::MAX,
            maximum_entries: limit.maximum_entries(),
        },
    )?;
    if provided_entries > limit.maximum_entries() {
        return Err(ComponentVariantError::CatalogLimitExceeded {
            provided_entries,
            maximum_entries: limit.maximum_entries(),
        });
    }
    Ok(())
}

fn validate_catalog_entry_limit(
    video_entries: usize,
    audio_entries: usize,
    coupled_entries: usize,
    limit: ComponentVariantCatalogLimit,
) -> Result<(), ComponentVariantError> {
    let provided_entries = video_entries
        .checked_add(audio_entries)
        .and_then(|entries| entries.checked_add(coupled_entries))
        .ok_or(ComponentVariantError::CatalogLimitExceeded {
            provided_entries: usize::MAX,
            maximum_entries: limit.maximum_entries(),
        })?;
    if provided_entries > limit.maximum_entries() {
        return Err(ComponentVariantError::CatalogLimitExceeded {
            provided_entries,
            maximum_entries: limit.maximum_entries(),
        });
    }
    Ok(())
}

/// Проверяет scope/axis каждой video row.
fn validate_video_variants(
    catalog: &ComponentVariantCatalogIdentity,
    variants: &[VideoComponentVariant],
) -> Result<(), ComponentVariantError> {
    for variant in variants {
        validate_variant_scope(
            catalog,
            variant.exact_identity(),
            variant.semantic_identity(),
            ComponentKind::Video,
        )?;
    }
    Ok(())
}

/// Проверяет scope/axis каждой audio row.
fn validate_audio_variants(
    catalog: &ComponentVariantCatalogIdentity,
    variants: &[AudioComponentVariant],
) -> Result<(), ComponentVariantError> {
    for variant in variants {
        validate_variant_scope(
            catalog,
            variant.exact_identity(),
            variant.semantic_identity(),
            ComponentKind::Audio,
        )?;
    }
    Ok(())
}

/// Проверяет exact и semantic identities одной row.
fn validate_variant_scope(
    catalog: &ComponentVariantCatalogIdentity,
    exact: &ComponentVariantExactIdentity,
    semantic: &ComponentVariantSemanticIdentity,
    expected_component: ComponentKind,
) -> Result<(), ComponentVariantError> {
    validate_exact_scope(catalog, exact, expected_component)?;
    validate_semantic_scope(catalog, semantic, expected_component)
}

/// Проверяет exact identity относительно active catalog.
fn validate_exact_scope(
    catalog: &ComponentVariantCatalogIdentity,
    exact: &ComponentVariantExactIdentity,
    expected_component: ComponentKind,
) -> Result<(), ComponentVariantError> {
    if catalog.source() != exact.catalog().source() {
        return Err(ComponentVariantError::SourceMismatch);
    }
    if catalog.parent() != exact.catalog().parent() {
        return Err(ComponentVariantError::CrossParent);
    }
    if catalog.generation() != exact.catalog().generation() {
        return Err(ComponentVariantError::StaleCatalogGeneration {
            expected: catalog.generation(),
            provided: exact.catalog().generation(),
        });
    }
    if exact.component() != expected_component {
        return Err(ComponentVariantError::WrongAxis {
            expected: expected_component,
            provided: exact.component(),
        });
    }
    Ok(())
}

/// Проверяет semantic identity относительно active parent без generation coupling.
pub(super) fn validate_semantic_scope(
    catalog: &ComponentVariantCatalogIdentity,
    semantic: &ComponentVariantSemanticIdentity,
    expected_component: ComponentKind,
) -> Result<(), ComponentVariantError> {
    if catalog.source() != semantic.source() {
        return Err(ComponentVariantError::SourceMismatch);
    }
    if catalog.parent().semantic() != semantic.parent() {
        return Err(ComponentVariantError::CrossParent);
    }
    if semantic.component() != expected_component {
        return Err(ComponentVariantError::WrongAxis {
            expected: expected_component,
            provided: semantic.component(),
        });
    }
    Ok(())
}

/// Проверяет unique exact и semantic identities across stored axes.
fn validate_unique_identities(
    video_variants: &[VideoComponentVariant],
    audio_variants: &[AudioComponentVariant],
) -> Result<(), ComponentVariantError> {
    for (index, variant) in video_variants.iter().enumerate() {
        if video_variants[..index]
            .iter()
            .any(|previous| previous.exact_identity() == variant.exact_identity())
        {
            return Err(ComponentVariantError::DuplicateExactIdentity {
                component: ComponentKind::Video,
            });
        }
        if video_variants[..index]
            .iter()
            .any(|previous| previous.semantic_identity() == variant.semantic_identity())
        {
            return Err(ComponentVariantError::AmbiguousSemanticIdentity {
                component: ComponentKind::Video,
            });
        }
    }
    for (index, variant) in audio_variants.iter().enumerate() {
        if audio_variants[..index]
            .iter()
            .any(|previous| previous.exact_identity() == variant.exact_identity())
        {
            return Err(ComponentVariantError::DuplicateExactIdentity {
                component: ComponentKind::Audio,
            });
        }
        if audio_variants[..index]
            .iter()
            .any(|previous| previous.semantic_identity() == variant.semantic_identity())
        {
            return Err(ComponentVariantError::AmbiguousSemanticIdentity {
                component: ComponentKind::Audio,
            });
        }
    }
    Ok(())
}

fn validate_coupled_variants(
    catalog: &ComponentVariantCatalogIdentity,
    variants: &[CoupledComponentVariant],
) -> Result<(), ComponentVariantError> {
    for (index, variant) in variants.iter().enumerate() {
        validate_coupled_exact_scope(catalog, variant.exact_identity())?;
        validate_coupled_semantic_scope(catalog, variant.semantic_identity())?;
        if variants[..index]
            .iter()
            .any(|previous| previous.exact_identity() == variant.exact_identity())
        {
            return Err(ComponentVariantError::DuplicateCoupledExactIdentity);
        }
        if variants[..index]
            .iter()
            .any(|previous| previous.semantic_identity() == variant.semantic_identity())
        {
            return Err(ComponentVariantError::AmbiguousCoupledSemanticIdentity);
        }
    }
    Ok(())
}

fn validate_compatibility(
    catalog: &ComponentVariantCatalogIdentity,
    video: &[VideoComponentVariant],
    audio: &[AudioComponentVariant],
    entries: ComponentVariantCompatibilityEntries,
) -> Result<ComponentVariantCompatibility, ComponentVariantError> {
    match entries {
        ComponentVariantCompatibilityEntries::Unavailable => {
            Ok(ComponentVariantCompatibility::unavailable())
        }
        ComponentVariantCompatibilityEntries::AllPairs { edge_limit } => {
            require_non_empty_axis(video.len(), ComponentKind::Video)?;
            require_non_empty_axis(audio.len(), ComponentKind::Audio)?;
            let logical_edges = video.len().checked_mul(audio.len()).ok_or(
                ComponentVariantError::CompatibilityEdgeLimitExceeded {
                    provided_edges: usize::MAX,
                    maximum_edges: edge_limit.maximum_edges(),
                },
            )?;
            validate_edge_limit(logical_edges, edge_limit)?;
            Ok(ComponentVariantCompatibility::all_pairs(logical_edges))
        }
        ComponentVariantCompatibilityEntries::Sparse { edge_limit, edges } => {
            validate_edge_limit(edges.len(), edge_limit)?;
            for (index, edge) in edges.iter().enumerate() {
                validate_exact_scope(catalog, edge.video(), ComponentKind::Video)?;
                validate_exact_scope(catalog, edge.audio(), ComponentKind::Audio)?;
                if !video
                    .iter()
                    .any(|variant| variant.exact_identity() == edge.video())
                {
                    return Err(ComponentVariantError::DanglingVariantReference {
                        component: ComponentKind::Video,
                    });
                }
                if !audio
                    .iter()
                    .any(|variant| variant.exact_identity() == edge.audio())
                {
                    return Err(ComponentVariantError::DanglingVariantReference {
                        component: ComponentKind::Audio,
                    });
                }
                if edges[..index].contains(edge) {
                    return Err(ComponentVariantError::DuplicateCompatibilityEdge);
                }
            }
            Ok(ComponentVariantCompatibility::sparse(edges))
        }
    }
}

fn validate_edge_limit(
    provided_edges: usize,
    limit: ComponentVariantEdgeLimit,
) -> Result<(), ComponentVariantError> {
    if provided_edges > limit.maximum_edges() {
        return Err(ComponentVariantError::CompatibilityEdgeLimitExceeded {
            provided_edges,
            maximum_edges: limit.maximum_edges(),
        });
    }
    Ok(())
}

fn validate_standalone_references(
    catalog: &ComponentVariantCatalogIdentity,
    video: &[VideoComponentVariant],
    audio: &[AudioComponentVariant],
    video_only: &[ComponentVariantExactIdentity],
    audio_only: &[ComponentVariantExactIdentity],
) -> Result<(), ComponentVariantError> {
    validate_standalone_axis(
        catalog,
        video_only,
        video.iter().map(VideoComponentVariant::exact_identity),
        ComponentKind::Video,
    )?;
    validate_standalone_axis(
        catalog,
        audio_only,
        audio.iter().map(AudioComponentVariant::exact_identity),
        ComponentKind::Audio,
    )
}

fn validate_standalone_axis<'a>(
    catalog: &ComponentVariantCatalogIdentity,
    references: &[ComponentVariantExactIdentity],
    available: impl Iterator<Item = &'a ComponentVariantExactIdentity> + Clone,
    component: ComponentKind,
) -> Result<(), ComponentVariantError> {
    for (index, identity) in references.iter().enumerate() {
        validate_exact_scope(catalog, identity, component)?;
        if !available.clone().any(|candidate| candidate == identity) {
            return Err(ComponentVariantError::DanglingVariantReference { component });
        }
        if references[..index].contains(identity) {
            return Err(ComponentVariantError::DuplicateVariantReference { component });
        }
    }
    Ok(())
}

fn validate_coupled_exact_scope(
    catalog: &ComponentVariantCatalogIdentity,
    exact: &CoupledVariantExactIdentity,
) -> Result<(), ComponentVariantError> {
    if catalog.source() != exact.catalog().source() {
        return Err(ComponentVariantError::SourceMismatch);
    }
    if catalog.parent() != exact.catalog().parent() {
        return Err(ComponentVariantError::CrossParent);
    }
    if catalog.generation() != exact.catalog().generation() {
        return Err(ComponentVariantError::StaleCatalogGeneration {
            expected: catalog.generation(),
            provided: exact.catalog().generation(),
        });
    }
    Ok(())
}

pub(super) fn validate_coupled_semantic_scope(
    catalog: &ComponentVariantCatalogIdentity,
    semantic: &CoupledVariantSemanticIdentity,
) -> Result<(), ComponentVariantError> {
    if catalog.source() != semantic.source() {
        return Err(ComponentVariantError::SourceMismatch);
    }
    if catalog.parent().semantic() != semantic.parent() {
        return Err(ComponentVariantError::CrossParent);
    }
    Ok(())
}

/// Не даёт применять selection к другому parent/generation catalog.
fn validate_selection_catalog_scope(
    selection: &ComponentVariantSelection,
    catalog: &ComponentVariantCatalog,
) -> Result<(), ComponentVariantError> {
    let selected_catalog = selection.catalog_identity();
    if selected_catalog.source() != catalog.identity().source() {
        return Err(ComponentVariantError::SourceMismatch);
    }
    if selected_catalog.parent() != catalog.identity().parent() {
        return Err(ComponentVariantError::CrossParent);
    }
    if selected_catalog.generation() != catalog.identity().generation() {
        return Err(ComponentVariantError::StaleCatalogGeneration {
            expected: catalog.identity().generation(),
            provided: selected_catalog.generation(),
        });
    }
    Ok(())
}
