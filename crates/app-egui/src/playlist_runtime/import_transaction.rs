//! Process-lifetime source-neutral playlist import transaction.

mod xspf_locator_registry;

use playlist_core::{
    MAX_PLAYLIST_ITEMS, PlaylistImportEntryDraft, PlaylistImportMaterializationError,
};

use super::PlaylistRuntime;
use super::controller::{
    ControllerImportCommitError, ControllerImportCommitOutcome, ImportReplacementDisposition,
};
use super::replacement_confirmation::{
    ImportConfirmationContinuation, PlaylistConfirmationReasons, QueueReplacementAdmissionError,
};
use super::view::PlaylistStructuralRevision;

#[allow(unused_imports)]
pub(crate) use xspf_locator_registry::{
    XspfLocationAdmission, XspfLocationFallbackIssue, admit_first_xspf_location,
};

/// Hard read-model bound для source-neutral issue prefix.
const MAX_PLAYLIST_IMPORT_PREVIEW_ISSUES: usize = 128;

/// Opaque identity одного staged preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PlaylistImportPreviewId(u64);

/// Typed queue intent исключает скрытую смену append/replace semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaylistImportIntent {
    /// Добавляет accepted prefix, не меняя current/playback.
    AppendToQueue,
    /// Explicit interactive replacement с detached old playback.
    ReplaceQueue,
    /// Trusted startup/CLI replacement без destructive prompt.
    StartupReplace,
}

/// Source-neutral issue category без raw path/URL payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaylistImportIssueKind {
    /// Parser/expansion сохранил bounded non-fatal diagnostic.
    SourceRejectedEntry,
    /// App locator registry не допустил source location.
    UnsupportedLocator,
    /// Часть source diagnostics не помещается в read-model prefix.
    DiagnosticPrefixTruncated,
}

/// Один bounded issue preview; secret-bearing source text сюда не попадает.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlaylistImportIssue {
    kind: PlaylistImportIssueKind,
}

impl PlaylistImportIssue {
    /// Создаёт typed issue без произвольной строки.
    pub(crate) const fn new(kind: PlaylistImportIssueKind) -> Self {
        Self { kind }
    }

    /// Возвращает category для будущего S09 presentation mapping.
    pub(crate) const fn kind(self) -> PlaylistImportIssueKind {
        self.kind
    }
}

/// Exact/at-least vocabulary не выдумывает недоказанный rejected tail.
#[allow(
    dead_code,
    reason = "S08 tests and future parser receipts preserve exact-count vocabulary"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaylistImportRejectedCount {
    /// Parser/capacity owner доказал точное число.
    Exact(usize),
    /// Budget доказал только нижнюю границу.
    AtLeast(usize),
}

/// Source truncation до queue-capacity policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlaylistImportSourceTruncation {
    rejected_entries: PlaylistImportRejectedCount,
}

impl PlaylistImportSourceTruncation {
    /// Создаёт source-owned bounded truncation receipt.
    pub(crate) const fn new(rejected_entries: PlaylistImportRejectedCount) -> Self {
        Self { rejected_entries }
    }

    /// Возвращает доказанную точность rejected entry count.
    pub(crate) const fn rejected_entries(self) -> PlaylistImportRejectedCount {
        self.rejected_entries
    }
}

/// Exact group-safe capacity tail, который не получит IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlaylistImportCapacityTruncation {
    rejected_entries: usize,
    rejected_items: usize,
}

impl PlaylistImportCapacityTruncation {
    /// Возвращает число целых top-level entries в rejected tail.
    pub(crate) const fn rejected_entries(self) -> usize {
        self.rejected_entries
    }

    /// Возвращает retained Item ID demand rejected tail-а.
    pub(crate) const fn rejected_items(self) -> usize {
        self.rejected_items
    }
}

/// Source-neutral accepted prefix counts для preview UI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PlaylistImportEntryCounts {
    singles: usize,
    groups: usize,
    retained_items: usize,
}

impl PlaylistImportEntryCounts {
    /// Возвращает число accepted top-level Singles.
    pub(crate) const fn singles(self) -> usize {
        self.singles
    }

    /// Возвращает число accepted top-level Compound groups.
    pub(crate) const fn groups(self) -> usize {
        self.groups
    }

    /// Возвращает retained Item ID demand accepted prefix-а.
    pub(crate) const fn retained_items(self) -> usize {
        self.retained_items
    }
}

