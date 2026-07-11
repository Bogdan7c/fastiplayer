use super::*;

/// Результат preview tick-а, по которому shell решает, нужен ли timed repaint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SettingsPreviewTick {
    /// Следующий wake-up для retry/coalesced update, если pending preview ещё есть.
    pub(crate) repaint_after: Option<Duration>,
}

impl SettingsRuntime {
    /// Отправляет pending live preview update, если pacing разрешает runtime apply сейчас.
    pub(crate) fn apply_due_preview<A>(
        &mut self,
        render_adapter: &mut A,
        now: Instant,
    ) -> SettingsResult<SettingsPreviewTick>
    where
        A: RenderLiveSettingsAdapter,
    {
        let pending_routes = self.controller.preview().pending_routes();
        if pending_routes.is_empty() {
            return Ok(SettingsPreviewTick::default());
        }

        let preview_interval = self.live_preview_interval();
        if let Some(last_sent_at) = self.last_preview_sent_at {
            let elapsed = now.saturating_duration_since(last_sent_at);
            if elapsed < preview_interval {
                return Ok(SettingsPreviewTick {
                    repaint_after: Some(preview_interval - elapsed),
                });
            }
        }

        self.invalidate_ui_model();
        let mut delegate = SettingsRuntimePreviewDelegate {
            route_appliers: &mut self.route_appliers,
            render_adapter,
        };
        let mut reports = Vec::new();
        for route in pending_routes {
            if let Some(report) = self
                .controller
                .apply_pending_preview(&route, &mut delegate)?
            {
                reports.push(report);
            }
        }

        if !reports.is_empty() {
            self.last_preview_sent_at = Some(now);
            self.status = status_from_preview_reports(&reports);
        }

        let repaint_after =
            (!self.controller.preview().pending_routes().is_empty()).then_some(preview_interval);
        Ok(SettingsPreviewTick { repaint_after })
    }
    /// Откатывает live preview routes и закрывает окно только после успешного rollback.
    pub(super) fn cancel_edit<A>(&mut self, render_adapter: &mut A) -> SettingsResult<bool>
    where
        A: RenderLiveSettingsAdapter,
    {
        let mut rollbacker = SettingsRuntimeRollbackDelegate {
            route_appliers: &mut self.route_appliers,
            render_adapter,
        };
        let report = self.controller.cancel(&mut rollbacker)?;
        self.settings_window_open = false;
        self.field_validation_errors.clear();
        self.status = status_from_cancel(&report);
        self.last_preview_sent_at = None;
        Ok(true)
    }
}

impl SettingsRuntimeRouteAppliers {
    /// Отправляет live preview update в renderer и фиксирует active preview snapshot.
    fn preview_render_live_settings<A>(
        &mut self,
        document: &AppConfig,
        render_adapter: &mut A,
    ) -> PreviewApplyResult
    where
        A: RenderLiveSettingsAdapter,
    {
        let next_settings = match render_live_settings_from_config(document) {
            Ok(settings) => settings,
            Err(error) => {
                return PreviewApplyResult::Fatal {
                    message: error.to_string(),
                };
            }
        };
        let update = RenderLiveSettingsUpdate::from_baseline(&self.render_live, next_settings);
        match render_adapter.preview_live_settings(&update) {
            Ok(report) => {
                self.render_live = update.settings;
                preview_result_from_render_report(report.outcome)
            }
            Err(error) => preview_result_from_render_error(&error),
        }
    }

    /// Откатывает renderer preview route к captured baseline document-у.
    fn rollback_render_live_settings<A>(
        &mut self,
        baseline_document: &AppConfig,
        render_adapter: &mut A,
    ) -> SettingsResult<RollbackResult>
    where
        A: RenderLiveSettingsAdapter,
    {
        let baseline_settings = render_live_settings_from_config(baseline_document)?;
        let report = render_adapter
            .rollback_live_settings(&baseline_settings)
            .map_err(|error| settings_core::SettingsError::access_failed(error.to_string()))?;
        self.render_live = baseline_settings;
        Ok(rollback_result_from_render_report(report.outcome))
    }

    /// Фиксирует preview-capable render settings как committed renderer state.
    pub(super) fn commit_render_preview_update<A>(
        &mut self,
        settings: &RenderLiveSettings,
        render_adapter: &mut A,
    ) -> AppRouteApplyResult
    where
        A: RenderLiveSettingsAdapter,
    {
        match render_adapter.commit_live_settings(settings) {
            Ok(_) => {
                self.render_live = settings.clone();
                AppRouteApplyResult::PreviewPromoted
            }
            Err(error) => AppRouteApplyResult::Failed {
                message: error.to_string(),
            },
        }
    }

