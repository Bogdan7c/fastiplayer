//! Secret-free correlation для bounded HTTP resource и каждого физического request attempt-а.

use std::fmt;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Нулевое значение зарезервировано, чтобы accidental default нельзя было принять за настоящий id.
const FIRST_DIAGNOSTIC_ID: u64 = 1;

/// Шаг редких cumulative-progress событий: достаточно точный для больших media body,
/// но не превращает каждый wire chunk в отдельную запись production diagnostics.
const BODY_PROGRESS_MILESTONE_BYTES: usize = 1024 * 1024;

/// Process-local последовательность logical resource-ов.
static NEXT_RESOURCE_ID: AtomicU64 = AtomicU64::new(FIRST_DIAGNOSTIC_ID);

/// Process-local последовательность физических HTTP attempt-ов.
static NEXT_REQUEST_ATTEMPT_ID: AtomicU64 = AtomicU64::new(FIRST_DIAGNOSTIC_ID);

/// Возвращает следующий bounded process-local id без URL, hash-а либо другого secret material.
fn allocate_id(counter: &AtomicU64) -> NonZeroU64 {
    let allocated = counter.fetch_add(1, Ordering::Relaxed);
    NonZeroU64::new(allocated).unwrap_or_else(|| {
        // Практически недостижимый wrap не должен публиковать зарезервированный ноль.
        let replacement = counter.fetch_add(1, Ordering::Relaxed);
        NonZeroU64::new(replacement).unwrap_or(NonZeroU64::MIN)
    })
}

/// Secret-free identity одного logical bounded resource-а через cache, redirect и retry.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct HttpResourceCorrelationId(NonZeroU64);

impl fmt::Debug for HttpResourceCorrelationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "HttpResourceCorrelationId({})", self.0)
    }
}

impl fmt::Display for HttpResourceCorrelationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "resource-{}", self.0)
    }
}

/// Secret-free identity ровно одного физического HTTP request attempt-а.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct HttpRequestAttemptId(NonZeroU64);

impl fmt::Debug for HttpRequestAttemptId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "HttpRequestAttemptId({})", self.0)
    }
}

impl fmt::Display for HttpRequestAttemptId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "request-{}", self.0)
    }
}

/// Purpose logical resource-а остаётся стабильным между cache lookup и network attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpResourcePurpose {
    GenericMetadata,
    GenericMedia,
    Manifest,
    ClockSynchronization,
    MediaSegment,
    Initialization,
    EncryptionKey,
}

impl HttpResourcePurpose {
    const fn diagnostic_name(self) -> &'static str {
        match self {
            Self::GenericMetadata => "generic_metadata",
            Self::GenericMedia => "generic_media",
            Self::Manifest => "manifest",
            Self::ClockSynchronization => "clock_synchronization",
            Self::MediaSegment => "media_segment",
            Self::Initialization => "initialization",
            Self::EncryptionKey => "encryption_key",
        }
    }
}

/// Cache outcome не смешивает replay с физическим HTTP request-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpResourceCacheOutcome {
    Ineligible,
    Miss,
    Replay,
}

impl HttpResourceCacheOutcome {
    const fn diagnostic_name(self) -> &'static str {
        match self {
            Self::Ineligible => "ineligible",
            Self::Miss => "miss",
            Self::Replay => "replay",
        }
    }
}

/// Owner handle одного logical resource-а; не содержит locator, headers или cache key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HttpResourceDiagnostics {
    correlation_id: HttpResourceCorrelationId,
    purpose: HttpResourcePurpose,
}

impl HttpResourceDiagnostics {
    /// Создаёт новый logical resource id до cache lookup-а.
    #[must_use]
    pub fn started(purpose: HttpResourcePurpose) -> Self {
        Self {
            correlation_id: HttpResourceCorrelationId(allocate_id(&NEXT_RESOURCE_ID)),
            purpose,
        }
    }

    /// Возвращает opaque typed id для передачи через retry/redirect boundary.
    #[must_use]
    pub const fn correlation_id(self) -> HttpResourceCorrelationId {
        self.correlation_id
    }