/// Caller-owned ID-less parse/service result до runtime staging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlaylistImportDraft {
    entries: Vec<PlaylistImportEntryDraft>,
    issues: Vec<PlaylistImportIssue>,
    source_truncation: Option<PlaylistImportSourceTruncation>,
    sensitive_durable_locator_count: usize,
}

impl PlaylistImportDraft {
    /// Создаёт source-neutral draft без queue/player/I/O authority.
    pub(crate) fn new(
        entries: Vec<PlaylistImportEntryDraft>,
        issues: Vec<PlaylistImportIssue>,
        source_truncation: Option<PlaylistImportSourceTruncation>,
        sensitive_durable_locator_count: usize,
    ) -> Self {
        Self {
            entries,
            issues,
            source_truncation,
            sensitive_durable_locator_count,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_summary(&self) -> (usize, usize, bool, usize) {
        (
            self.entries.len(),
            self.issues.len(),
            self.source_truncation.is_some(),
            self.sensitive_durable_locator_count,
        )
    }
}

/// Immutable read model единственного staged preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlaylistImportPreview {
    preview_id: PlaylistImportPreviewId,
    intent: PlaylistImportIntent,
    accepted: PlaylistImportEntryCounts,
    issues: Box<[PlaylistImportIssue]>,
    omitted_issue_count: usize,
    source_truncation: Option<PlaylistImportSourceTruncation>,
    capacity_truncation: Option<PlaylistImportCapacityTruncation>,
    sensitive_durable_locator_count: usize,
}

impl PlaylistImportPreview {
    /// Возвращает opaque exact identity для Continue/Cancel.
    pub(crate) const fn preview_id(&self) -> PlaylistImportPreviewId {
        self.preview_id
    }

    /// Возвращает explicit import intent.
    pub(crate) const fn intent(&self) -> PlaylistImportIntent {
        self.intent
    }

    /// Возвращает accepted source-neutral Single/Group/part counts.
    pub(crate) const fn accepted(&self) -> PlaylistImportEntryCounts {
        self.accepted
    }

    /// Возвращает bounded issue prefix.
    pub(crate) fn issues(&self) -> &[PlaylistImportIssue] {
        &self.issues
    }

    /// Возвращает число issues за пределами bounded prefix.
    pub(crate) const fn omitted_issue_count(&self) -> usize {
        self.omitted_issue_count
    }

    /// Возвращает source-owned truncation, если parser доказал её.
    pub(crate) const fn source_truncation(&self) -> Option<PlaylistImportSourceTruncation> {
        self.source_truncation
    }

    /// Возвращает exact group-safe capacity tail.
    pub(crate) const fn capacity_truncation(&self) -> Option<PlaylistImportCapacityTruncation> {
        self.capacity_truncation
    }

    /// Возвращает aggregated durable-locator acknowledgement count.
    pub(crate) const fn sensitive_durable_locator_count(&self) -> usize {
        self.sensitive_durable_locator_count
    }

    /// Partial/truncated preview требует отдельного explicit Continue.
    pub(crate) const fn requires_partial_decision(&self) -> bool {
        !self.issues.is_empty()
            || self.omitted_issue_count > 0
            || self.source_truncation.is_some()
            || self.capacity_truncation.is_some()
    }

