//! Dependency-injected boundary запуска внешнего extractor process.
//!
//! Модуль сообщает test/diagnostic launcher-у пользовательскую причину и
//! внутреннюю фазу каждого spawn attempt-а, но не передаёт ему ownership
//! lifecycle. Возвращённый `Child` немедленно оборачивается process-tree owner-ом.

use std::io;
use std::process::{Child, Command};
use std::sync::Arc;

use web_media_core::ExtractorInvocationReason;

/// Внутренняя фаза subprocess-а внутри одной пользовательской extraction.
///
/// Phase дополняет [`ExtractorInvocationReason`], а не заменяет его: primary,
/// recovery page capture и recovered embed сохраняют исходный product intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtractorProcessPhase {
    /// Primary single-item `--dump-single-json` candidate extraction.
    CandidatePrimary,
    /// Primary collection/topology extraction через bounded lazy JSON lines.
    TopologyPrimary,
    /// Recovery capture исходной HTML page через `--write-pages`.
    RecoveryPageCapture,
    /// Проверка одного найденного non-platform embed candidate-а.
    RecoveryEmbedCandidate,
}

/// Secret-free описание одного фактического process spawn attempt-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExtractorProcessInvocation {
    /// Пользовательская причина обращения к extractor adapter-у.
    reason: ExtractorInvocationReason,
    /// Конкретная subprocess phase внутри этой extraction.
    phase: ExtractorProcessPhase,
}

impl ExtractorProcessInvocation {
    /// Собирает internal event без URL, argv или request material.
    pub(crate) const fn new(
        reason: ExtractorInvocationReason,
        phase: ExtractorProcessPhase,
    ) -> Self {
        Self { reason, phase }
    }

    /// Возвращает неизменённую пользовательскую причину extraction.
    #[must_use]
    pub const fn reason(self) -> ExtractorInvocationReason {
        self.reason
    }

    /// Возвращает typed internal subprocess phase.
    #[must_use]
    pub const fn phase(self) -> ExtractorProcessPhase {
        self.phase
    }
}

/// Injected launcher, через который проходит каждый extractor spawn attempt.
///
/// Реализация может наблюдать typed invocation или добавлять hermetic process
/// environment. Она не получает `OwnedProcess`: kill/reap/process-group и pipe
/// lifecycle остаются исключительной обязанностью `service-ytdlp`.
pub trait ExtractorProcessLauncher: Send + Sync {
    /// Запускает уже полностью сконфигурированный command.
    ///
    /// На Unix process owner до этого вызова уже установил отдельную process
    /// group. Реализация обязана запускать именно переданный `Command`, чтобы не
    /// потерять эту lifecycle-конфигурацию.
    fn spawn(
        &self,
        command: &mut Command,
        invocation: ExtractorProcessInvocation,
    ) -> io::Result<Child>;
}

/// Production launcher без изменяемого глобального состояния.
#[derive(Debug, Default)]
struct SystemExtractorProcessLauncher;

impl ExtractorProcessLauncher for SystemExtractorProcessLauncher {
    fn spawn(
        &self,
        command: &mut Command,
        _invocation: ExtractorProcessInvocation,
    ) -> io::Result<Child> {
        command.spawn()
    }
}

/// Узкая injected façade над candidate/topology/metadata extractor entrypoints.
#[derive(Clone)]
pub struct YtDlpExtractorAdapter {
    /// Единственный launcher текущего adapter instance-а.
    process_launcher: Arc<dyn ExtractorProcessLauncher>,
}

impl YtDlpExtractorAdapter {
    /// Создаёт adapter с caller-owned launcher/spy без global test hook-а.
    #[must_use]
    pub fn with_process_launcher(process_launcher: Arc<dyn ExtractorProcessLauncher>) -> Self {
        Self { process_launcher }
    }

    /// Возвращает launcher только внутреннему process owner-у.
    pub(crate) fn process_launcher(&self) -> Arc<dyn ExtractorProcessLauncher> {
        Arc::clone(&self.process_launcher)
    }

    /// Извлекает candidate snapshot с обязательной typed product reason.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_candidate_snapshot_with_cancellation(
        &self,
        locator: &crate::YtDlpMediaLocator,
        source: web_media_core::SourceIdentity,
        generation: web_media_core::ExtractionGeneration,
        yt_dlp_config: &rustiplayer_config::YtDlpConfig,
        invocation_reason: ExtractorInvocationReason,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<crate::YtDlpCandidateSnapshot, crate::YtDlpServiceError> {
        crate::candidate::resolve_candidate_snapshot_with_adapter(
            self,
            locator,
            source,
            generation,
            yt_dlp_config,
            invocation_reason,
            is_cancelled,
        )
    }

    /// Извлекает bounded topology с explicit product reason.
    pub fn extract_topology_with_budgets(
        &self,
        locator: &crate::YtDlpMediaLocator,
        yt_dlp_config: &rustiplayer_config::YtDlpConfig,
        budgets: crate::YtDlpTopologyBudgets,
        invocation_reason: ExtractorInvocationReason,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<crate::YtDlpTopology, crate::YtDlpTopologyError> {
        crate::topology::extract_topology_with_adapter_budgets(
            self,
            locator,
            yt_dlp_config,
            budgets,
            invocation_reason,
            is_cancelled,
        )
    }

    /// Извлекает single-item metadata с explicit product reason.
    pub fn resolve_playlist_metadata_with_cancellation(
        &self,
        locator: &crate::YtDlpMediaLocator,
        yt_dlp_config: &rustiplayer_config::YtDlpConfig,
        invocation_reason: ExtractorInvocationReason,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<crate::YtDlpPlaylistMetadata, crate::YtDlpServiceError> {
        crate::metadata::resolve_playlist_metadata_with_adapter(
            self,
            locator,
            yt_dlp_config,
            invocation_reason,
            is_cancelled,
        )
    }
}

impl Default for YtDlpExtractorAdapter {
    fn default() -> Self {
        Self {
            process_launcher: Arc::new(SystemExtractorProcessLauncher),
        }
    }
}

impl std::fmt::Debug for YtDlpExtractorAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("YtDlpExtractorAdapter")
            .field("process_launcher", &"<injected>")
            .finish()
    }
}

#[cfg(all(test, unix))]
mod tests;
