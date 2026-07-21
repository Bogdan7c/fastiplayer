use std::num::NonZeroUsize;
use std::time::Duration;

use source_core::CancellationToken;

use crate::{DemuxContainerId, DemuxInputCapability, DemuxMimeType, DemuxSourceExtension};

/// Ошибка построения bounded sniff budget.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DemuxSniffBudgetError {
    /// Deadline обязан позволять хотя бы одну cooperative boundary check.
    #[error("demux sniff deadline должен быть больше нуля")]
    ZeroDeadline,
}

/// Явные верхние границы probe I/O и удерживаемой replay-памяти.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemuxSniffBudget {
    /// Максимум bytes, доступных всем factory для content detection.
    max_bytes: NonZeroUsize,
    /// Максимум segment-ов, которые registry может снять и replay-нуть.
    max_segments: NonZeroUsize,
    /// Cooperative wall-clock deadline между source read boundaries.
    max_duration: Duration,
}

impl DemuxSniffBudget {
    /// Создаёт policy только из named bounds; скрытых default literals нет.
    pub fn new(
        max_bytes: NonZeroUsize,
        max_segments: NonZeroUsize,
        max_duration: Duration,
    ) -> Result<Self, DemuxSniffBudgetError> {
        if max_duration.is_zero() {
            return Err(DemuxSniffBudgetError::ZeroDeadline);
        }
        Ok(Self {
            max_bytes,
            max_segments,
            max_duration,
        })
    }

    /// Максимум bytes, который registry может прочитать и удержать для replay.
    #[must_use]
    pub const fn max_bytes(self) -> usize {
        self.max_bytes.get()
    }

    /// Максимум segment-ов для защиты от бесконечной серии пустых chunks.
    #[must_use]
    pub const fn max_segments(self) -> usize {
        self.max_segments.get()
    }

    /// Cooperative deadline всего sniff прохода.
    #[must_use]
    pub const fn max_duration(self) -> Duration {
        self.max_duration
    }
}

/// Typed metadata hints; каждое поле независимо и может расходиться с content.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DemuxHints {
    /// Extension без ведущей точки.
    pub extension: Option<DemuxSourceExtension>,
    /// MIME type от trusted/untrusted metadata source-а.
    pub mime_type: Option<DemuxMimeType>,
    /// Уже нормализованная container identity из верхнего typed layer-а.
    pub container: Option<DemuxContainerId>,
}

impl DemuxHints {
    /// Создаёт отсутствие metadata hints.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            extension: None,
            mime_type: None,
            container: None,
        }
    }

    /// Добавляет extension без изменения остальных hint dimensions.
    #[must_use]
    pub fn with_extension(mut self, extension: DemuxSourceExtension) -> Self {
        self.extension = Some(extension);
        self
    }

    /// Добавляет MIME type без изменения остальных hint dimensions.
    #[must_use]
    pub fn with_mime_type(mut self, mime_type: DemuxMimeType) -> Self {
        self.mime_type = Some(mime_type);
        self
    }

    /// Добавляет exact neutral container identity.
    #[must_use]
    pub fn with_container(mut self, container: DemuxContainerId) -> Self {
        self.container = Some(container);
        self
    }
}

/// Связь выбранного content match-а с caller metadata hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemuxHintRelationship {
    /// Caller не передал ни одного hint-а.
    Absent,
    /// Все распознанные hints согласуются с content container-ом.
    Agrees,
    /// Хотя бы один распознанный hint указывает на другой container.
    Disagrees,
}

/// Сила content evidence; ordering используется только при выборе registry winner-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DemuxProbeConfidence {
    /// Content ещё не подтвердил container, но все переданные hints согласованы.
    HintOnly,
    /// Stable container signature подтверждена bounded prefix-ом.
    Signature,
    /// Signature и дополнительная structural проверка дали exact match.
    Exact,
}

/// Успешный factory-local probe result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemuxProbeMatch {
    /// Container identity, подтверждённая factory.
    pub container: DemuxContainerId,
    /// Сила bounded content evidence.
    pub confidence: DemuxProbeConfidence,
    /// Явная диагностика hint/content agreement.
    pub hint_relationship: DemuxHintRelationship,
}

/// Typed probe rejection, которая не смешивается с runtime demux error-ами.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DemuxProbeRejection {
    /// Caller отменил probe до terminal selection.
    #[error("demux probe отменён")]
    Cancelled,
    /// Input capability отсутствует у factory registration.
    #[error("demux factory не поддерживает input capability {capability:?}")]
    UnsupportedInput {
        /// Exact runtime input shape.
        capability: DemuxInputCapability,
    },
    /// Prefix похож на известный container, но header оборван.
    #[error(
        "container header оборван: доступно {available_bytes}, требуется минимум {required_bytes} bytes"
    )]
    Truncated {
        /// Число bytes, реально доступных probe.
        available_bytes: usize,
        /// Минимум bytes для terminal signature decision.
        required_bytes: usize,
    },
    /// Sniff I/O превысил явный wall-clock bound.
    #[error("demux sniff превысил deadline {max_duration:?}")]
    DeadlineExceeded {
        /// Configured cooperative deadline.
        max_duration: Duration,
    },
    /// Input прочитать не удалось до factory selection.
    #[error("demux sniff input failure: {reason}")]
    InputFailure {
        /// Secret-safe bounded source reason.
        reason: String,
    },
    /// Один segment сам по себе нарушает bounded replay policy.
    #[error(
        "ordered segment размером {segment_bytes} bytes превышает sniff budget {max_bytes} bytes"
    )]
    SegmentExceedsByteBudget {
        /// Exact размер полученного immutable segment-а.
        segment_bytes: usize,
        /// Configured maximum replay bytes.
        max_bytes: usize,
    },
    /// Factory распознал family, но header structurally malformed.
    #[error("malformed container header: {reason}")]
    Malformed {
        /// Secret-safe bounded parse reason.
        reason: String,
    },
}

/// Factory-local probe decision; registry отдельно решает no-match/ambiguity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DemuxProbeDecision {
    /// Factory подтвердил container с typed confidence.
    Match(DemuxProbeMatch),
    /// Prefix/hints не принадлежат этому factory.
    NoMatch,
    /// Factory узнал вход, но не может безопасно продолжить probe.
    Rejected(DemuxProbeRejection),
}

/// Immutable bounded probe view, общий для всех registered factory.
#[derive(Debug, Clone, Copy)]
pub struct DemuxProbeRequest<'request> {
    /// Caller hints, которые никогда не имеют приоритета над content signature.
    pub hints: &'request DemuxHints,
    /// Prefix длиной не больше `DemuxSniffBudget::max_bytes`.
    pub sniffed_bytes: &'request [u8],
    /// Exact input shape, который factory должен уметь открыть после probe.
    pub input_capability: DemuxInputCapability,
    /// Shared cancellation token для cheap checks внутри factory probe.
    pub cancellation: &'request CancellationToken,
}