    /// Собирает focused UI fixture без queue/controller ownership.
    #[cfg(test)]
    pub(crate) fn for_ui_test(fixture: PlaylistImportPreviewUiFixture<'_>) -> Self {
        let PlaylistImportPreviewUiFixture {
            intent,
            accepted,
            issue_kinds,
            source_rejected_at_least,
            capacity_rejected,
            sensitive_durable_locator_count,
        } = fixture;
        Self {
            preview_id: PlaylistImportPreviewId(41),
            intent,
            accepted: PlaylistImportEntryCounts {
                singles: accepted.singles,
                groups: accepted.groups,
                retained_items: accepted.retained_items,
            },
            issues: issue_kinds
                .iter()
                .copied()
                .map(PlaylistImportIssue::new)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            omitted_issue_count: 0,
            source_truncation: source_rejected_at_least.map(|count| {
                PlaylistImportSourceTruncation::new(PlaylistImportRejectedCount::AtLeast(count))
            }),
            capacity_truncation: capacity_rejected.map(|capacity| {
                PlaylistImportCapacityTruncation {
                    rejected_entries: capacity.rejected_entries,
                    rejected_items: capacity.rejected_items,
                }
            }),
            sensitive_durable_locator_count,
        }
    }
}

/// Именованные accepted counts не требуют помнить порядок трёх `usize`.
#[cfg(test)]
pub(crate) struct PlaylistImportPreviewUiAcceptedFixture {
    pub(crate) singles: usize,
    pub(crate) groups: usize,
    pub(crate) retained_items: usize,
}

/// Именованные capacity counts сохраняют различие entry и retained item.
#[cfg(test)]
pub(crate) struct PlaylistImportPreviewUiCapacityFixture {
    pub(crate) rejected_entries: usize,
    pub(crate) rejected_items: usize,
}

/// Именованный test-only input не превращает preview semantics в позиционные числа.
#[cfg(test)]
pub(crate) struct PlaylistImportPreviewUiFixture<'fixture> {
    pub(crate) intent: PlaylistImportIntent,
    pub(crate) accepted: PlaylistImportPreviewUiAcceptedFixture,
    pub(crate) issue_kinds: &'fixture [PlaylistImportIssueKind],
    pub(crate) source_rejected_at_least: Option<usize>,
    pub(crate) capacity_rejected: Option<PlaylistImportPreviewUiCapacityFixture>,
    pub(crate) sensitive_durable_locator_count: usize,
}

/// Continue либо коммитит, либо занимает generalized confirmation slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlaylistImportContinueOutcome {
    /// Sensitive/replacement reasons опубликованы одним deterministic set.
    AwaitingConfirmation,
    /// Accepted prefix committed без media open/probe.
    Committed(ControllerImportCommitOutcome),
    /// Preview/confirmation относится к superseded generation.
    Stale,
    /// Runtime больше не принимает работу.
    RuntimeClosed,
    /// Materialization отвергла недоказуемый operational locator.
    MaterializationRejected(PlaylistImportMaterializationError),
    /// Controller/domain preflight не изменил queue.
    CommitRejected(ControllerImportCommitError),
    /// Confirmation identity исчерпана без оживления старого slot-а.
    ConfirmationIdentityExhausted,
}

/// Ошибка staging до publication нового preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaylistImportStageError {
    /// Runtime/controller admission уже закрыт.
    RuntimeClosed,
    /// Startup allocator/load gate ещё не установил controller owner.
    ControllerUnavailable,
    /// Opaque preview identity исчерпана.
    PreviewIdentityExhausted,
}

/// Mutable exact transaction payload, недоступный renderer-bound UI.
struct StagedPlaylistImport {
    generation: u64,
    preview: PlaylistImportPreview,
    expected_revision: PlaylistStructuralRevision,
    accepted_entries: Vec<PlaylistImportEntryDraft>,
}

/// Process-lifetime owner одного latest-only import preview.
pub(super) struct PlaylistImportTransactionState {
    next_preview_id: u64,
    generation: u64,
    staged: Option<StagedPlaylistImport>,
}

impl PlaylistImportTransactionState {
    /// Создаёт пустой owner без allocation/queue mutation.
    pub(super) const fn new() -> Self {
        Self {
            next_preview_id: 1,
            generation: 0,
            staged: None,
        }
    }

    /// Supersede-ит staged payload и делает все старые actions stale.
    pub(super) fn cancel(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.staged = None;
    }
}