    /// Публикует один cache lookup/replay marker без выдуманного network request id.
    pub fn record_cache_outcome(
        self,
        outcome: HttpResourceCacheOutcome,
        replay_chunks: usize,
        replay_bytes: usize,
    ) {
        tracing::debug!(
            resource_id = %self.correlation_id,
            purpose = self.purpose.diagnostic_name(),
            cache_outcome = outcome.diagnostic_name(),
            replay_chunks,
            replay_bytes,
            "Bounded HTTP resource cache outcome"
        );
    }
}

impl fmt::Debug for HttpResourceDiagnostics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpResourceDiagnostics")
            .field("correlation_id", &self.correlation_id)
            .field("purpose", &self.purpose)
            .finish()
    }
}

/// Secret-free bounded request shape для lifecycle markers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HttpRequestDiagnosticBounds {
    pub(crate) range_start: Option<u64>,
    pub(crate) requested_bytes: usize,
}

/// Typed terminal category сохраняет low-cardinality telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HttpRequestDiagnosticError {
    Timeout,
    Request,
    ResponsePolicy,
    BodyRead,
    BodyTooLarge,
    UnexpectedEof,
}

/// Typed terminal state не позволяет повторно представить один attempt как другой outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HttpRequestDiagnosticTerminal {
    Complete,
    Redirect,
    Cancelled,
    Error(HttpRequestDiagnosticError),
}

impl HttpRequestDiagnosticTerminal {
    const fn outcome_name(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Redirect => "redirect",
            Self::Cancelled => "cancelled",
            Self::Error(_) => "error",
        }
    }

    const fn error(self) -> Option<HttpRequestDiagnosticError> {
        match self {
            Self::Error(error) => Some(error),
            Self::Complete | Self::Redirect | Self::Cancelled => None,
        }
    }
}

impl HttpRequestDiagnosticError {
    const fn diagnostic_name(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Request => "request",
            Self::ResponsePolicy => "response_policy",
            Self::BodyRead => "body_read",
            Self::BodyTooLarge => "body_too_large",
            Self::UnexpectedEof => "unexpected_eof",
        }
    }
}

/// Lifecycle recorder одного физического attempt-а.
pub(crate) struct HttpRequestAttemptDiagnostics {
    attempt_id: HttpRequestAttemptId,
    resource: HttpResourceDiagnostics,
    operation: &'static str,
    bounds: HttpRequestDiagnosticBounds,
    started_at: Instant,
    first_body_chunk_recorded: bool,
    observed_body_bytes: usize,
    next_body_progress_milestone_bytes: usize,
    terminal: Option<HttpRequestDiagnosticTerminal>,
    terminal_received_body_bytes: Option<usize>,
}

impl HttpRequestAttemptDiagnostics {
    pub(crate) fn started(
        resource: HttpResourceDiagnostics,
        operation: &'static str,
        bounds: HttpRequestDiagnosticBounds,
    ) -> Self {
        let diagnostics = Self {
            attempt_id: HttpRequestAttemptId(allocate_id(&NEXT_REQUEST_ATTEMPT_ID)),
            resource,
            operation,
            bounds,
            started_at: Instant::now(),
            first_body_chunk_recorded: false,
            observed_body_bytes: 0,
            next_body_progress_milestone_bytes: BODY_PROGRESS_MILESTONE_BYTES,
            terminal: None,
            terminal_received_body_bytes: None,
        };
        tracing::debug!(
            request_id = %diagnostics.attempt_id,
            resource_id = %diagnostics.resource.correlation_id,
            purpose = diagnostics.resource.purpose.diagnostic_name(),
            operation_kind = operation,
            range_start = ?bounds.range_start,
            requested_bytes = bounds.requested_bytes,
            elapsed_milliseconds = 0_u64,
            received_body_bytes = 0_usize,
            "Source HTTP request started"
        );
        diagnostics
    }

    pub(crate) const fn attempt_id(&self) -> HttpRequestAttemptId {
        self.attempt_id
    }

