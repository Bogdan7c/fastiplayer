//! Process-lifetime admission boundary для destructive replacement непустой очереди.
//!
//! Модуль хранит исходный secret-bearing intent только внутри `PlaylistRuntime`.
//! UI получает отдельную immutable модель без locator-а и возвращает typed action.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::media_open::SafeMediaLabel;
use crate::playlist_runtime::PlaylistRuntime;
use crate::url_service_adapter::StartupUrlLocator;

/// Opaque identity одного confirmation intent-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueueReplacementIntentId(u64);

/// Typed reason set единственного generalized confirmation slot-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistConfirmationReasons {
    queue_replacement: bool,
    sensitive_url_persistence: bool,
    sensitive_playlist_export_locator_count: Option<usize>,
}

/// Stable presentation order composed reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistConfirmationReason {
    /// Exact durable URL identities требуют aggregated acknowledgement.
    SensitiveDurableLocatorPersistence,
    /// Export document содержит acknowledged durable identities.
    SensitivePlaylistExport { locator_count: usize },
    /// Destructive replacement подтверждается непосредственно перед commit.
    QueueReplacement,
}

impl PlaylistConfirmationReasons {
    pub(crate) const fn queue_replacement(self) -> bool {
        self.queue_replacement
    }

    pub(crate) const fn sensitive_url_persistence(self) -> bool {
        self.sensitive_url_persistence
    }

    pub(crate) const fn sensitive_playlist_export_locator_count(self) -> Option<usize> {
        self.sensitive_playlist_export_locator_count
    }

    const fn replacement_only(self) -> bool {
        self.queue_replacement
            && !self.sensitive_url_persistence
            && self.sensitive_playlist_export_locator_count.is_none()
    }

    /// Строит reason set import transaction без forgeable `confirmed: bool`.
    pub(super) const fn for_import(
        sensitive_durable_locator_persistence: bool,
        queue_replacement: bool,
    ) -> Self {
        Self {
            queue_replacement,
            sensitive_url_persistence: sensitive_durable_locator_persistence,
            sensitive_playlist_export_locator_count: None,
        }
    }

    /// Итерирует reasons в product-defined порядке: sensitive, затем replacement.
    pub(crate) fn ordered(self) -> impl Iterator<Item = PlaylistConfirmationReason> {
        [
            self.sensitive_url_persistence
                .then_some(PlaylistConfirmationReason::SensitiveDurableLocatorPersistence),
            self.sensitive_playlist_export_locator_count
                .map(
                    |locator_count| PlaylistConfirmationReason::SensitivePlaylistExport {
                        locator_count,
                    },
                ),
            self.queue_replacement
                .then_some(PlaylistConfirmationReason::QueueReplacement),
        ]
        .into_iter()
        .flatten()
    }
}

/// Session 19-safe model: только opaque ID, redacted label и typed reason set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingPlaylistConfirmation {
    intent_id: QueueReplacementIntentId,
    safe_label: SafeMediaLabel,
    reasons: PlaylistConfirmationReasons,
}

/// D15 name для того же generalized entity; отдельного race-prone slot-а нет.
pub(crate) type PendingSensitiveUrlPersistenceDecision = PendingPlaylistConfirmation;

impl PendingPlaylistConfirmation {
    pub(crate) const fn intent_id(&self) -> QueueReplacementIntentId {
        self.intent_id
    }

    pub(crate) fn safe_label(&self) -> &str {
        self.safe_label.as_str()
    }

    pub(crate) const fn reasons(&self) -> PlaylistConfirmationReasons {
        self.reasons
    }
}

/// Единственная безопасная модель, которую разрешено передавать renderer-bound UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingQueueReplacementConfirmation {
    intent_id: QueueReplacementIntentId,
    safe_label: SafeMediaLabel,
}

impl PendingQueueReplacementConfirmation {
    /// Возвращает opaque correlation identity для typed UI response.
    pub(crate) const fn intent_id(&self) -> QueueReplacementIntentId {
        self.intent_id
    }

