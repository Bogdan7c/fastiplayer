//! Service-owned composition video-only + audio-only из одного fresh snapshot.

use std::fmt;

use web_media_core::{
    CandidateDescriptor, CandidateFormatIdentity, CandidateIdentity, SemanticIdentity, StreamLayout,
};

use super::model::{
    YtDlpCandidateComponentRole, YtDlpCandidateSelection, YtDlpCandidateSnapshot,
    YtDlpNormalizedCandidate,
};

/// Способ разрешения обеих component identities в snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpCompositionMatchKind {
    /// Обе components принадлежат исходной extraction generation.
    Exact,
    /// Обе components независимо rematch-нуты по semantic identity.
    SemanticRematch,
}

/// Typed ошибки service-owned A/V composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtDlpCompositionError {
    /// Selection относится к другой source lineage.
    ForeignSource,
    /// Selection той же generation не принадлежит authoritative inventory.
    ForeignGenerationOrInventory,
    /// Video selection не является atomic video-only row.
    VideoComponentRequired,
    /// Audio selection не является atomic audio-only row.
    AudioComponentRequired,
    /// Fresh video component отсутствует.
    MissingVideoComponent,
    /// Fresh audio component отсутствует.
    MissingAudioComponent,
    /// Fresh video semantic identity неоднозначна.
    AmbiguousVideoComponent,
    /// Fresh audio semantic identity неоднозначна.
    AmbiguousAudioComponent,
    /// Normalized component не содержит request material своей роли.
    MissingComponentRequest,
    /// Bounded composed identity не удалось построить.
    IdentityConstruction,
}

impl fmt::Display for YtDlpCompositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ForeignSource => "The selection component is from another source",
            Self::ForeignGenerationOrInventory => {
                "The selection component does not include authoritative fresh inventory"
            }
            Self::VideoComponentRequired => "composition requires an atomic video-only selection",
            Self::AudioComponentRequired => "composition requires an atomic audio-only selection",
            Self::MissingVideoComponent => "fresh video component is missing",
            Self::MissingAudioComponent => "fresh audio component is missing",
            Self::AmbiguousVideoComponent => "fresh video component is ambiguous",
            Self::AmbiguousAudioComponent => "fresh audio component is ambiguous",
            Self::MissingComponentRequest => "component request material is missing",
            Self::IdentityConstruction => "bounded composed identity was not built",
        })
    }
}

impl std::error::Error for YtDlpCompositionError {}

/// Exact+semantic composed intent без locator, format ID или request material.
#[derive(Clone, PartialEq, Eq)]
pub struct YtDlpComposedSelection {
    descriptor: CandidateDescriptor,
    video: YtDlpCandidateSelection,
    audio: YtDlpCandidateSelection,
}

impl fmt::Debug for YtDlpComposedSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpComposedSelection")
            .field("descriptor", &self.descriptor)
            .field("component_count", &2)
            .finish()
    }
}

impl YtDlpComposedSelection {
    /// Возвращает synthetic service-owned descriptor без raw component IDs.
    pub const fn descriptor(&self) -> &CandidateDescriptor {
        &self.descriptor
    }

    /// Возвращает semantic identity выбранного audio для preservation policy.
    pub const fn audio_semantic_identity(&self) -> &SemanticIdentity {
        self.audio.semantic_identity()
    }

    /// Возвращает video parent selection для согласованной app-owned picker projection.
    pub const fn video_parent_selection(&self) -> &YtDlpCandidateSelection {
        &self.video
    }
}

impl YtDlpCandidateSnapshot {
    /// Собирает exact video-only + audio-only intent только из authoritative inventory.
    pub fn compose_inventory_av(
        &self,
        video: &YtDlpCandidateSelection,
        audio: &YtDlpCandidateSelection,
    ) -> Result<YtDlpComposedSelection, YtDlpCompositionError> {
        let video = self.resolve_atomic_component(video, ComponentRole::Video)?;
        let audio = self.resolve_atomic_component(audio, ComponentRole::Audio)?;
        build_selection(video, audio)
    }

    /// Находит video-only строку inventory, из которой сделан выбранный compound-кандидат.
    ///
    /// Выбор yt-dlp по умолчанию (`requested_formats`) — это пара «видео + звук» в одном
    /// кандидате. Чтобы подменить в ней только звук, нужен video-only выбор из inventory с
    /// тем же request material (тот же поток). `None` — selection не compound этой
    /// генерации либо подходящей строки в inventory нет.
    pub fn inventory_video_of_compound(
        &self,
        compound: &YtDlpCandidateSelection,
    ) -> Option<YtDlpCandidateSelection> {
        self.inventory_component_of_compound(compound, ComponentRole::Video)
    }

