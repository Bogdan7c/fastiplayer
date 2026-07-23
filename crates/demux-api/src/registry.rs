use std::collections::{BTreeMap, BTreeSet};

use media_core::Demuxer;
use source_core::CancellationToken;

use crate::{
    DemuxContainerId, DemuxFactoryId, DemuxFixtureId, DemuxHintRelationship, DemuxHints,
    DemuxInput, DemuxInputCapabilities, DemuxInputCapability, DemuxMimeType, DemuxProbeDecision,
    DemuxProbeMatch, DemuxProbeRejection, DemuxProbeRequest, DemuxSniffBudget,
    DemuxSourceExtension,
};

mod input_replay;

use input_replay::sniff_and_restore_input;

/// Hint identities и evidence, которыми один factory владеет для container-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemuxContainerRegistration {
    /// Stable neutral container identity.
    pub container: DemuxContainerId,
    /// Exact input shapes, которые runtime factory умеет открыть для этого container-а.
    input_capabilities: DemuxInputCapabilities,
    /// Extensions без ведущей точки.
    pub extensions: Vec<DemuxSourceExtension>,
    /// Canonical MIME aliases.
    pub mime_types: Vec<DemuxMimeType>,
}

impl DemuxContainerRegistration {
    /// Создаёт registration без скрытого alias expansion.
    #[must_use]
    pub fn new(
        container: DemuxContainerId,
        input_capabilities: DemuxInputCapabilities,
        extensions: Vec<DemuxSourceExtension>,
        mime_types: Vec<DemuxMimeType>,
    ) -> Self {
        Self {
            container,
            input_capabilities,
            extensions,
            mime_types,
        }
    }

    /// Возвращает exact input shapes только текущего container registration row.
    #[must_use]
    pub const fn input_capabilities(&self) -> DemuxInputCapabilities {
        self.input_capabilities
    }

    /// Проверяет поддержку input shape без обращения к aggregate factory capability.
    #[must_use]
    pub const fn supports_input(&self, capability: DemuxInputCapability) -> bool {
        self.input_capabilities.contains(capability)
    }

    /// Сравнивает каждый присутствующий hint с exact container registration.
    #[must_use]
    pub fn hint_relationship(&self, hints: &DemuxHints) -> DemuxHintRelationship {
        let mut saw_hint = false;
        let mut disagreement = false;

        if let Some(extension) = hints.extension.as_ref() {
            saw_hint = true;
            disagreement |= !self.extensions.contains(extension);
        }
        if let Some(mime_type) = hints.mime_type.as_ref() {
            saw_hint = true;
            disagreement |= !self.mime_types.contains(mime_type);
        }
        if let Some(container) = hints.container.as_ref() {
            saw_hint = true;
            disagreement |= container != &self.container;
        }

        match (saw_hint, disagreement) {
            (false, _) => DemuxHintRelationship::Absent,
            (true, false) => DemuxHintRelationship::Agrees,
            (true, true) => DemuxHintRelationship::Disagrees,
        }
    }
}

/// Immutable self-description одного concrete demux factory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemuxFactoryDescriptor {
    /// Stable factory identity для duplicate/diagnostics.
    pub factory_id: DemuxFactoryId,
    /// Container identities и их exact metadata aliases.
    pub containers: Vec<DemuxContainerRegistration>,
    /// Focused fixture evidence, сопровождающее registration.
    pub fixture_ids: Vec<DemuxFixtureId>,
}

impl DemuxFactoryDescriptor {
    /// Собирает descriptor; полноту проверяет registry в момент регистрации.
    #[must_use]
    pub fn new(
        factory_id: DemuxFactoryId,
        containers: Vec<DemuxContainerRegistration>,
        fixture_ids: Vec<DemuxFixtureId>,
    ) -> Self {
        Self {
            factory_id,
            containers,
            fixture_ids,
        }
    }

    /// Объединяет capabilities всех rows только для быстрого factory prefilter-а.
    ///
    /// Решение о конкретной паре `(container, input)` всегда принимает registration row.
    #[must_use]
    pub fn input_capabilities(&self) -> DemuxInputCapabilities {
        self.containers.iter().fold(
            DemuxInputCapabilities::default(),
            |aggregate, registration| aggregate.union(registration.input_capabilities()),
        )
    }