    /// Возвращает только bounded/redacted label без исходного locator-а.
    pub(crate) fn safe_label(&self) -> &str {
        self.safe_label.as_str()
    }
}

/// Явный ответ UI; positional `bool confirmed` на boundary отсутствует.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueueReplacementConfirmationDecision {
    Confirm,
    Cancel,
}

/// Correlated UI action для единственного process-lifetime slot-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueueReplacementConfirmationAction {
    pub(crate) intent_id: QueueReplacementIntentId,
    pub(crate) decision: QueueReplacementConfirmationDecision,
}

/// Generalized exact response; decision vocabulary остаётся общей и typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistConfirmationAction {
    pub(crate) intent_id: QueueReplacementIntentId,
    pub(crate) decision: QueueReplacementConfirmationDecision,
}

/// Local intent, прошедший либо empty-queue gate, либо matching Confirm.
pub(crate) struct AdmittedLocalFileOpen {
    path: PathBuf,
}

impl AdmittedLocalFileOpen {
    /// Даёт read-only path только для уже безопасного label formatter-а до consume.
    pub(crate) fn path_for_safe_label(&self) -> &Path {
        &self.path
    }

    /// Передаёт exact native path только следующему preparation owner-у.
    pub(crate) fn into_path(self) -> PathBuf {
        self.path
    }
}

impl fmt::Debug for AdmittedLocalFileOpen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AdmittedLocalFileOpen(<redacted>)")
    }
}

/// URL intent, прошедший либо empty-queue gate, либо matching Confirm.
pub(crate) struct AdmittedUrlOpen {
    locator: StartupUrlLocator,
}

impl AdmittedUrlOpen {
    /// Передаёт typed service locator только зарегистрированному URL adapter-у.
    pub(crate) fn into_locator(self) -> StartupUrlLocator {
        self.locator
    }
}

impl fmt::Debug for AdmittedUrlOpen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AdmittedUrlOpen(<redacted>)")
    }
}

/// Единственный payload, который разрешено передать ниже confirmation boundary.
#[derive(Debug)]
pub(crate) enum AdmittedQueueReplacementIntent {
    LocalFile(AdmittedLocalFileOpen),
    ServiceUrl(AdmittedUrlOpen),
}

/// In-app intent нельзя сконструировать как trusted startup origin.
pub(crate) struct InAppQueueReplacementIntent {
    target: QueueReplacementTarget,
    safe_label: SafeMediaLabel,
}

impl InAppQueueReplacementIntent {
    /// Захватывает local path без probe/stat/open и строит generic redacted label.
    pub(crate) fn local_file(path: PathBuf) -> Self {
        let safe_label = safe_local_open_label(&path);
        Self {
            target: QueueReplacementTarget::LocalFile(path),
            safe_label,
        }
    }

    /// Захватывает typed service locator без network request и повторного URL parse-а.
    #[allow(
        dead_code,
        reason = "production in-app URL editor belongs to a later session"
    )]
    pub(crate) fn service_url(locator: StartupUrlLocator) -> Self {
        let safe_label = SafeMediaLabel::from_service_safe_label(locator.safe_label());
        Self {
            target: QueueReplacementTarget::ServiceUrl(locator),
            safe_label,
        }
    }
}

impl fmt::Debug for InAppQueueReplacementIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InAppQueueReplacementIntent")
            .field("target_kind", &self.target.kind_name())
            .field("safe_label", &self.safe_label)
            .finish()
    }
}

/// CLI/startup origin — отдельный тип, который невозможно получить через in-app response.
pub(crate) struct TrustedStartupQueueReplacementIntent {
    target: QueueReplacementTarget,
}

impl TrustedStartupQueueReplacementIntent {
    /// Создаёт trusted local startup intent после process-args adapter-а.
    pub(crate) fn local_file(path: PathBuf) -> Self {
        Self {
            target: QueueReplacementTarget::LocalFile(path),
        }
    }

    /// Создаёт trusted typed URL startup intent после service classifier-а.
    pub(crate) fn service_url(locator: StartupUrlLocator) -> Self {
        Self {
            target: QueueReplacementTarget::ServiceUrl(locator),
        }
    }
}