    #[cfg(test)]
    pub(crate) const fn resource_id(&self) -> HttpResourceCorrelationId {
        self.resource.correlation_id
    }

    pub(crate) fn record_headers_ready(&self, status: u16) {
        tracing::debug!(
            request_id = %self.attempt_id,
            resource_id = %self.resource.correlation_id,
            purpose = self.resource.purpose.diagnostic_name(),
            operation_kind = self.operation,
            elapsed_milliseconds = elapsed_milliseconds(self.started_at),
            status,
            received_body_bytes = 0_usize,
            "Source HTTP response headers ready"
        );
    }

    /// Учитывает каждый принятый chunk, но публикует только first-byte и MiB milestones.
    pub(crate) fn record_body_chunk(&mut self, chunk_bytes: usize, received_bytes: usize) {
        if chunk_bytes == 0 || self.terminal.is_some() {
            return;
        }
        debug_assert!(received_bytes >= self.observed_body_bytes);
        self.observed_body_bytes = received_bytes;
        if !self.first_body_chunk_recorded {
            self.first_body_chunk_recorded = true;
            tracing::debug!(
                request_id = %self.attempt_id,
                resource_id = %self.resource.correlation_id,
                purpose = self.resource.purpose.diagnostic_name(),
                operation_kind = self.operation,
                elapsed_milliseconds = elapsed_milliseconds(self.started_at),
                chunk_bytes,
                received_body_bytes = received_bytes,
                "Source HTTP first non-empty body chunk ready"
            );
        }
        if received_bytes < self.next_body_progress_milestone_bytes {
            return;
        }
        let crossed_milestone_bytes = self.next_body_progress_milestone_bytes;
        self.next_body_progress_milestone_bytes = received_bytes
            .checked_div(BODY_PROGRESS_MILESTONE_BYTES)
            .and_then(|completed_milestones| completed_milestones.checked_add(1))
            .and_then(|next_milestone| next_milestone.checked_mul(BODY_PROGRESS_MILESTONE_BYTES))
            .unwrap_or(usize::MAX);
        tracing::debug!(
            request_id = %self.attempt_id,
            resource_id = %self.resource.correlation_id,
            purpose = self.resource.purpose.diagnostic_name(),
            operation_kind = self.operation,
            elapsed_milliseconds = elapsed_milliseconds(self.started_at),
            chunk_bytes,
            crossed_milestone_bytes,
            received_body_bytes = received_bytes,
            "Source HTTP body progress"
        );
    }

    pub(crate) fn record_complete(&mut self, received_bytes: usize) {
        if self.terminal.is_some() {
            return;
        }
        self.observed_body_bytes = received_bytes;
        self.terminal = Some(HttpRequestDiagnosticTerminal::Complete);
        self.terminal_received_body_bytes = Some(received_bytes);
        tracing::debug!(
            request_id = %self.attempt_id,
            resource_id = %self.resource.correlation_id,
            purpose = self.resource.purpose.diagnostic_name(),
            operation_kind = self.operation,
            elapsed_milliseconds = elapsed_milliseconds(self.started_at),
            received_body_bytes = received_bytes,
            outcome = HttpRequestDiagnosticTerminal::Complete.outcome_name(),
            "Source HTTP validated body complete"
        );
    }

    pub(crate) fn record_redirect(&mut self) {
        self.record_terminal(HttpRequestDiagnosticTerminal::Redirect, 0);
    }

    pub(crate) fn record_cancelled(&mut self) {
        self.record_terminal(
            HttpRequestDiagnosticTerminal::Cancelled,
            self.observed_body_bytes,
        );
    }

    pub(crate) fn record_error(
        &mut self,
        error: HttpRequestDiagnosticError,
        received_bytes: usize,
    ) {
        self.record_terminal(HttpRequestDiagnosticTerminal::Error(error), received_bytes);
    }