impl PlaylistRuntime {
    /// Публикует source-neutral preview и group-safe accepted prefix.
    pub(crate) fn stage_playlist_import(
        &mut self,
        intent: PlaylistImportIntent,
        draft: PlaylistImportDraft,
    ) -> Result<PlaylistImportPreview, PlaylistImportStageError> {
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(PlaylistImportStageError::RuntimeClosed);
        }
        let Some(controller) = self.controller.as_ref() else {
            return Err(PlaylistImportStageError::ControllerUnavailable);
        };
        let preview_id = PlaylistImportPreviewId(self.import_transaction.next_preview_id);
        let next_preview_id = self
            .import_transaction
            .next_preview_id
            .checked_add(1)
            .ok_or(PlaylistImportStageError::PreviewIdentityExhausted)?;
        let next_generation = self
            .import_transaction
            .generation
            .checked_add(1)
            .ok_or(PlaylistImportStageError::PreviewIdentityExhausted)?;
        let remaining_capacity = match intent {
            PlaylistImportIntent::AppendToQueue => {
                MAX_PLAYLIST_ITEMS.saturating_sub(controller.queue().retained_item_count())
            }
            PlaylistImportIntent::ReplaceQueue | PlaylistImportIntent::StartupReplace => {
                MAX_PLAYLIST_ITEMS
            }
        };
        let (accepted_entries, capacity_truncation) =
            capped_import_prefix(draft.entries, remaining_capacity);
        let accepted = count_entries(&accepted_entries);
        let omitted_issue_count = draft
            .issues
            .len()
            .saturating_sub(MAX_PLAYLIST_IMPORT_PREVIEW_ISSUES);
        let issues = draft
            .issues
            .into_iter()
            .take(MAX_PLAYLIST_IMPORT_PREVIEW_ISSUES)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let preview = PlaylistImportPreview {
            preview_id,
            intent,
            accepted,
            issues,
            omitted_issue_count,
            source_truncation: draft.source_truncation,
            capacity_truncation,
            sensitive_durable_locator_count: draft.sensitive_durable_locator_count,
        };
        let expected_revision = controller.view_snapshot().structural_revision();

        self.replacement_confirmation.cancel();
        self.import_transaction.next_preview_id = next_preview_id;
        self.import_transaction.generation = next_generation;
        self.import_transaction.staged = Some(StagedPlaylistImport {
            generation: next_generation,
            preview: preview.clone(),
            expected_revision,
            accepted_entries,
        });
        Ok(preview)
    }

    /// Возвращает immutable latest preview process-lifetime owner-а.
    pub(crate) fn pending_playlist_import_preview(&self) -> Option<PlaylistImportPreview> {
        self.import_transaction
            .staged
            .as_ref()
            .map(|staged| staged.preview.clone())
    }

    /// Explicit Continue завершает partial stage и строит composed reason set.
    pub(crate) fn continue_playlist_import(
        &mut self,
        preview_id: PlaylistImportPreviewId,
    ) -> PlaylistImportContinueOutcome {
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return PlaylistImportContinueOutcome::RuntimeClosed;
        }
        let Some(staged) = self.import_transaction.staged.as_ref() else {
            return PlaylistImportContinueOutcome::Stale;
        };
        if staged.preview.preview_id != preview_id
            || staged.generation != self.import_transaction.generation
        {
            return PlaylistImportContinueOutcome::Stale;
        }
        let Some(controller) = self.controller.as_ref() else {
            return PlaylistImportContinueOutcome::RuntimeClosed;
        };
        if controller.view_snapshot().structural_revision() != staged.expected_revision {
            self.import_transaction.cancel();
            self.replacement_confirmation.cancel();
            return PlaylistImportContinueOutcome::Stale;
        }

        let requires_sensitive_ack = staged.preview.sensitive_durable_locator_count > 0;
        let requires_replacement_ack = staged.preview.intent == PlaylistImportIntent::ReplaceQueue
            && controller.queue().retained_item_count() > 0;
        if staged.preview.intent != PlaylistImportIntent::StartupReplace
            && (requires_sensitive_ack || requires_replacement_ack)
        {
            let continuation = ImportConfirmationContinuation {
                preview_id,
                generation: staged.generation,
            };
            let reasons = PlaylistConfirmationReasons::for_import(
                requires_sensitive_ack,
                requires_replacement_ack,
            );
            return match self.replacement_confirmation.replace_with_import(
                crate::media_open::SafeMediaLabel::from_service_safe_label("Импорт плейлиста"),
                reasons,
                continuation,
            ) {
                Ok(()) => PlaylistImportContinueOutcome::AwaitingConfirmation,
                Err(QueueReplacementAdmissionError::IntentIdentityExhausted) => {
                    PlaylistImportContinueOutcome::ConfirmationIdentityExhausted
                }
                Err(
                    QueueReplacementAdmissionError::StartupDraft(_)
                    | QueueReplacementAdmissionError::RuntimeShuttingDown,
                ) => {
                    unreachable!("import slot allocation has no startup/runtime admission branch")
                }
            };
        }
        self.commit_staged_playlist_import(preview_id, staged.generation)
    }

    /// Cancel exact preview не затрагивает queue/current/playback.
    pub(crate) fn cancel_playlist_import(&mut self, preview_id: PlaylistImportPreviewId) -> bool {
        let matches = self
            .import_transaction
            .staged
            .as_ref()
            .is_some_and(|staged| staged.preview.preview_id == preview_id);
        if matches {
            self.import_transaction.cancel();
            self.replacement_confirmation.cancel();
        }
        matches
    }

    /// Matching generalized confirmation revalidates generation/revision again.
    pub(super) fn confirm_staged_playlist_import(
        &mut self,
        continuation: ImportConfirmationContinuation,
    ) -> PlaylistImportContinueOutcome {
        self.commit_staged_playlist_import(continuation.preview_id, continuation.generation)
    }

    /// Единственный terminal commit materialize-ит весь prefix до queue mutation.
    fn commit_staged_playlist_import(
        &mut self,
        preview_id: PlaylistImportPreviewId,
        generation: u64,
    ) -> PlaylistImportContinueOutcome {
        let Some(staged) = self.import_transaction.staged.take() else {
            return PlaylistImportContinueOutcome::Stale;
        };
        if staged.preview.preview_id != preview_id
            || staged.generation != generation
            || self.import_transaction.generation != generation
        {
            self.import_transaction.staged = Some(staged);
            return PlaylistImportContinueOutcome::Stale;
        }
        let queue_drafts = match staged
            .accepted_entries
            .into_iter()
            .map(PlaylistImportEntryDraft::into_queue_draft)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(drafts) => drafts,
            Err(error) => {
                self.import_transaction.cancel();
                return PlaylistImportContinueOutcome::MaterializationRejected(error);
            }
        };
        let Some(controller) = self.controller.as_mut() else {
            self.import_transaction.cancel();
            return PlaylistImportContinueOutcome::RuntimeClosed;
        };
        let outcome = match staged.preview.intent {
            PlaylistImportIntent::AppendToQueue => {
                controller.commit_import_append(staged.expected_revision, queue_drafts)
            }
            PlaylistImportIntent::ReplaceQueue => controller.commit_import_replace(
                staged.expected_revision,
                queue_drafts,
                ImportReplacementDisposition::InteractiveDetached,
            ),
            PlaylistImportIntent::StartupReplace => controller.commit_import_replace(
                staged.expected_revision,
                queue_drafts,
                ImportReplacementDisposition::Startup,
            ),
        };
        self.import_transaction.cancel();
        match outcome {
            Ok(committed) => PlaylistImportContinueOutcome::Committed(committed),
            Err(error) => PlaylistImportContinueOutcome::CommitRejected(error),
        }
    }
}