impl fmt::Debug for TrustedStartupQueueReplacementIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("TrustedStartupQueueReplacementIntent")
            .field(&self.target.kind_name())
            .finish()
    }
}

/// Результат in-app admission до любого media/discovery I/O.
#[derive(Debug)]
pub(crate) enum InAppQueueReplacementAdmission {
    StartNow(AdmittedQueueReplacementIntent),
    AwaitingConfirmation,
}

/// Результат correlated Confirm/Cancel response.
#[derive(Debug)]
pub(crate) enum QueueReplacementConfirmationOutcome {
    Confirmed(AdmittedQueueReplacementIntent),
    Cancelled,
    Stale,
}

/// Ошибка admission сохраняет load/lifecycle/identity причины раздельно.
#[derive(Debug, thiserror::Error)]
pub(crate) enum QueueReplacementAdmissionError {
    #[error("playlist startup replacement draft rejected the in-app open: {0}")]
    StartupDraft(#[from] crate::playlist_runtime::StartupDraftAdmissionError),
    #[error("playlist runtime no longer accepts queue replacement actions")]
    RuntimeShuttingDown,
    #[error("queue replacement confirmation identity space is exhausted")]
    IntentIdentityExhausted,
}

/// Late export completion не расширяет admission errors синхронных user intents.
#[derive(Debug, thiserror::Error)]
pub(super) enum ExportConfirmationInstallError {
    #[error("a newer playlist confirmation already occupies the authoritative slot")]
    SlotOccupied,
    #[error("playlist confirmation identity space is exhausted")]
    IntentIdentityExhausted,
}

/// Secret-bearing payload никогда не реализует automatic `Debug`/`Display`.
pub(super) enum QueueReplacementTarget {
    LocalFile(PathBuf),
    ServiceUrl(StartupUrlLocator),
}

impl QueueReplacementTarget {
    fn kind_name(&self) -> &'static str {
        match self {
            Self::LocalFile(_) => "local-file",
            Self::ServiceUrl(_) => "service-url",
        }
    }

    fn requires_sensitive_persistence_acknowledgement(&self) -> bool {
        matches!(
            self,
            Self::ServiceUrl(locator)
                if locator.requires_sensitive_persistence_acknowledgement()
        )
    }

    pub(super) fn admit(self) -> AdmittedQueueReplacementIntent {
        match self {
            Self::LocalFile(path) => {
                AdmittedQueueReplacementIntent::LocalFile(AdmittedLocalFileOpen { path })
            }
            Self::ServiceUrl(locator) => {
                AdmittedQueueReplacementIntent::ServiceUrl(AdmittedUrlOpen { locator })
            }
        }
    }
}

/// Authoritative mutable state единственного process-lifetime confirmation slot-а.
pub(super) struct QueueReplacementConfirmationState {
    next_intent_id: u64,
    pending: Option<PendingQueueReplacementIntent>,
}

struct PendingQueueReplacementIntent {
    model: PendingPlaylistConfirmation,
    target: PendingConfirmationTarget,
}

pub(super) enum PendingConfirmationTarget {
    QueueReplacement(QueueReplacementTarget),
    SensitiveUrlAppend(Box<playlist_core::PlaylistItemDraft>),
    Import(ImportConfirmationContinuation),
    Export(super::export_io::PlaylistExportConfirmationContinuation),
}

/// Exact staged import correlation, который не раскрывается UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ImportConfirmationContinuation {
    pub(super) preview_id: super::import_transaction::PlaylistImportPreviewId,
    pub(super) generation: u64,
}

pub(super) enum PlaylistConfirmationResolution {
    Confirmed(PendingConfirmationTarget),
    Cancelled,
    Stale,
}

impl QueueReplacementConfirmationState {
    pub(super) const fn new() -> Self {
        Self {
            next_intent_id: 1,
            pending: None,
        }
    }

