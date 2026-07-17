//! Debounced persistence app-owned ширины sidebar.
//!
//! Live geometry меняется в `SidebarHostState` немедленно. Этот модуль владеет
//! только latest-only quiet-period и одно-полевым settings transaction.

use super::*;
use crate::ui::sidebar::{SidebarWidthChange, SidebarWidthPoints};

/// Quiet-period после последнего отличающегося округлённого drag значения.
pub(super) const SIDEBAR_RESIZE_PERSIST_DEBOUNCE: Duration = Duration::from_millis(500);

/// Stable setting id единственного runtime-originated sidebar resize commit-а.
pub(super) const SIDEBAR_WIDTH_SETTING_ID: &str = "ui.sidebar.width_points";

/// Последний resize intent, который ещё не дошёл до atomic config persistence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PendingSidebarResize {
    /// Округлённое значение config boundary.
    width_points: SidebarWidthPoints,

    /// Монотонный deadline quiet-period.
    persist_at: Instant,
}

/// Результат попытки flush без смешения «нечего делать» и transaction failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarResizeFlushOutcome {
    /// Pending intent отсутствует либо его deadline ещё не наступил.
    NoPending,
    /// Commit завершился success/no-op.
    Succeeded,
    /// Transaction report содержит typed failure; rollback уже выполнен.
    Failed,
}

impl SidebarResizeFlushOutcome {
    /// Любая завершённая попытка меняет owner/status state и требует visual refresh.
    #[must_use]
    pub(crate) fn needs_redraw(self) -> bool {
        self != Self::NoPending
    }

    /// Apply/OK может продолжаться только после успешной синхронизации pending resize.
    #[must_use]
    pub(crate) fn allows_followup_apply(self) -> bool {
        self != Self::Failed
    }
}

impl SettingsRuntime {
    /// Запоминает только последнее отличающееся округлённое значение.
    ///
    /// Повтор того же `u16` не переносит deadline: sub-point движения мыши не должны
    /// бесконечно откладывать уже сформированный config intent.
    pub(crate) fn record_sidebar_width_change(
        &mut self,
        change: SidebarWidthChange,
        now: Instant,
    ) -> bool {
        if self
            .pending_sidebar_resize
            .is_some_and(|pending| pending.width_points == change.width_points)
        {
            return false;
        }

        self.pending_sidebar_resize = Some(PendingSidebarResize {
            width_points: change.width_points,
            persist_at: now + SIDEBAR_RESIZE_PERSIST_DEBOUNCE,
        });
        true
    }

    /// Ближайший owner-produced deadline для idle winit event loop.
    #[must_use]
    pub(crate) fn next_sidebar_resize_deadline(&self) -> Option<Instant> {
        self.pending_sidebar_resize
            .map(|pending| pending.persist_at)
    }

    /// Flush-ит resize, только когда quiet-period уже наступил.
    pub(crate) fn flush_due_sidebar_resize<A>(
        &mut self,
        now: Instant,
        runtime_adapter: &mut A,
    ) -> SettingsResult<SidebarResizeFlushOutcome>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        let Some(pending) = self.pending_sidebar_resize else {
            return Ok(SidebarResizeFlushOutcome::NoPending);
        };
        if now < pending.persist_at {
            return Ok(SidebarResizeFlushOutcome::NoPending);
        }

        self.flush_pending_sidebar_resize(runtime_adapter)
    }

    /// Принудительно завершает pending resize перед Apply/OK или lifecycle boundary.
    pub(crate) fn flush_pending_sidebar_resize<A>(
        &mut self,
        runtime_adapter: &mut A,
    ) -> SettingsResult<SidebarResizeFlushOutcome>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        let Some(pending) = self.pending_sidebar_resize.take() else {
            return Ok(SidebarResizeFlushOutcome::NoPending);
        };

        let committed_width_before_attempt =
            SidebarWidthPoints::from_committed(self.controller.committed().ui.sidebar.width_points);
        let report = match self.commit_runtime_setting_with_runtime_adapter(
            RuntimeSettingCommitRequest::new(
                SIDEBAR_WIDTH_SETTING_ID,
                SettingValue::Integer(i64::from(pending.width_points.value())),
            ),
            runtime_adapter,
        ) {
            Ok(report) => report,
            Err(error) => {
                runtime_adapter.restore_sidebar_width(committed_width_before_attempt);
                return Err(error);
            }
        };

        if matches!(
            report.final_state,
            ApplyFinalState::FullyApplied | ApplyFinalState::NoChanges
        ) {
            // `NoChanges` не вызывает snapshot sync, но live sub-point geometry всё равно
            // нормализуется к serializable committed u16.
            if report.final_state == ApplyFinalState::NoChanges {
                runtime_adapter.restore_sidebar_width(committed_width_before_attempt);
            }
            return Ok(SidebarResizeFlushOutcome::Succeeded);
        }

        // Controller уже компенсировал runtime route и сохранил committed/draft.
        // Geometry rollback остаётся отдельным намерением внешнего AppState owner-а.
        runtime_adapter.restore_sidebar_width(committed_width_before_attempt);
        self.invalidate_ui_model();
        self.status = status_from_apply_report(&report);
        tracing::error!(
            final_state = ?report.final_state,
            attempted_width_points = pending.width_points.value(),
            "Не удалось сохранить ширину sidebar; live geometry возвращена к committed config"
        );
        Ok(SidebarResizeFlushOutcome::Failed)
    }
}