/// Вычисляет maximal whole-entry prefix; после первого overflow весь tail rejected.
fn capped_import_prefix(
    mut entries: Vec<PlaylistImportEntryDraft>,
    capacity: usize,
) -> (
    Vec<PlaylistImportEntryDraft>,
    Option<PlaylistImportCapacityTruncation>,
) {
    let mut accepted_entries = 0usize;
    let mut accepted_items = 0usize;
    for entry in &entries {
        let Some(next_items) = accepted_items.checked_add(entry.retained_item_count()) else {
            break;
        };
        if next_items > capacity {
            break;
        }
        accepted_entries += 1;
        accepted_items = next_items;
    }
    if accepted_entries == entries.len() {
        return (entries, None);
    }
    let rejected_entries = entries.len().saturating_sub(accepted_entries);
    let rejected_items = entries[accepted_entries..]
        .iter()
        .fold(0usize, |count, entry| {
            count.saturating_add(entry.retained_item_count())
        });
    entries.truncate(accepted_entries);
    (
        entries,
        Some(PlaylistImportCapacityTruncation {
            rejected_entries,
            rejected_items,
        }),
    )
}

/// Считает source-neutral preview shape без flatten cache.
fn count_entries(entries: &[PlaylistImportEntryDraft]) -> PlaylistImportEntryCounts {
    entries
        .iter()
        .fold(PlaylistImportEntryCounts::default(), |mut counts, entry| {
            match entry {
                PlaylistImportEntryDraft::Single(_) => counts.singles += 1,
                PlaylistImportEntryDraft::Compound(_) => counts.groups += 1,
            }
            counts.retained_items = counts
                .retained_items
                .saturating_add(entry.retained_item_count());
            counts
        })
}

#[cfg(test)]
mod tests;