    fn replace_with_confirmation(
        &mut self,
        intent: InAppQueueReplacementIntent,
        requires_queue_replacement: bool,
    ) -> Result<(), QueueReplacementAdmissionError> {
        // Сам факт нового explicit intent supersede-ит старый slot даже при редком
        // identity overflow: response к прежнему intent больше не должен ожить.
        self.pending = None;
        let intent_id = QueueReplacementIntentId(self.next_intent_id);
        self.next_intent_id = self
            .next_intent_id
            .checked_add(1)
            .ok_or(QueueReplacementAdmissionError::IntentIdentityExhausted)?;
        let reasons = PlaylistConfirmationReasons {
            queue_replacement: requires_queue_replacement,
            sensitive_url_persistence: intent
                .target
                .requires_sensitive_persistence_acknowledgement(),
            sensitive_playlist_export_locator_count: None,
        };
        let model = PendingPlaylistConfirmation {
            intent_id,
            safe_label: intent.safe_label,
            reasons,
        };
        self.pending = Some(PendingQueueReplacementIntent {
            model,
            target: PendingConfirmationTarget::QueueReplacement(intent.target),
        });
        Ok(())
    }

    fn replacement_only_model(&self) -> Option<PendingQueueReplacementConfirmation> {
        let pending = self.pending.as_ref()?;
        if !matches!(
            &pending.target,
            PendingConfirmationTarget::QueueReplacement(_)
        ) {
            return None;
        }
        pending
            .model
            .reasons
            .replacement_only()
            .then(|| PendingQueueReplacementConfirmation {
                intent_id: pending.model.intent_id,
                safe_label: pending.model.safe_label.clone(),
            })
    }

    fn model(&self) -> Option<PendingPlaylistConfirmation> {
        self.pending.as_ref().map(|pending| pending.model.clone())
    }

    pub(super) fn replace_with_sensitive_url_append(
        &mut self,
        safe_label: SafeMediaLabel,
        draft: playlist_core::PlaylistItemDraft,
    ) -> Result<(), QueueReplacementAdmissionError> {
        self.pending = None;
        let intent_id = QueueReplacementIntentId(self.next_intent_id);
        self.next_intent_id = self
            .next_intent_id
            .checked_add(1)
            .ok_or(QueueReplacementAdmissionError::IntentIdentityExhausted)?;
        self.pending = Some(PendingQueueReplacementIntent {
            model: PendingPlaylistConfirmation {
                intent_id,
                safe_label,
                reasons: PlaylistConfirmationReasons {
                    queue_replacement: false,
                    sensitive_url_persistence: true,
                    sensitive_playlist_export_locator_count: None,
                },
            },
            target: PendingConfirmationTarget::SensitiveUrlAppend(Box::new(draft)),
        });
        Ok(())
    }

    /// Занимает тот же authoritative slot import continuation-ом.
    pub(super) fn replace_with_import(
        &mut self,
        safe_label: SafeMediaLabel,
        reasons: PlaylistConfirmationReasons,
        continuation: ImportConfirmationContinuation,
    ) -> Result<(), QueueReplacementAdmissionError> {
        self.pending = None;
        let intent_id = QueueReplacementIntentId(self.next_intent_id);
        self.next_intent_id = self
            .next_intent_id
            .checked_add(1)
            .ok_or(QueueReplacementAdmissionError::IntentIdentityExhausted)?;
        self.pending = Some(PendingQueueReplacementIntent {
            model: PendingPlaylistConfirmation {
                intent_id,
                safe_label,
                reasons,
            },
            target: PendingConfirmationTarget::Import(continuation),
        });
        Ok(())
    }