    /// Компенсирует committed preview promotion через отдельный rollback intent.
    pub(super) fn rollback_committed_render_preview_update<A>(
        &mut self,
        settings: &RenderLiveSettings,
        render_adapter: &mut A,
    ) -> AppRouteApplyResult
    where
        A: RenderLiveSettingsAdapter,
    {
        match render_adapter.rollback_live_settings(settings) {
            Ok(_) => {
                self.render_live = settings.clone();
                AppRouteApplyResult::Applied
            }
            Err(error) => AppRouteApplyResult::Failed {
                message: error.to_string(),
            },
        }
    }
}
/// Delegate preview-а: controller отвечает за coalescing, runtime - за renderer boundary.
struct SettingsRuntimePreviewDelegate<'runtime, A> {
    /// App-owned route snapshots.
    route_appliers: &'runtime mut SettingsRuntimeRouteAppliers,

    /// Renderer-neutral adapter без WGPU типов в settings/runtime controller.
    render_adapter: &'runtime mut A,
}

impl<A> PreviewSettingsApplier<AppConfig> for SettingsRuntimePreviewDelegate<'_, A>
where
    A: RenderLiveSettingsAdapter,
{
    fn apply_preview(
        &mut self,
        request: PreviewApplyRequest<'_, AppConfig>,
    ) -> SettingsResult<PreviewApplyReport> {
        let result = if request.update.route == SettingRouteId::from(RENDER_PREVIEW_ROUTE_ID) {
            self.route_appliers
                .preview_render_live_settings(&request.update.document, self.render_adapter)
        } else {
            PreviewApplyResult::Unsupported {
                message: format!(
                    "Preview route `{}` не поддержан settings runtime",
                    request.update.route
                ),
            }
        };
        Ok(PreviewApplyReport {
            route: request.update.route.clone(),
            result,
            affected_settings: request.update.affected_settings.clone(),
        })
    }
}

/// Delegate rollback-а: controller хранит baseline, runtime применяет его к renderer.
struct SettingsRuntimeRollbackDelegate<'runtime, A> {
    /// App-owned route snapshots.
    route_appliers: &'runtime mut SettingsRuntimeRouteAppliers,

    /// Renderer-neutral adapter без зависимости UI от renderer backend-а.
    render_adapter: &'runtime mut A,
}

impl<A> PreviewRollbacker<AppConfig> for SettingsRuntimeRollbackDelegate<'_, A>
where
    A: RenderLiveSettingsAdapter,
{
    fn rollback_preview(
        &mut self,
        request: PreviewRollbackRequest<'_, AppConfig>,
    ) -> SettingsResult<RollbackReport> {
        let result = if request.route == &SettingRouteId::from(RENDER_PREVIEW_ROUTE_ID) {
            self.route_appliers
                .rollback_render_live_settings(request.baseline_document, self.render_adapter)?
        } else {
            RollbackResult::Noop
        };
        Ok(RollbackReport {
            route: request.route.clone(),
            result,
            affected_settings: request.affected_settings.to_vec(),
        })
    }
}
/// Переводит успешный render-core preview report в neutral settings-core result.
fn preview_result_from_render_report(outcome: RenderLiveApplyOutcome) -> PreviewApplyResult {
    match outcome {
        RenderLiveApplyOutcome::Applied => PreviewApplyResult::Applied,
        RenderLiveApplyOutcome::NoOp => PreviewApplyResult::Noop,
    }
}

/// Переводит render-core error taxonomy в retry/non-retry preview status.
fn preview_result_from_render_error(
    error: &render_core::RenderLiveSettingsError,
) -> PreviewApplyResult {
    match error.kind() {
        RenderLiveSettingsErrorKind::AbsentResource => PreviewApplyResult::Backpressured,
        RenderLiveSettingsErrorKind::Unsupported => PreviewApplyResult::Unsupported {
            message: error.to_string(),
        },
        RenderLiveSettingsErrorKind::Fatal => PreviewApplyResult::Fatal {
            message: error.to_string(),
        },
    }
}

/// Переводит successful renderer rollback outcome в settings-core rollback result.
fn rollback_result_from_render_report(outcome: RenderLiveApplyOutcome) -> RollbackResult {
    match outcome {
        RenderLiveApplyOutcome::Applied => RollbackResult::RolledBack,
        RenderLiveApplyOutcome::NoOp => RollbackResult::Noop,
    }
}