    /// Находит audio-only строку inventory, из которой сделан выбранный compound-кандидат.
    ///
    /// Нужна BestPlayable-замене compound-а, чьи components нельзя открыть одним
    /// runtime-маршрутом (например HLS video + HTTPS audio): звук и язык, выбранные
    /// yt-dlp, сохраняются, меняется только video component. `None` — selection не
    /// compound этой генерации либо подходящей строки в inventory нет.
    pub fn inventory_audio_of_compound(
        &self,
        compound: &YtDlpCandidateSelection,
    ) -> Option<YtDlpCandidateSelection> {
        self.inventory_component_of_compound(compound, ComponentRole::Audio)
    }

    /// Общий поиск atomic inventory-строки с тем же request material, что component compound-а.
    fn inventory_component_of_compound(
        &self,
        compound: &YtDlpCandidateSelection,
        role: ComponentRole,
    ) -> Option<YtDlpCandidateSelection> {
        if compound.exact_identity().generation() != self.generation() {
            return None;
        }
        let compound_candidate = self.accepted_candidates().find(|candidate| {
            candidate.descriptor() == compound.descriptor()
                && candidate.video_color_evidence() == compound.video_color_evidence()
        })?;
        if !matches!(
            compound_candidate.descriptor().layout(),
            StreamLayout::Separate { .. }
        ) {
            return None;
        }
        let request_role = role.request_role();
        let component_request = compound_candidate.component_request_material(request_role)?;
        let expected_layout = role.layout_matches();
        let atomic_row = self.accepted_inventory_candidates().find(|candidate| {
            expected_layout(candidate.descriptor().layout())
                && candidate.component_request_material(request_role)
                    == Some(component_request.clone())
        })?;
        self.selection_for(atomic_row).ok()
    }

    /// Независимо rematch-ит обе components и восстанавливает fresh private request material.
    pub fn rematch_composed(
        &self,
        selection: &YtDlpComposedSelection,
    ) -> Result<
        (
            YtDlpCompositionMatchKind,
            YtDlpComposedSelection,
            YtDlpNormalizedCandidate,
        ),
        YtDlpCompositionError,
    > {
        if selection.descriptor.identity().source() != self.source() {
            return Err(YtDlpCompositionError::ForeignSource);
        }
        let exact = selection.descriptor.identity().generation() == self.generation();
        let video = self.resolve_atomic_component(&selection.video, ComponentRole::Video)?;
        let audio = self.resolve_atomic_component(&selection.audio, ComponentRole::Audio)?;
        let fresh_selection = build_selection(video, audio)?;
        let candidate = build_candidate(video, audio, fresh_selection.descriptor.clone())?;
        Ok((
            if exact {
                YtDlpCompositionMatchKind::Exact
            } else {
                YtDlpCompositionMatchKind::SemanticRematch
            },
            fresh_selection,
            candidate,
        ))
    }

    fn resolve_atomic_component<'a>(
        &'a self,
        selection: &YtDlpCandidateSelection,
        role: ComponentRole,
    ) -> Result<&'a YtDlpNormalizedCandidate, YtDlpCompositionError> {
        if selection.exact_identity().source() != self.source() {
            return Err(YtDlpCompositionError::ForeignSource);
        }
        let expected_layout = role.layout_matches();
        if selection.exact_identity().generation() == self.generation() {
            let candidate = self
                .accepted_candidates()
                .find(|candidate| {
                    candidate.descriptor() == selection.descriptor()
                        && candidate.video_color_evidence() == selection.video_color_evidence()
                })
                .ok_or(YtDlpCompositionError::ForeignGenerationOrInventory)?;
            if !self.has_equivalent_accepted_inventory_membership(candidate) {
                return Err(YtDlpCompositionError::ForeignGenerationOrInventory);
            }
            if !expected_layout(candidate.descriptor().layout()) {
                return Err(role.wrong_shape_error());
            }
            return Ok(candidate);
        }

        let mut matches = self.accepted_candidates().filter(|candidate| {
            expected_layout(candidate.descriptor().layout())
                && candidate.descriptor().semantic_identity() == selection.semantic_identity()
                && candidate.video_color_evidence() == selection.video_color_evidence()
                && self.has_equivalent_accepted_inventory_membership(candidate)
        });
        let candidate = matches.next().ok_or(role.missing_error())?;
        if matches.next().is_some() {
            return Err(role.ambiguous_error());
        }
        Ok(candidate)
    }
}

#[derive(Clone, Copy)]
enum ComponentRole {
    Video,
    Audio,
}