    /// Занимает generalized slot prepared export continuation-ом без target mutation.
    pub(super) fn replace_with_export(
        &mut self,
        safe_label: SafeMediaLabel,
        locator_count: usize,
        continuation: super::export_io::PlaylistExportConfirmationContinuation,
    ) -> Result<(), ExportConfirmationInstallError> {
        // Late background completion не имеет права вытеснить более новый user intent.
        if self.pending.is_some() {
            return Err(ExportConfirmationInstallError::SlotOccupied);
        }
        let intent_id = QueueReplacementIntentId(self.next_intent_id);
        self.next_intent_id = self
            .next_intent_id
            .checked_add(1)
            .ok_or(ExportConfirmationInstallError::IntentIdentityExhausted)?;
        self.pending = Some(PendingQueueReplacementIntent {
            model: PendingPlaylistConfirmation {
                intent_id,
                safe_label,
                reasons: PlaylistConfirmationReasons {
                    queue_replacement: false,
                    sensitive_url_persistence: false,
                    sensitive_playlist_export_locator_count: Some(locator_count),
                },
            },
            target: PendingConfirmationTarget::Export(continuation),
        });
        Ok(())
    }

    fn respond(
        &mut self,
        action: QueueReplacementConfirmationAction,
    ) -> QueueReplacementConfirmationOutcome {
        let Some(pending) = self.pending.as_ref() else {
            return QueueReplacementConfirmationOutcome::Stale;
        };
        if pending.model.intent_id != action.intent_id {
            return QueueReplacementConfirmationOutcome::Stale;
        }
        if !pending.model.reasons.replacement_only() {
            return QueueReplacementConfirmationOutcome::Stale;
        }
        if !matches!(
            &pending.target,
            PendingConfirmationTarget::QueueReplacement(_)
        ) {
            return QueueReplacementConfirmationOutcome::Stale;
        }

        let pending = self
            .pending
            .take()
            .expect("matching confirmation must remain present until atomic consume");
        match action.decision {
            QueueReplacementConfirmationDecision::Confirm => {
                let PendingConfirmationTarget::QueueReplacement(target) = pending.target else {
                    unreachable!("replacement-only model always owns replacement target")
                };
                QueueReplacementConfirmationOutcome::Confirmed(target.admit())
            }
            QueueReplacementConfirmationDecision::Cancel => {
                QueueReplacementConfirmationOutcome::Cancelled
            }
        }
    }

    pub(super) fn respond_generalized(
        &mut self,
        action: PlaylistConfirmationAction,
    ) -> PlaylistConfirmationResolution {
        let Some(pending) = self.pending.as_ref() else {
            return PlaylistConfirmationResolution::Stale;
        };
        if pending.model.intent_id != action.intent_id {
            return PlaylistConfirmationResolution::Stale;
        }
        let pending = self
            .pending
            .take()
            .expect("matching generalized confirmation remains present until consume");
        match action.decision {
            QueueReplacementConfirmationDecision::Confirm => {
                PlaylistConfirmationResolution::Confirmed(pending.target)
            }
            QueueReplacementConfirmationDecision::Cancel => {
                PlaylistConfirmationResolution::Cancelled
            }
        }
    }

    pub(super) fn cancel(&mut self) {
        self.pending = None;
    }
}

impl PlaylistRuntime {
    /// Проверяет committed queue и либо выдаёт admitted token, либо сохраняет intent до Confirm.
    pub(crate) fn admit_in_app_queue_replacement(
        &mut self,
        intent: InAppQueueReplacementIntent,
    ) -> Result<InAppQueueReplacementAdmission, QueueReplacementAdmissionError> {
        self.supersede_startup_media_apply();
        self.supersede_manual_add_queue_generation();
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(QueueReplacementAdmissionError::RuntimeShuttingDown);
        }
        self.supersede_playlist_import_flow();
        let requires_sensitive_persistence_acknowledgement = intent
            .target
            .requires_sensitive_persistence_acknowledgement();
        let Some(controller) = self.controller.as_ref() else {
            // До load decision committed queue ещё отсутствует. D65 supersede-ит только
            // restore apply, после чего этот explicit open может готовиться без Item ID.
            self.record_startup_media_replacement()?;
            if requires_sensitive_persistence_acknowledgement {
                self.replacement_confirmation
                    .replace_with_confirmation(intent, false)?;
                return Ok(InAppQueueReplacementAdmission::AwaitingConfirmation);
            }
            self.replacement_confirmation.cancel();
            return Ok(InAppQueueReplacementAdmission::StartNow(
                intent.target.admit(),
            ));
        };