    fn record_terminal(&mut self, terminal: HttpRequestDiagnosticTerminal, received_bytes: usize) {
        if self.terminal.is_some() {
            return;
        }
        self.observed_body_bytes = received_bytes;
        self.terminal = Some(terminal);
        self.terminal_received_body_bytes = Some(received_bytes);
        tracing::debug!(
            request_id = %self.attempt_id,
            resource_id = %self.resource.correlation_id,
            purpose = self.resource.purpose.diagnostic_name(),
            operation_kind = self.operation,
            elapsed_milliseconds = elapsed_milliseconds(self.started_at),
            outcome = terminal.outcome_name(),
            error_category = terminal.error().map(HttpRequestDiagnosticError::diagnostic_name),
            received_bytes,
            range_start = ?self.bounds.range_start,
            requested_bytes = self.bounds.requested_bytes,
            "Bounded HTTP request terminal"
        );
    }
}

impl Drop for HttpRequestAttemptDiagnostics {
    fn drop(&mut self) {
        if self.terminal.is_none() {
            self.record_cancelled();
        }
    }
}

fn elapsed_milliseconds(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_attempt_ids_are_distinct_inside_one_resource() {
        let resource = HttpResourceDiagnostics::started(HttpResourcePurpose::MediaSegment);
        let first = HttpRequestAttemptDiagnostics::started(
            resource,
            "test",
            HttpRequestDiagnosticBounds {
                range_start: None,
                requested_bytes: 16,
            },
        );
        let second = HttpRequestAttemptDiagnostics::started(
            resource,
            "test",
            HttpRequestDiagnosticBounds {
                range_start: None,
                requested_bytes: 16,
            },
        );
        assert_ne!(first.attempt_id(), second.attempt_id());
        assert_eq!(first.resource_id(), second.resource_id());
    }

    #[test]
    fn debug_output_contains_only_typed_ids_and_purpose() {
        let diagnostics = HttpResourceDiagnostics::started(HttpResourcePurpose::EncryptionKey);
        let rendered = format!("{diagnostics:?}");
        assert!(rendered.contains("HttpResourceCorrelationId"));
        assert!(rendered.contains("EncryptionKey"));
        assert!(!rendered.contains("http"));
        assert!(!rendered.contains("token"));
    }

    #[test]
    fn cancellation_is_terminal_and_later_error_cannot_replace_it() {
        let resource = HttpResourceDiagnostics::started(HttpResourcePurpose::MediaSegment);
        let mut diagnostics = HttpRequestAttemptDiagnostics::started(
            resource,
            "test",
            HttpRequestDiagnosticBounds {
                range_start: None,
                requested_bytes: 16,
            },
        );
        diagnostics.record_body_chunk(4, 4);
        diagnostics.record_cancelled();
        diagnostics.record_error(HttpRequestDiagnosticError::BodyRead, 4);
        assert_eq!(
            diagnostics.terminal,
            Some(HttpRequestDiagnosticTerminal::Cancelled)
        );
        assert_eq!(diagnostics.terminal_received_body_bytes, Some(4));
    }

    #[test]
    fn body_progress_accounting_is_monotonic_and_advances_by_mib_milestones() {
        let resource = HttpResourceDiagnostics::started(HttpResourcePurpose::MediaSegment);
        let mut diagnostics = HttpRequestAttemptDiagnostics::started(
            resource,
            "test",
            HttpRequestDiagnosticBounds {
                range_start: None,
                requested_bytes: 4 * BODY_PROGRESS_MILESTONE_BYTES,
            },
        );
        diagnostics.record_body_chunk(8, 8);
        diagnostics.record_body_chunk(
            2 * BODY_PROGRESS_MILESTONE_BYTES,
            2 * BODY_PROGRESS_MILESTONE_BYTES + 8,
        );
        assert_eq!(
            diagnostics.next_body_progress_milestone_bytes,
            3 * BODY_PROGRESS_MILESTONE_BYTES
        );
        diagnostics.record_cancelled();
        assert_eq!(
            diagnostics.terminal_received_body_bytes,
            Some(2 * BODY_PROGRESS_MILESTONE_BYTES + 8)
        );
    }
}