    /// Возвращает exact registration одного canonical container-а.
    #[must_use]
    pub fn container_registration(
        &self,
        container: &DemuxContainerId,
    ) -> Option<&DemuxContainerRegistration> {
        self.containers
            .iter()
            .find(|registration| &registration.container == container)
    }
}

/// Factory-specific typed open rejection до публикации runtime demuxer-а.
#[derive(Debug, thiserror::Error)]
pub enum DemuxFactoryOpenError {
    /// Caller отменил open.
    #[error("demux open отменён")]
    Cancelled,
    /// Runtime input shape не поддерживается выбранным factory.
    #[error("demux factory не поддерживает input capability {capability:?}")]
    UnsupportedInput {
        /// Exact unsupported input shape.
        capability: DemuxInputCapability,
    },
    /// Factory подтвердил container, но не поддерживает конкретный open contract.
    #[error("demux open отклонён: {reason}")]
    Rejected {
        /// Bounded secret-safe причина.
        reason: String,
    },
    /// Concrete backend open error с сохранённой downcast chain.
    #[error("concrete demux backend не смог открыть input")]
    Backend(#[source] anyhow::Error),
}

/// Owned request, который получает только победивший factory.
#[derive(Debug)]
pub struct DemuxOpenRequest {
    /// Input с восстановленным sniff prefix/cursor.
    pub input: DemuxInput,
    /// Исходные caller hints для backend optimization.
    pub hints: DemuxHints,
    /// Match, которым registry обосновал выбор factory.
    pub selected_probe: DemuxProbeMatch,
    /// Shared cooperative cancellation token.
    pub cancellation: CancellationToken,
}

/// Concrete adapter boundary, регистрируемый в neutral registry.
pub trait DemuxFactory: Send + Sync {
    /// Возвращает immutable registration descriptor.
    fn descriptor(&self) -> &DemuxFactoryDescriptor;

    /// Классифицирует общий bounded prefix без дополнительного I/O.
    fn probe(&self, request: DemuxProbeRequest<'_>) -> DemuxProbeDecision;

    /// Открывает owned input и возвращает existing runtime boundary.
    fn open(
        &self,
        request: DemuxOpenRequest,
    ) -> Result<Box<dyn Demuxer + Send>, DemuxFactoryOpenError>;
}

/// Ошибка изменения registry composition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DemuxRegistryError {
    /// Factory ID уже занят и не может менять meaning по порядку регистрации.
    #[error("demux factory `{factory_id}` уже зарегистрирован")]
    DuplicateFactory {
        /// Exact duplicate factory identity.
        factory_id: DemuxFactoryId,
    },
    /// Один canonical container не может иметь двух неявных owners.
    #[error("demux container `{container}` уже принадлежит factory `{existing_factory_id}`")]
    DuplicateContainer {
        /// Exact duplicate container identity.
        container: DemuxContainerId,
        /// Ранее зарегистрированный owner.
        existing_factory_id: DemuxFactoryId,
    },
    /// Container row без input capability никогда не может быть выбран честно.
    #[error(
        "demux factory `{factory_id}` не объявил input capabilities для container `{container}`"
    )]
    MissingInputCapabilities {
        /// Invalid factory identity.
        factory_id: DemuxFactoryId,
        /// Invalid container registration.
        container: DemuxContainerId,
    },
    /// Factory без container registration не имеет typed selection evidence.
    #[error("demux factory `{factory_id}` не объявил containers")]
    MissingContainers {
        /// Invalid factory identity.
        factory_id: DemuxFactoryId,
    },
    /// Fixture IDs обязательны как regression evidence registration-а.
    #[error("demux factory `{factory_id}` не объявил fixture IDs")]
    MissingFixtureIds {
        /// Invalid factory identity.
        factory_id: DemuxFactoryId,
    },
}

/// Public probe result без выдачи concrete factory handle наружу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemuxProbeSelection {
    /// Stable identity выбранного factory.
    pub factory_id: DemuxFactoryId,
    /// Exact container/confidence/hint relationship.
    pub matched: DemuxProbeMatch,
}