        if controller.queue().is_empty() {
            if requires_sensitive_persistence_acknowledgement {
                self.replacement_confirmation
                    .replace_with_confirmation(intent, false)?;
                return Ok(InAppQueueReplacementAdmission::AwaitingConfirmation);
            }
            self.replacement_confirmation.cancel();
            if self.startup_action_retention_is_active() {
                self.retain_startup_media_replacement().map_err(|error| {
                    QueueReplacementAdmissionError::StartupDraft(
                        crate::playlist_runtime::StartupDraftAdmissionError::Draft(error),
                    )
                })?;
            }
            return Ok(InAppQueueReplacementAdmission::StartNow(
                intent.target.admit(),
            ));
        }

        self.replacement_confirmation
            .replace_with_confirmation(intent, true)?;
        Ok(InAppQueueReplacementAdmission::AwaitingConfirmation)
    }

    /// Trusted startup origin bypass-ит dialog типом, а не forgeable флагом.
    pub(crate) fn admit_trusted_startup_queue_replacement(
        &mut self,
        intent: TrustedStartupQueueReplacementIntent,
    ) -> Result<AdmittedQueueReplacementIntent, QueueReplacementAdmissionError> {
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(QueueReplacementAdmissionError::RuntimeShuttingDown);
        }
        self.supersede_playlist_import_flow();
        self.replacement_confirmation.cancel();
        Ok(intent.target.admit())
    }

    /// Возвращает immutable safe model; AppState не становится authoritative owner-ом.
    pub(crate) fn pending_queue_replacement_confirmation(
        &self,
    ) -> Option<PendingQueueReplacementConfirmation> {
        self.replacement_confirmation.replacement_only_model()
    }

    /// Generalized model — единственный accessor для sensitive/composed Session 19 UI.
    pub(crate) fn pending_playlist_confirmation(&self) -> Option<PendingPlaylistConfirmation> {
        self.replacement_confirmation.model()
    }

    /// Compatibility-free typed D15 view остаётся тем же generalized entity.
    pub(crate) fn pending_sensitive_url_persistence_decision(
        &self,
    ) -> Option<PendingSensitiveUrlPersistenceDecision> {
        self.pending_playlist_confirmation()
            .filter(|model| model.reasons().sensitive_url_persistence())
    }

    /// Exact response сначала атомарно consumes slot и только затем возвращает original intent.
    pub(crate) fn respond_to_queue_replacement_confirmation(
        &mut self,
        action: QueueReplacementConfirmationAction,
    ) -> QueueReplacementConfirmationOutcome {
        let outcome = self.replacement_confirmation.respond(action);
        if matches!(outcome, QueueReplacementConfirmationOutcome::Confirmed(_))
            && self.startup_action_retention_is_active()
            && let Err(error) = self.retain_startup_media_replacement()
        {
            tracing::error!(?error, "Не удалось сохранить confirmed startup replacement");
        }
        outcome
    }

    /// Explicit Play конкретной строки supersede-ит старое replacement confirmation.
    #[allow(
        dead_code,
        reason = "playlist row UI wiring belongs to a later session"
    )]
    pub(crate) fn supersede_queue_replacement_confirmation_for_row_play(&mut self) {
        self.supersede_playlist_import_flow();
        self.replacement_confirmation.cancel();
    }

    /// Несовместимая structural replacement не оставляет response к старой queue lineage.
    pub(super) fn cancel_queue_replacement_confirmation_for_structural_replacement(&mut self) {
        self.supersede_playlist_import_flow();
        self.replacement_confirmation.cancel();
    }
}

/// Safe helper для production local logs/status без parent path.
pub(crate) fn safe_local_open_label(_path: &Path) -> SafeMediaLabel {
    // Generic label намеренно не интерпретирует native или foreign path units:
    // даже filename может быть чувствительным либо содержать чужой separator vocabulary.
    SafeMediaLabel::from_service_safe_label("локальный media-файл")
}

#[cfg(test)]
mod tests;
