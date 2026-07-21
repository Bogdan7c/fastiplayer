use audio_core::AudioDecodeCapabilitySnapshot;
use capability_core::SystemCapabilities;
use demux_api::DemuxInputCapabilities;
use web_media_core::{ContainerFamily, TransportFamily};

/// Одна registration transport family и всех neutral input shapes на её выходе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportCapabilityRegistration {
    /// Static transport family, которую умеет хотя бы один зарегистрированный provider.
    family: TransportFamily,
    /// Возможные формы transport output без выбора concrete runtime path.
    output_inputs: DemuxInputCapabilities,
}

impl TransportCapabilityRegistration {
    /// Создаёт registration только с непустым набором output shapes.
    pub fn new(
        family: TransportFamily,
        output_inputs: DemuxInputCapabilities,
    ) -> Result<Self, CapabilitySnapshotBuildError> {
        if output_inputs.is_empty() {
            return Err(CapabilitySnapshotBuildError::EmptyTransportOutputs { family });
        }

        Ok(Self {
            family,
            output_inputs,
        })
    }

    /// Возвращает зарегистрированную transport family.
    pub const fn family(self) -> TransportFamily {
        self.family
    }

    /// Возвращает возможные neutral output shapes provider-а.
    pub const fn output_inputs(self) -> DemuxInputCapabilities {
        self.output_inputs
    }
}

/// Immutable snapshot всех зарегистрированных transport capabilities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransportCapabilitySnapshot {
    /// Registrations сохраняются value-owned и не удерживают provider objects.
    registrations: Box<[TransportCapabilityRegistration]>,
}

impl TransportCapabilitySnapshot {
    /// Создаёт snapshot; пустой snapshot означает отсутствие transport provider-ов.
    #[must_use]
    pub fn new(registrations: Vec<TransportCapabilityRegistration>) -> Self {
        Self {
            registrations: registrations.into_boxed_slice(),
        }
    }

    /// Возвращает immutable registrations для diagnostics/composition tests.
    pub const fn registrations(&self) -> &[TransportCapabilityRegistration] {
        &self.registrations
    }

    /// Объединяет output shapes всех provider registrations одной family.
    #[must_use]
    pub fn output_inputs_for(&self, family: TransportFamily) -> DemuxInputCapabilities {
        self.registrations
            .iter()
            .filter(|registration| registration.family == family)
            .fold(DemuxInputCapabilities::NONE, |inputs, registration| {
                inputs.union(registration.output_inputs)
            })
    }
}

/// Одна registration container family и поддержанных input shapes demuxer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemuxCapabilityRegistration {
    /// Container family, доказанная concrete demux registration-ом.
    container: ContainerFamily,
    /// Формы input, которые container demuxer реально умеет читать.
    input_capabilities: DemuxInputCapabilities,
}

impl DemuxCapabilityRegistration {
    /// Создаёт registration только с непустым набором input shapes.
    pub fn new(
        container: ContainerFamily,
        input_capabilities: DemuxInputCapabilities,
    ) -> Result<Self, CapabilitySnapshotBuildError> {
        if input_capabilities.is_empty() {
            return Err(CapabilitySnapshotBuildError::EmptyDemuxInputs { container });
        }

        Ok(Self {
            container,
            input_capabilities,
        })
    }

    /// Возвращает зарегистрированную container family.
    pub const fn container(self) -> ContainerFamily {
        self.container
    }

    /// Возвращает поддержанные demux input shapes.
    pub const fn input_capabilities(self) -> DemuxInputCapabilities {
        self.input_capabilities
    }
}

/// Immutable snapshot всех зарегистрированных demux capabilities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DemuxCapabilitySnapshot {
    /// Registrations не удерживают factory или probe/open state.
    registrations: Box<[DemuxCapabilityRegistration]>,
}

impl DemuxCapabilitySnapshot {
    /// Создаёт snapshot; пустой snapshot означает отсутствие demux factory registrations.
    #[must_use]
    pub fn new(registrations: Vec<DemuxCapabilityRegistration>) -> Self {
        Self {
            registrations: registrations.into_boxed_slice(),
        }
    }

    /// Возвращает immutable registrations для diagnostics/composition tests.
    pub const fn registrations(&self) -> &[DemuxCapabilityRegistration] {
        &self.registrations
    }

    /// Объединяет input shapes всех demux registrations одной container family.
    #[must_use]
    pub fn input_capabilities_for(&self, container: ContainerFamily) -> DemuxInputCapabilities {
        self.registrations
            .iter()
            .filter(|registration| registration.container == container)
            .fold(DemuxInputCapabilities::NONE, |inputs, registration| {
                inputs.union(registration.input_capabilities)
            })
    }
}

/// Read-only composition всех capability layers, нужных pure planner-у.
#[derive(Debug, Clone, Copy)]
pub struct PlaybackCapabilitySnapshot<'snapshot> {
    /// Зарегистрированные transport capabilities.
    transport: &'snapshot TransportCapabilitySnapshot,
    /// Зарегистрированные demux capabilities.
    demux: &'snapshot DemuxCapabilitySnapshot,
    /// Existing system-level video decoder/renderer intersection.
    video: &'snapshot SystemCapabilities,
    /// S20 read-only audio decoder snapshot.
    audio: AudioDecodeCapabilitySnapshot,
}

impl<'snapshot> PlaybackCapabilitySnapshot<'snapshot> {
    /// Связывает четыре immutable snapshots без копирования runtime owners.
    #[must_use]
    pub const fn new(
        transport: &'snapshot TransportCapabilitySnapshot,
        demux: &'snapshot DemuxCapabilitySnapshot,
        video: &'snapshot SystemCapabilities,
        audio: AudioDecodeCapabilitySnapshot,
    ) -> Self {
        Self {
            transport,
            demux,
            video,
            audio,
        }
    }

    /// Возвращает immutable transport snapshot.
    pub const fn transport(self) -> &'snapshot TransportCapabilitySnapshot {
        self.transport
    }

    /// Возвращает immutable demux snapshot.
    pub const fn demux(self) -> &'snapshot DemuxCapabilitySnapshot {
        self.demux
    }

    /// Возвращает existing system video snapshot.
    pub const fn video(self) -> &'snapshot SystemCapabilities {
        self.video
    }

    /// Возвращает S20 audio snapshot по значению.
    pub const fn audio(self) -> AudioDecodeCapabilitySnapshot {
        self.audio
    }
}

/// Ошибка формирования capability registration до запуска planner-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySnapshotBuildError {
    /// Transport registration не объявил ни одной output shape.
    EmptyTransportOutputs {
        /// Проблемная transport family.
        family: TransportFamily,
    },
    /// Demux registration не объявил ни одной input shape.
    EmptyDemuxInputs {
        /// Проблемная container family.
        container: ContainerFamily,
    },
}

impl std::fmt::Display for CapabilitySnapshotBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyTransportOutputs { family } => {
                write!(formatter, "transport {family:?} не объявил output shapes")
            }
            Self::EmptyDemuxInputs { container } => {
                write!(formatter, "demux {container:?} не объявил input shapes")
            }
        }
    }
}

impl std::error::Error for CapabilitySnapshotBuildError {}
