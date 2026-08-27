//! Correlation identity одной public scrub command внутри `PlayerSession`.
//!
//! Модуль намеренно не расширяет public `PlayerCommand` и worker channel. Session
//! остаётся единственным владельцем порядка dispatch-а и выдаёт identity до того,
//! как одна и та же команда публикуется в двух structured INFO correlation forms.

use std::num::NonZeroU64;
use std::time::Duration;

use crate::{PlayerCommand, PlayerError, PlayerErrorKind, PlayerResult, SeekRequest, SeekTarget};

/// Версия structured scrub command schema в tracing logs.
pub(super) const SCRUB_COMMAND_SCHEMA_VERSION: u64 = 1;

/// Monotonic identity одной scrub command в пределах session/process source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ScrubCommandId(NonZeroU64);

impl ScrubCommandId {
    /// Возвращает число для secret-safe structured tracing field.
    pub(super) const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Semantic scrub stage, независимый от Debug-представления public enum-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScrubCommandStage {
    Begin,
    Update,
    Preview,
    End,
}

impl ScrubCommandStage {
    /// Возвращает стабильное schema value для parser-а.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Begin => "begin",
            Self::Update => "update",
            Self::Preview => "preview",
            Self::End => "end",
        }
    }
}

/// Requested target identity до state-dependent seek resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScrubRequestedTarget {
    None,
    Absolute(Duration),
    Relative(Duration),
}

impl ScrubRequestedTarget {
    /// Строит identity из public request без чтения mutable session state.
    fn from_request(request: SeekRequest) -> Self {
        match request.target {
            SeekTarget::Absolute(target) => Self::Absolute(target.as_duration()),
            SeekTarget::Relative(step) => Self::Relative(step),
        }
    }

    /// Возвращает стабильный target kind для cross-form validation.
    pub(super) const fn kind(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Absolute(_) => "absolute",
            Self::Relative(_) => "relative",
        }
    }

    /// Возвращает target magnitude; для stages без target schema использует ноль.
    pub(super) fn milliseconds(self) -> u128 {
        match self {
            Self::None => 0,
            Self::Absolute(target) | Self::Relative(target) => target.as_millis(),
        }
    }
}

/// Correlation fields одной scrub command, общие для двух INFO markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScrubCommandCorrelation {
    id: ScrubCommandId,
    stage: ScrubCommandStage,
    requested_target: ScrubRequestedTarget,
}

impl ScrubCommandCorrelation {
    /// Возвращает monotonic command identity.
    pub(super) const fn id(self) -> ScrubCommandId {
        self.id
    }

    /// Возвращает semantic command stage.
    pub(super) const fn stage(self) -> ScrubCommandStage {
        self.stage
    }

    /// Возвращает request identity, одинаковую в обеих tracing forms.
    pub(super) const fn requested_target(self) -> ScrubRequestedTarget {
        self.requested_target
    }
}

/// Internal envelope public command-а и optional scrub correlation metadata.
#[derive(Debug)]
pub(super) struct CorrelatedPlayerCommand {
    command: PlayerCommand,
    scrub: Option<ScrubCommandCorrelation>,
}

impl CorrelatedPlayerCommand {
    /// Даёт read-only доступ к исходной public command для отдельного DEBUG receipt.
    pub(super) const fn command(&self) -> &PlayerCommand {
        &self.command
    }

    /// Возвращает correlation только для четырёх scrub variants.
    pub(super) const fn scrub(&self) -> Option<ScrubCommandCorrelation> {
        self.scrub
    }

    /// Передаёт исходную command существующей state-machine после telemetry.
    pub(super) fn into_command(self) -> PlayerCommand {
        self.command
    }
}

/// Session-owned allocator; sender и worker channel не знают про diagnostics ID.
#[derive(Debug)]
pub(super) struct ScrubCommandCorrelationRuntime {
    next_id: NonZeroU64,
}

impl ScrubCommandCorrelationRuntime {
    /// Оборачивает command и выделяет ID только для scrub variants.
    pub(super) fn correlate(
        &mut self,
        command: PlayerCommand,
    ) -> PlayerResult<CorrelatedPlayerCommand> {
        let Some((stage, requested_target)) = scrub_command_identity(&command) else {
            return Ok(CorrelatedPlayerCommand {
                command,
                scrub: None,
            });
        };

        let current_id = self.next_id;
        let next_numeric_id = current_id.get().checked_add(1).ok_or_else(|| {
            PlayerError::new(
                PlayerErrorKind::RuntimeError,
                "scrub command correlation ID space exhausted",
            )
        })?;
        self.next_id = NonZeroU64::new(next_numeric_id).ok_or_else(|| {
            PlayerError::new(
                PlayerErrorKind::RuntimeError,
                "next scrub command correlation ID became zero",
            )
        })?;

        Ok(CorrelatedPlayerCommand {
            command,
            scrub: Some(ScrubCommandCorrelation {
                id: ScrubCommandId(current_id),
                stage,
                requested_target,
            }),
        })
    }
}

impl Default for ScrubCommandCorrelationRuntime {
    fn default() -> Self {
        Self {
            next_id: NonZeroU64::MIN,
        }
    }
}

/// Извлекает semantic identity без URL, wall time или Debug parsing.
fn scrub_command_identity(
    command: &PlayerCommand,
) -> Option<(ScrubCommandStage, ScrubRequestedTarget)> {
    match command {
        PlayerCommand::BeginScrub { .. } => {
            Some((ScrubCommandStage::Begin, ScrubRequestedTarget::None))
        }
        PlayerCommand::UpdateScrub(request) => Some((
            ScrubCommandStage::Update,
            ScrubRequestedTarget::from_request(*request),
        )),
        PlayerCommand::PreviewScrub { request, .. } => Some((
            ScrubCommandStage::Preview,
            ScrubRequestedTarget::from_request(*request),
        )),
        PlayerCommand::EndScrub { .. } => {
            Some((ScrubCommandStage::End, ScrubRequestedTarget::None))
        }
        _ => None,
    }
}