/// Результат content-proven open с exact container identity выбранного factory.
pub struct DemuxProbedOpen {
    demuxer: Box<dyn Demuxer + Send>,
    container: DemuxContainerId,
}

impl DemuxProbedOpen {
    /// Возвращает container, доказанный bounded content sniff-ом.
    #[must_use]
    pub const fn container(&self) -> &DemuxContainerId {
        &self.container
    }

    /// Передаёт открытый demuxer вызывающему owner-у.
    #[must_use]
    pub fn into_demuxer(self) -> Box<dyn Demuxer + Send> {
        self.demuxer
    }
}

/// Typed registry probe/open failure.
#[derive(Debug, thiserror::Error)]
pub enum DemuxOpenError {
    /// Registry не нашёл ни одного content/hint match-а.
    #[error("ни один зарегистрированный demux factory не распознал input")]
    NoMatch,
    /// Несколько factory дали одинаково сильное evidence.
    #[error("demux probe неоднозначен между factory {factory_ids:?}")]
    AmbiguousMatch {
        /// Stable identities всех tied winners.
        factory_ids: Vec<DemuxFactoryId>,
    },
    /// Probe узнал input, но terminal typed rejection запрещает open.
    #[error("demux probe отклонён")]
    ProbeRejected(#[source] DemuxProbeRejection),
    /// Content sniff доказал другой container, чем требовал concrete manifest owner.
    #[error("demux content container не совпадает с required container")]
    UnexpectedContainer {
        /// Required intent после manifest/profile validation.
        expected: DemuxContainerId,
        /// Фактически доказанный content sniff-ом container.
        matched: DemuxContainerId,
    },
    /// Выбранный factory не смог открыть восстановленный input.
    #[error("demux factory `{factory_id}` отклонил open")]
    FactoryRejected {
        /// Stable selected factory identity.
        factory_id: DemuxFactoryId,
        /// Typed concrete open rejection.
        #[source]
        source: DemuxFactoryOpenError,
    },
}

/// Process-local deterministic demux factory registry.
#[derive(Default)]
pub struct DemuxRegistry {
    /// Registration order используется только как стабильный iteration order, не tie-break.
    factories: Vec<Box<dyn DemuxFactory>>,
}

impl DemuxRegistry {
    /// Создаёт пустой registry для composition root-а.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            factories: Vec::new(),
        }
    }

    /// Добавляет factory после проверки owner uniqueness и evidence completeness.
    pub fn register(&mut self, factory: Box<dyn DemuxFactory>) -> Result<(), DemuxRegistryError> {
        let descriptor = factory.descriptor();
        if descriptor.containers.is_empty() {
            return Err(DemuxRegistryError::MissingContainers {
                factory_id: descriptor.factory_id.clone(),
            });
        }
        if let Some(registration) = descriptor
            .containers
            .iter()
            .find(|registration| registration.input_capabilities().is_empty())
        {
            return Err(DemuxRegistryError::MissingInputCapabilities {
                factory_id: descriptor.factory_id.clone(),
                container: registration.container.clone(),
            });
        }
        if descriptor.fixture_ids.is_empty() {
            return Err(DemuxRegistryError::MissingFixtureIds {
                factory_id: descriptor.factory_id.clone(),
            });
        }
        if self
            .factories
            .iter()
            .any(|registered| registered.descriptor().factory_id == descriptor.factory_id)
        {
            return Err(DemuxRegistryError::DuplicateFactory {
                factory_id: descriptor.factory_id.clone(),
            });
        }

        let mut declared_containers = BTreeSet::new();
        for registration in &descriptor.containers {
            if !declared_containers.insert(&registration.container) {
                return Err(DemuxRegistryError::DuplicateContainer {
                    container: registration.container.clone(),
                    existing_factory_id: descriptor.factory_id.clone(),
                });
            }
        }

        let existing_containers = self
            .factories
            .iter()
            .flat_map(|registered| {
                registered
                    .descriptor()
                    .containers
                    .iter()
                    .map(move |container| {
                        (&container.container, &registered.descriptor().factory_id)
                    })
            })
            .collect::<BTreeMap<_, _>>();
        for container in &descriptor.containers {
            if let Some(existing_factory_id) = existing_containers.get(&container.container) {
                return Err(DemuxRegistryError::DuplicateContainer {
                    container: container.container.clone(),
                    existing_factory_id: (*existing_factory_id).clone(),
                });
            }
        }

        self.factories.push(factory);
        Ok(())
    }

    /// Probes уже bounded sample без I/O; полезно для capability planning и tests.
    pub fn probe_sample(
        &self,
        input_capability: DemuxInputCapability,
        hints: &DemuxHints,
        sniffed_bytes: &[u8],
        cancellation: &CancellationToken,
    ) -> Result<DemuxProbeSelection, DemuxOpenError> {
        let selected = self.select_factory(input_capability, hints, sniffed_bytes, cancellation)?;
        Ok(DemuxProbeSelection {
            factory_id: self.factories[selected.factory_index]
                .descriptor()
                .factory_id
                .clone(),
            matched: selected.matched,
        })
    }

    /// Выполняет bounded sniff, восстанавливает input и открывает selected factory.
    pub fn open(
        &self,
        input: DemuxInput,
        hints: DemuxHints,
        sniff_budget: DemuxSniffBudget,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn Demuxer + Send>, DemuxOpenError> {
        self.open_with_optional_container(input, hints, sniff_budget, cancellation, None)
            .map(DemuxProbedOpen::into_demuxer)
    }

    /// Открывает input и возвращает exact container identity из того же content probe.
    pub fn open_probed(
        &self,
        input: DemuxInput,
        hints: DemuxHints,
        sniff_budget: DemuxSniffBudget,
        cancellation: CancellationToken,
    ) -> Result<DemuxProbedOpen, DemuxOpenError> {
        self.open_with_optional_container(input, hints, sniff_budget, cancellation, None)
    }

    /// Открывает input только если content sniff доказал exact required container.
    ///
    /// Manifest owner использует этот intent-boundary вместо extension/MIME guess-а. Existing
    /// generic `open` сохраняет прежнюю selection semantics.
    pub fn open_required_container(
        &self,
        input: DemuxInput,
        hints: DemuxHints,
        sniff_budget: DemuxSniffBudget,
        cancellation: CancellationToken,
        required_container: DemuxContainerId,
    ) -> Result<Box<dyn Demuxer + Send>, DemuxOpenError> {
        self.open_with_optional_container(
            input,
            hints,
            sniff_budget,
            cancellation,
            Some(required_container),
        )
        .map(DemuxProbedOpen::into_demuxer)
    }

    fn open_with_optional_container(
        &self,
        input: DemuxInput,
        hints: DemuxHints,
        sniff_budget: DemuxSniffBudget,
        cancellation: CancellationToken,
        required_container: Option<DemuxContainerId>,
    ) -> Result<DemuxProbedOpen, DemuxOpenError> {
        ensure_active(&cancellation)?;
        let input_capability = input.capability();
        let (restored_input, sniffed_bytes) =
            sniff_and_restore_input(input, sniff_budget, &cancellation)?;
        let selected =
            self.select_factory(input_capability, &hints, &sniffed_bytes, &cancellation)?;
        if let Some(expected) = required_container
            && selected.matched.container != expected
        {
            return Err(DemuxOpenError::UnexpectedContainer {
                expected,
                matched: selected.matched.container.clone(),
            });
        }
        let factory = &self.factories[selected.factory_index];
        let factory_id = factory.descriptor().factory_id.clone();
        let container = selected.matched.container.clone();
        let demuxer = factory
            .open(DemuxOpenRequest {
                input: restored_input,
                hints,
                selected_probe: selected.matched,
                cancellation,
            })
            .map_err(|source| DemuxOpenError::FactoryRejected { factory_id, source })?;
        Ok(DemuxProbedOpen { demuxer, container })
    }

    /// Выбирает unique strongest probe match без order-based fallback-а.
    fn select_factory(
        &self,
        input_capability: DemuxInputCapability,
        hints: &DemuxHints,
        sniffed_bytes: &[u8],
        cancellation: &CancellationToken,
    ) -> Result<SelectedFactory, DemuxOpenError> {
        ensure_active(cancellation)?;
        let mut matches = Vec::new();
        let mut strongest_rejection = None;

        for (factory_index, factory) in self.factories.iter().enumerate() {
            let descriptor = factory.descriptor();
            if !descriptor.input_capabilities().contains(input_capability) {
                continue;
            }
            let decision = factory.probe(DemuxProbeRequest {
                hints,
                sniffed_bytes,
                input_capability,
                cancellation,
            });
            match decision {
                DemuxProbeDecision::Match(matched) => {
                    match descriptor.container_registration(&matched.container) {
                        Some(registration) if registration.supports_input(input_capability) => {
                            matches.push(SelectedFactory {
                                factory_index,
                                matched,
                            });
                        }
                        Some(_) => {
                            strongest_rejection = choose_probe_rejection(
                                strongest_rejection,
                                DemuxProbeRejection::UnsupportedInput {
                                    capability: input_capability,
                                },
                            );
                        }
                        None => {
                            strongest_rejection = choose_probe_rejection(
                                strongest_rejection,
                                DemuxProbeRejection::Malformed {
                                    reason: "demux factory matched незарегистрированный container"
                                        .to_owned(),
                                },
                            );
                        }
                    }
                }
                DemuxProbeDecision::NoMatch => {}
                DemuxProbeDecision::Rejected(rejection) => {
                    strongest_rejection = choose_probe_rejection(strongest_rejection, rejection);
                }
            }
        }

        let Some(strongest_confidence) = matches
            .iter()
            .map(|selected| selected.matched.confidence)
            .max()
        else {
            return strongest_rejection.map_or(Err(DemuxOpenError::NoMatch), |rejection| {
                Err(DemuxOpenError::ProbeRejected(rejection))
            });
        };
        let mut strongest_matches = matches
            .into_iter()
            .filter(|selected| selected.matched.confidence == strongest_confidence);
        let winner = strongest_matches
            .next()
            .expect("non-empty strongest matches");
        let tied_factory_indices = strongest_matches
            .map(|selected| selected.factory_index)
            .collect::<Vec<_>>();
        if tied_factory_indices.is_empty() {
            return Ok(winner);
        }

        let mut factory_ids = vec![
            self.factories[winner.factory_index]
                .descriptor()
                .factory_id
                .clone(),
        ];
        factory_ids.extend(tied_factory_indices.into_iter().map(|factory_index| {
            self.factories[factory_index]
                .descriptor()
                .factory_id
                .clone()
        }));
        Err(DemuxOpenError::AmbiguousMatch { factory_ids })
    }
}