impl ComponentRole {
    /// Роль request material-а, которой component представлен внутри compound-а.
    const fn request_role(self) -> YtDlpCandidateComponentRole {
        match self {
            Self::Video => YtDlpCandidateComponentRole::Video,
            Self::Audio => YtDlpCandidateComponentRole::Audio,
        }
    }

    fn layout_matches(self) -> fn(&StreamLayout) -> bool {
        match self {
            Self::Video => |layout| matches!(layout, StreamLayout::VideoOnly(_)),
            Self::Audio => |layout| matches!(layout, StreamLayout::AudioOnly(_)),
        }
    }

    const fn wrong_shape_error(self) -> YtDlpCompositionError {
        match self {
            Self::Video => YtDlpCompositionError::VideoComponentRequired,
            Self::Audio => YtDlpCompositionError::AudioComponentRequired,
        }
    }

    const fn missing_error(self) -> YtDlpCompositionError {
        match self {
            Self::Video => YtDlpCompositionError::MissingVideoComponent,
            Self::Audio => YtDlpCompositionError::MissingAudioComponent,
        }
    }

    const fn ambiguous_error(self) -> YtDlpCompositionError {
        match self {
            Self::Video => YtDlpCompositionError::AmbiguousVideoComponent,
            Self::Audio => YtDlpCompositionError::AmbiguousAudioComponent,
        }
    }
}

fn build_selection(
    video: &YtDlpNormalizedCandidate,
    audio: &YtDlpNormalizedCandidate,
) -> Result<YtDlpComposedSelection, YtDlpCompositionError> {
    let descriptor = composed_descriptor(video, audio)?;
    Ok(YtDlpComposedSelection {
        descriptor,
        video: YtDlpCandidateSelection::from_candidate(video),
        audio: YtDlpCandidateSelection::from_candidate(audio),
    })
}

fn build_candidate(
    video: &YtDlpNormalizedCandidate,
    audio: &YtDlpNormalizedCandidate,
    descriptor: CandidateDescriptor,
) -> Result<YtDlpNormalizedCandidate, YtDlpCompositionError> {
    let video_request = video
        .component_request_material(YtDlpCandidateComponentRole::Video)
        .ok_or(YtDlpCompositionError::MissingComponentRequest)?;
    let audio_request = audio
        .component_request_material(YtDlpCandidateComponentRole::Audio)
        .ok_or(YtDlpCompositionError::MissingComponentRequest)?;
    Ok(YtDlpNormalizedCandidate::new(
        descriptor,
        video.video_color_evidence(),
        vec![
            (YtDlpCandidateComponentRole::Video, video_request),
            (YtDlpCandidateComponentRole::Audio, audio_request),
        ],
        audio.selection_hints(),
    ))
}

fn composed_descriptor(
    video: &YtDlpNormalizedCandidate,
    audio: &YtDlpNormalizedCandidate,
) -> Result<CandidateDescriptor, YtDlpCompositionError> {
    let StreamLayout::VideoOnly(video_component) = video.descriptor().layout() else {
        return Err(YtDlpCompositionError::VideoComponentRequired);
    };
    let StreamLayout::AudioOnly(audio_component) = audio.descriptor().layout() else {
        return Err(YtDlpCompositionError::AudioComponentRequired);
    };
    let source = video.descriptor().identity().source();
    if source != audio.descriptor().identity().source()
        || video.descriptor().identity().generation() != audio.descriptor().identity().generation()
    {
        return Err(YtDlpCompositionError::ForeignGenerationOrInventory);
    }
    let exact_key = stable_pair_key(
        b"yt-dlp-composed-exact-v1",
        video.descriptor().identity().format().as_str(),
        audio.descriptor().identity().format().as_str(),
    );
    let semantic_key = stable_pair_key(
        b"yt-dlp-composed-semantic-v1",
        video.descriptor().semantic_identity().key(),
        audio.descriptor().semantic_identity().key(),
    );
    let identity = CandidateIdentity::new(
        source,
        video.descriptor().identity().generation(),
        CandidateFormatIdentity::new(exact_key)
            .map_err(|_| YtDlpCompositionError::IdentityConstruction)?,
    );
    let semantic = SemanticIdentity::new(source, semantic_key)
        .map_err(|_| YtDlpCompositionError::IdentityConstruction)?;
    CandidateDescriptor::new(
        identity,
        semantic,
        StreamLayout::Separate {
            video: video_component.clone(),
            audio: audio_component.clone(),
        },
        Vec::new(),
    )
    .map_err(|_| YtDlpCompositionError::IdentityConstruction)
}

fn stable_pair_key(domain: &[u8], first: &str, second: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for field in [domain, first.as_bytes(), second.as_bytes()] {
        for byte in (field.len() as u64).to_le_bytes().iter().chain(field) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("cav1-{hash:016x}")
}