/// Internal selection сохраняет factory index только до немедленного open-а.
struct SelectedFactory {
    /// Index immutable registry row.
    factory_index: usize,
    /// Typed match выбранного factory.
    matched: DemuxProbeMatch,
}

/// Cancellation имеет приоритет над любым content result.
fn ensure_active(cancellation: &CancellationToken) -> Result<(), DemuxOpenError> {
    if cancellation.is_cancelled() {
        Err(DemuxOpenError::ProbeRejected(
            DemuxProbeRejection::Cancelled,
        ))
    } else {
        Ok(())
    }
}

/// Выбирает наиболее конкретную terminal rejection при отсутствии match-а.
fn choose_probe_rejection(
    current: Option<DemuxProbeRejection>,
    candidate: DemuxProbeRejection,
) -> Option<DemuxProbeRejection> {
    let priority = |rejection: &DemuxProbeRejection| match rejection {
        DemuxProbeRejection::Cancelled => 6,
        DemuxProbeRejection::Malformed { .. } => 5,
        DemuxProbeRejection::Truncated { .. } => 4,
        DemuxProbeRejection::SegmentExceedsByteBudget { .. } => 3,
        DemuxProbeRejection::DeadlineExceeded { .. } => 2,
        DemuxProbeRejection::InputFailure { .. } => 1,
        DemuxProbeRejection::UnsupportedInput { .. } => 0,
    };
    match current {
        Some(current) if priority(&current) >= priority(&candidate) => Some(current),
        _ => Some(candidate),
    }
}

#[cfg(test)]
mod tests;
