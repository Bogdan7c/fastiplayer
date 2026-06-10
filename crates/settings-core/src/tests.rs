use crate::{
    ApplyFinalState, ApplyRouteReport, ApplyRouteResult, CommittedApplyRequest,
    CommittedSettingsApplier, DefaultBehavior, NumericDescriptor, NumericRange, NumericStep,
    PersistReport, PersistRequest, PreviewApplyReport, PreviewApplyRequest, PreviewApplyResult,
    PreviewRollbackRequest, PreviewRollbacker, PreviewSettingsApplier, RollbackReport,
    RouteGeneration, SettingAccess, SettingApplyMode, SettingDescriptor, SettingDescriptorText,
    SettingEditor, SettingId, SettingOption, SettingOptionCurrentValue, SettingOptionId,
    SettingOptions, SettingPlacement, SettingRouteId, SettingText, SettingValue, SettingValueError,
    SettingValueType, SettingsController, SettingsError, SettingsPersister, SettingsRegistry,
    SettingsValidator, TextDescriptor, TextFormat, ValidationReport, ValidationRequest,
    VectorDescriptor,
};

#[derive(Debug, Clone, PartialEq)]
struct TestDocument {
    brightness: f64,
    schema_version: i64,
    title: String,
    rgb: Vec<f64>,
}

impl Default for TestDocument {
    fn default() -> Self {
        Self {
            brightness: 0.5,
            schema_version: 2,
            title: "Исходное имя".to_owned(),
            rgb: vec![1.0, 1.0, 1.0],
        }
    }
}

struct BrightnessAccessor;

impl crate::SettingAccessor<TestDocument> for BrightnessAccessor {
    fn get(&self, document: &TestDocument) -> crate::SettingsResult<SettingValue> {
        Ok(SettingValue::Float(document.brightness))
    }

    fn set(&self, document: &mut TestDocument, value: SettingValue) -> crate::SettingsResult<()> {
        let SettingValue::Float(brightness) = value else {
            return Err(SettingsError::access_failed("brightness expected float"));
        };
        document.brightness = brightness;
        Ok(())
    }

    fn reset(
        &self,
        document: &mut TestDocument,
        default_document: &TestDocument,
    ) -> crate::SettingsResult<()> {
        document.brightness = default_document.brightness;
        Ok(())
    }
}

struct SchemaVersionAccessor;

impl crate::SettingAccessor<TestDocument> for SchemaVersionAccessor {
    fn get(&self, document: &TestDocument) -> crate::SettingsResult<SettingValue> {
        Ok(SettingValue::Integer(document.schema_version))
    }

    fn set(&self, document: &mut TestDocument, value: SettingValue) -> crate::SettingsResult<()> {
        let SettingValue::Integer(schema_version) = value else {
            return Err(SettingsError::access_failed(
                "schema version expected integer",
            ));
        };
        document.schema_version = schema_version;
        Ok(())
    }

    fn reset(
        &self,
        document: &mut TestDocument,
        default_document: &TestDocument,
    ) -> crate::SettingsResult<()> {
        document.schema_version = default_document.schema_version;
        Ok(())
    }
}

struct TitleAccessor;

impl crate::SettingAccessor<TestDocument> for TitleAccessor {
    fn get(&self, document: &TestDocument) -> crate::SettingsResult<SettingValue> {
        Ok(SettingValue::Text(document.title.clone()))
    }

    fn set(&self, document: &mut TestDocument, value: SettingValue) -> crate::SettingsResult<()> {
        let SettingValue::Text(title) = value else {
            return Err(SettingsError::access_failed("title expected text"));
        };
        document.title = title;
        Ok(())
    }

    fn reset(
        &self,
        document: &mut TestDocument,
        default_document: &TestDocument,
    ) -> crate::SettingsResult<()> {
        document.title.clone_from(&default_document.title);
        Ok(())
    }
}

struct RgbAccessor;

impl crate::SettingAccessor<TestDocument> for RgbAccessor {
    fn get(&self, document: &TestDocument) -> crate::SettingsResult<SettingValue> {
        Ok(SettingValue::NumericVector(document.rgb.clone()))
    }

    fn set(&self, document: &mut TestDocument, value: SettingValue) -> crate::SettingsResult<()> {
        let SettingValue::NumericVector(rgb) = value else {
            return Err(SettingsError::access_failed("rgb expected numeric vector"));
        };
        document.rgb = rgb;
        Ok(())
    }

    fn reset(
        &self,
        document: &mut TestDocument,
        default_document: &TestDocument,
    ) -> crate::SettingsResult<()> {
        document.rgb.clone_from(&default_document.rgb);
        Ok(())
    }
}

#[derive(Default)]
struct RecordingApplyDelegate {
    calls: Vec<&'static str>,
    validate_error: Option<SettingsError>,
    persist_error: Option<SettingsError>,
    apply_error: Option<SettingsError>,
    route_reports: Option<Vec<ApplyRouteReport>>,
}

impl SettingsValidator<TestDocument> for RecordingApplyDelegate {
    fn validate(
        &mut self,
        request: ValidationRequest<'_, TestDocument>,
    ) -> crate::SettingsResult<ValidationReport> {
        self.calls.push("validate");
        if let Some(error) = self.validate_error.clone() {
            return Err(error);
        }

        Ok(ValidationReport::valid(
            request
                .changed_settings
                .changes()
                .iter()
                .map(|change| change.id.clone())
                .collect(),
        ))
    }
}

impl SettingsPersister<TestDocument> for RecordingApplyDelegate {
    fn persist(
        &mut self,
        _request: PersistRequest<'_, TestDocument>,
    ) -> crate::SettingsResult<PersistReport> {
        self.calls.push("persist");
        if let Some(error) = self.persist_error.clone() {
            return Err(error);
        }

        Ok(PersistReport::persisted())
    }
}

impl CommittedSettingsApplier<TestDocument> for RecordingApplyDelegate {
    fn apply_committed(
        &mut self,
        request: CommittedApplyRequest<'_, TestDocument>,
    ) -> crate::SettingsResult<Vec<ApplyRouteReport>> {
        self.calls.push("apply");
        if let Some(error) = self.apply_error.clone() {
            return Err(error);
        }
        if let Some(route_reports) = self.route_reports.clone() {
            return Ok(route_reports);
        }

        Ok(request
            .route_updates
            .iter()
            .map(|update| {
                ApplyRouteReport::applied(update.route.clone(), update.affected_settings.clone())
            })
            .collect())
    }
}

#[derive(Default)]
struct RecordingRollbacker {
    rolled_back_routes: Vec<SettingRouteId>,
}

impl PreviewRollbacker<TestDocument> for RecordingRollbacker {
    fn rollback_preview(
        &mut self,
        request: PreviewRollbackRequest<'_, TestDocument>,
    ) -> crate::SettingsResult<RollbackReport> {
        self.rolled_back_routes.push(request.route.clone());
        Ok(RollbackReport::rolled_back(
            request.route.clone(),
            request.affected_settings.to_vec(),
        ))
    }
}

#[derive(Default)]
struct RecordingPreviewer {
    result: Option<PreviewApplyResult>,
    applied_documents: Vec<TestDocument>,
}

impl PreviewSettingsApplier<TestDocument> for RecordingPreviewer {
    fn apply_preview(
        &mut self,
        request: PreviewApplyRequest<'_, TestDocument>,
    ) -> crate::SettingsResult<PreviewApplyReport> {
        self.applied_documents.push(request.update.document.clone());
        let result = self.result.clone().unwrap_or(PreviewApplyResult::Applied);
        Ok(PreviewApplyReport {
            route: request.update.route.clone(),
            result,
            affected_settings: request.update.affected_settings.clone(),
        })
    }
}

#[test]
fn registry_rejects_duplicate_ids() {
    let mut registry = SettingsRegistry::<TestDocument>::empty();
    let descriptor = brightness_descriptor();

    registry
        .register(descriptor.clone(), BrightnessAccessor)
        .expect("first descriptor should register");

    let error = registry
        .register(descriptor, BrightnessAccessor)
        .expect_err("duplicate setting id must be rejected");

    assert_eq!(
        error,
        SettingsError::DuplicateSettingId {
            id: SettingId::from("render.brightness"),
        }
    );
}

#[test]
fn descriptor_text_ids_and_fallback_text_are_represented() {
    let descriptor = brightness_descriptor();

    assert_eq!(
        descriptor.text.label.text_id.as_str(),
        "settings.render.brightness.label"
    );
    assert_eq!(descriptor.text.label.fallback_ru, "Яркость");

    let description = descriptor
        .text
        .description
        .as_ref()
        .expect("description fallback must be present");
    assert_eq!(
        description.text_id.as_str(),
        "settings.render.brightness.description"
    );
    assert_eq!(description.fallback_ru, "Регулирует яркость изображения.");

    let help = descriptor
        .text
        .help
        .as_ref()
        .expect("help fallback must be present");
    assert_eq!(help.text_id.as_str(), "settings.render.brightness.help");
    assert_eq!(
        help.fallback_ru,
        "Значение 0.5 соответствует нейтральной яркости."
    );
}

#[test]
fn diff_only_reports_changed_fields() {
    let mut registry = SettingsRegistry::<TestDocument>::empty();
    registry
        .register(brightness_descriptor(), BrightnessAccessor)
        .expect("brightness descriptor should register");
    registry
        .register(title_descriptor(), TitleAccessor)
        .expect("title descriptor should register");
    registry
        .register(rgb_descriptor(), RgbAccessor)
        .expect("rgb descriptor should register");

    let baseline = TestDocument::default();
    let mut current = baseline.clone();
    current.title = "Новое имя".to_owned();

    let diff = registry
        .diff(&baseline, &current)
        .expect("diff should read values through accessors");

    assert_eq!(diff.len(), 1);
    assert_eq!(diff.changes()[0].id, SettingId::from("ui.title"));
    assert_eq!(
        diff.changes()[0].before,
        SettingValue::Text("Исходное имя".to_owned())
    );
    assert_eq!(
        diff.changes()[0].after,
        SettingValue::Text("Новое имя".to_owned())
    );
}

#[test]
fn read_only_setting_rejects_set() {
    let mut registry = SettingsRegistry::<TestDocument>::empty();
    registry
        .register(schema_descriptor(), SchemaVersionAccessor)
        .expect("schema descriptor should register");

    let mut document = TestDocument::default();
    let error = registry
        .set_value(
            &mut document,
            &SettingId::from("system.schema_version"),
            SettingValue::Integer(3),
        )
        .expect_err("read-only setting must reject set");

    assert_eq!(
        error,
        SettingsError::ReadOnlySetting {
            id: SettingId::from("system.schema_version"),
        }
    );
    assert_eq!(document.schema_version, 2);
}

#[test]
fn vector_length_validation_rejects_wrong_length() {
    let mut registry = SettingsRegistry::<TestDocument>::empty();
    registry
        .register(rgb_descriptor(), RgbAccessor)
        .expect("rgb descriptor should register");

    let mut document = TestDocument::default();
    let error = registry
        .set_value(
            &mut document,
            &SettingId::from("render.rgb"),
            SettingValue::NumericVector(vec![1.0, 0.5]),
        )
        .expect_err("wrong vector length must be rejected");

    assert_eq!(
        error,
        SettingsError::InvalidValue {
            id: SettingId::from("render.rgb"),
            reason: SettingValueError::InvalidVectorLength {
                expected: 3,
                actual: 2,
            },
        }
    );
    assert_eq!(document.rgb, vec![1.0, 1.0, 1.0]);
}

#[test]
fn dynamic_unavailable_current_value_is_explicit() {
    let options = SettingOptions::ready(
        "audio.output_device",
        vec![SettingOption::new(
            "default",
            SettingText::new("settings.audio.output.default", "Системное устройство"),
        )],
        SettingOptionCurrentValue::UnavailableCurrent {
            id: SettingOptionId::from("usb-dac-42"),
            label: SettingText::new(
                "settings.audio.output.unavailable_current",
                "USB DAC 42 (сейчас недоступно)",
            ),
        },
    );

    assert!(options.current.is_unavailable_current());
    let SettingOptionCurrentValue::UnavailableCurrent { id, label } = options.current else {
        panic!("current value should be explicitly unavailable");
    };
    assert_eq!(id.as_str(), "usb-dac-42");
    assert_eq!(label.fallback_ru, "USB DAC 42 (сейчас недоступно)");
}

#[test]
fn controller_draft_edit_does_not_mutate_committed_document() {
    let registry = controller_registry();
    let mut controller = SettingsController::new(TestDocument::default(), registry);

    let report = controller
        .set_value(
            SettingId::from("ui.title"),
            SettingValue::Text("Черновое имя".to_owned()),
        )
        .expect("draft edit should pass through registry accessor");

    assert!(report.draft_changed);
    assert!(!report.preview_queued);
    assert_eq!(controller.committed().title, "Исходное имя");
    assert_eq!(controller.draft().title, "Черновое имя");
    assert_eq!(
        controller
            .diff()
            .expect("controller diff should be readable")
            .len(),
        1
    );
}

#[test]
fn controller_lazy_preview_baseline_captures_only_on_actual_preview_change() {
    let registry = controller_registry();
    let mut controller = SettingsController::new(TestDocument::default(), registry);
    let render_route = SettingRouteId::from("render");

    controller
        .set_value(
            SettingId::from("ui.title"),
            SettingValue::Text("Не preview".to_owned()),
        )
        .expect("committed-only draft edit should succeed");
    assert!(controller.preview().is_empty());

    let no_change_report = controller
        .set_value(
            SettingId::from("render.brightness"),
            SettingValue::Float(0.5),
        )
        .expect("setting the existing value should succeed");
    assert!(!no_change_report.draft_changed);
    assert!(controller.preview().is_empty());

    controller.set_route_generation(render_route.clone(), RouteGeneration::new(7));
    controller
        .set_value(
            SettingId::from("render.brightness"),
            SettingValue::Float(0.75),
        )
        .expect("first actual preview edit should queue update");
    controller.set_route_generation(render_route.clone(), RouteGeneration::new(9));
    controller
        .set_value(
            SettingId::from("render.rgb"),
            SettingValue::NumericVector(vec![0.9, 0.8, 0.7]),
        )
        .expect("second preview edit on same route should not replace baseline");

    let route_state = controller
        .preview()
        .route_state(&render_route)
        .expect("preview baseline should be captured for render route");
    assert_eq!(route_state.baseline_document.brightness, 0.5);
    assert_eq!(route_state.baseline_generation, RouteGeneration::new(7));
    assert_eq!(
        controller.route_generation_baseline(&render_route),
        Some(RouteGeneration::new(7))
    );
}

#[test]
fn controller_cancel_rolls_back_only_routes_with_preview_baseline() {
    let registry = controller_registry();
    let mut controller = SettingsController::new(TestDocument::default(), registry);
    let render_route = SettingRouteId::from("render");

    controller
        .set_value(
            SettingId::from("ui.title"),
            SettingValue::Text("Черновое имя".to_owned()),
        )
        .expect("committed-only draft edit should succeed");
    controller
        .set_value(
            SettingId::from("render.brightness"),
            SettingValue::Float(0.2),
        )
        .expect("preview draft edit should succeed");

    let mut previewer = RecordingPreviewer::default();
    controller
        .apply_pending_preview(&render_route, &mut previewer)
        .expect("preview delegate should run")
        .expect("render preview should be pending");

    let mut rollbacker = RecordingRollbacker::default();
    let cancel_report = controller
        .cancel(&mut rollbacker)
        .expect("cancel should rollback preview and discard draft");

    assert_eq!(rollbacker.rolled_back_routes, vec![render_route.clone()]);
    assert_eq!(cancel_report.rolled_back_routes.len(), 1);
    assert_eq!(controller.draft(), controller.committed());
    assert!(controller.preview().is_empty());
}

#[test]
fn controller_reset_field_uses_default_document() {
    let registry = controller_registry();
    let committed = TestDocument {
        title: "Сохранённое имя".to_owned(),
        ..TestDocument::default()
    };
    let mut controller = SettingsController::new(committed, registry);

    let report = controller
        .reset_value(SettingId::from("ui.title"))
        .expect("field reset should use default document through accessor");

    assert_eq!(controller.committed().title, "Сохранённое имя");
    assert_eq!(controller.draft().title, "Исходное имя");
    assert_eq!(report.affected_settings, vec![SettingId::from("ui.title")]);
}

#[test]
fn controller_reset_group_and_surface_use_registered_scopes() {
    let registry = controller_registry();
    let committed = TestDocument {
        brightness: 0.1,
        rgb: vec![0.1, 0.2, 0.3],
        title: "Сохранённое имя".to_owned(),
        ..TestDocument::default()
    };
    let mut controller = SettingsController::new(committed, registry);

    controller
        .reset_group("render".into(), "color".into())
        .expect("group reset should reset render color fields");
    assert_eq!(controller.draft().brightness, 0.5);
    assert_eq!(controller.draft().rgb, vec![1.0, 1.0, 1.0]);
    assert_eq!(controller.draft().title, "Сохранённое имя");
    assert_eq!(
        controller.preview().pending_routes(),
        vec![SettingRouteId::from("render")]
    );

    controller
        .reset_surface("main-settings-window".into())
        .expect("surface reset should reset all resettable settings on the surface");
    assert_eq!(controller.draft(), &TestDocument::default());
}

#[test]
fn controller_reset_all_replaces_draft_with_default_document() {
    let registry = controller_registry();
    let committed = TestDocument {
        brightness: 0.1,
        title: "Сохранённое имя".to_owned(),
        ..TestDocument::default()
    };
    let mut controller = SettingsController::new(committed, registry);

    controller
        .set_value(
            SettingId::from("render.rgb"),
            SettingValue::NumericVector(vec![0.2, 0.3, 0.4]),
        )
        .expect("draft edit should succeed before reset all");

    let report = controller
        .reset_all()
        .expect("reset all should replace the whole draft document");

    assert_eq!(controller.draft(), &TestDocument::default());
    assert!(
        report
            .affected_settings
            .contains(&SettingId::from("ui.title"))
    );
    assert!(
        report
            .affected_settings
            .contains(&SettingId::from("render.brightness"))
    );
}

#[test]
fn controller_apply_runs_validate_persist_then_runtime_apply() {
    let registry = controller_registry();
    let mut controller = SettingsController::new(TestDocument::default(), registry);
    let ui_route = SettingRouteId::from("ui");
    controller
        .set_value(
            SettingId::from("ui.title"),
            SettingValue::Text("Применённое имя".to_owned()),
        )
        .expect("draft edit should succeed");

    let mut delegate = RecordingApplyDelegate::default();
    let report = controller
        .apply(&mut delegate)
        .expect("apply should return a detailed report");

    assert_eq!(delegate.calls, vec!["validate", "persist", "apply"]);
    assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
    assert_eq!(controller.committed().title, "Применённое имя");
    assert_eq!(controller.draft(), controller.committed());
    assert_eq!(
        controller.route_generation(&ui_route),
        RouteGeneration::new(1)
    );
}

#[test]
fn controller_persist_failure_stops_runtime_apply() {
    let registry = controller_registry();
    let mut controller = SettingsController::new(TestDocument::default(), registry);
    controller
        .set_value(
            SettingId::from("ui.title"),
            SettingValue::Text("Не сохранено".to_owned()),
        )
        .expect("draft edit should succeed");
    let mut delegate = RecordingApplyDelegate {
        persist_error: Some(SettingsError::access_failed("disk is unavailable")),
        ..RecordingApplyDelegate::default()
    };

    let report = controller
        .apply(&mut delegate)
        .expect("persist failure should be represented as apply report");

    assert_eq!(delegate.calls, vec!["validate", "persist"]);
    assert_eq!(report.final_state, ApplyFinalState::PersistFailed);
    assert_eq!(controller.committed().title, "Исходное имя");
    assert_eq!(controller.draft().title, "Не сохранено");
}

#[test]
fn controller_runtime_partial_failure_reports_persisted_runtime_divergence() {
    let registry = controller_registry();
    let mut controller = SettingsController::new(TestDocument::default(), registry);
    controller
        .set_value(
            SettingId::from("ui.title"),
            SettingValue::Text("Сохранено частично".to_owned()),
        )
        .expect("draft edit should succeed");
    let mut delegate = RecordingApplyDelegate {
        route_reports: Some(vec![ApplyRouteReport::partial_failure(
            SettingRouteId::from("ui"),
            vec![SettingId::from("ui.title")],
            "runtime owner accepted only part of the update",
        )]),
        ..RecordingApplyDelegate::default()
    };

    let report = controller
        .apply(&mut delegate)
        .expect("partial runtime failure should be reported");

    assert_eq!(
        report.final_state,
        ApplyFinalState::PersistedRuntimeDiverged
    );
    assert_eq!(controller.committed().title, "Сохранено частично");
    assert_eq!(controller.draft(), controller.committed());
    let divergence = controller
        .runtime_divergence()
        .expect("controller should retain explicit divergence state");
    assert_eq!(divergence.persisted_document.title, "Сохранено частично");
    assert_eq!(divergence.last_fully_active_document.title, "Исходное имя");
    assert!(matches!(
        divergence.route_reports[0].result,
        ApplyRouteResult::PartialFailure { .. }
    ));
}

#[test]
fn controller_runtime_divergence_retry_reapplies_saved_document() {
    let registry = controller_registry();
    let mut controller = SettingsController::new(TestDocument::default(), registry);
    controller
        .set_value(
            SettingId::from("ui.title"),
            SettingValue::Text("Нужно применить повторно".to_owned()),
        )
        .expect("draft edit should succeed");
    let mut first_delegate = RecordingApplyDelegate {
        route_reports: Some(vec![ApplyRouteReport::partial_failure(
            SettingRouteId::from("ui"),
            vec![SettingId::from("ui.title")],
            "runtime owner failed once",
        )]),
        ..RecordingApplyDelegate::default()
    };
    controller
        .apply(&mut first_delegate)
        .expect("first apply should persist and report runtime divergence");
    controller.begin_edit();
    assert!(
        controller.runtime_divergence().is_some(),
        "opening a fresh edit transaction must keep saved/runtime divergence"
    );

    let mut retry_delegate = RecordingApplyDelegate::default();
    let retry_report = controller
        .apply(&mut retry_delegate)
        .expect("retry should apply saved document to runtime");

    assert_eq!(retry_delegate.calls, vec!["validate", "apply"]);
    assert_eq!(retry_report.final_state, ApplyFinalState::FullyApplied);
    assert_eq!(
        retry_report
            .persistence
            .expect("retry should still report persistence state")
            .outcome,
        crate::PersistOutcome::SkippedNoChanges
    );
    assert!(controller.runtime_divergence().is_none());
    assert_eq!(controller.committed().title, "Нужно применить повторно");
}

#[test]
fn controller_preview_baseline_after_divergence_uses_last_fully_active_runtime() {
    let registry = controller_registry();
    let mut controller = SettingsController::new(TestDocument::default(), registry);
    let render_route = SettingRouteId::from("render");
    controller
        .set_value(
            SettingId::from("render.brightness"),
            SettingValue::Float(0.8),
        )
        .expect("preview edit should succeed");
    let mut delegate = RecordingApplyDelegate {
        route_reports: Some(vec![ApplyRouteReport::partial_failure(
            render_route.clone(),
            vec![SettingId::from("render.brightness")],
            "render runtime did not accept committed brightness",
        )]),
        ..RecordingApplyDelegate::default()
    };
    controller
        .apply(&mut delegate)
        .expect("partial render apply should create runtime divergence");

    assert_eq!(controller.committed().brightness, 0.8);
    controller.begin_edit();
    controller
        .set_value(
            SettingId::from("render.rgb"),
            SettingValue::NumericVector(vec![0.2, 0.3, 0.4]),
        )
        .expect("new preview edit should capture active runtime baseline");

    let route_state = controller
        .preview()
        .route_state(&render_route)
        .expect("render preview baseline should be captured again after begin_edit");
    assert_eq!(route_state.baseline_document.brightness, 0.5);
    assert_eq!(route_state.baseline_document.rgb, vec![1.0, 1.0, 1.0]);
}

#[test]
fn controller_route_generation_conflict_blocks_last_writer_wins() {
    let registry = controller_registry();
    let mut controller = SettingsController::new(TestDocument::default(), registry);
    let ui_route = SettingRouteId::from("ui");
    controller
        .set_value(
            SettingId::from("ui.title"),
            SettingValue::Text("Локальный черновик".to_owned()),
        )
        .expect("draft edit should capture route generation baseline");
    controller.set_route_generation(ui_route.clone(), RouteGeneration::new(1));

    let mut delegate = RecordingApplyDelegate::default();
    let report = controller
        .apply(&mut delegate)
        .expect("conflict should be represented as apply report");

    assert!(delegate.calls.is_empty());
    assert_eq!(report.final_state, ApplyFinalState::BlockedByConflicts);
    assert_eq!(report.conflicts[0].route, ui_route);
    assert_eq!(report.conflicts[0].baseline, RouteGeneration::new(0));
    assert_eq!(report.conflicts[0].current, RouteGeneration::new(1));
    assert_eq!(controller.committed().title, "Исходное имя");
    assert_eq!(controller.draft().title, "Локальный черновик");
}

#[test]
fn controller_preview_coalescing_keeps_latest_update_per_route() {
    let registry = controller_registry();
    let mut controller = SettingsController::new(TestDocument::default(), registry);
    let render_route = SettingRouteId::from("render");

    controller
        .set_value(
            SettingId::from("render.brightness"),
            SettingValue::Float(0.4),
        )
        .expect("first preview edit should queue render update");
    controller
        .set_value(
            SettingId::from("render.brightness"),
            SettingValue::Float(0.9),
        )
        .expect("second preview edit should replace pending render update");
    controller
        .set_value(
            SettingId::from("render.rgb"),
            SettingValue::NumericVector(vec![0.2, 0.3, 0.4]),
        )
        .expect("same route preview edit should remain coalesced");

    assert_eq!(
        controller.preview().pending_routes(),
        vec![render_route.clone()]
    );
    let pending = controller
        .preview()
        .pending_update(&render_route)
        .expect("render route should have latest pending preview update");
    assert_eq!(pending.document.brightness, 0.9);
    assert_eq!(pending.document.rgb, vec![0.2, 0.3, 0.4]);
    assert_eq!(
        pending.affected_settings,
        vec![
            SettingId::from("render.brightness"),
            SettingId::from("render.rgb")
        ]
    );
}

fn controller_registry() -> SettingsRegistry<TestDocument> {
    SettingsRegistry::new(vec![
        crate::RegisteredSetting::new(brightness_descriptor(), BrightnessAccessor),
        crate::RegisteredSetting::new(schema_descriptor(), SchemaVersionAccessor),
        crate::RegisteredSetting::new(title_descriptor(), TitleAccessor),
        crate::RegisteredSetting::new(rgb_descriptor(), RgbAccessor),
    ])
    .expect("test controller registry should be valid")
}

fn brightness_descriptor() -> SettingDescriptor {
    SettingDescriptor {
        id: SettingId::from("render.brightness"),
        path: "render.brightness".into(),
        text: SettingDescriptorText::new(SettingText::new(
            "settings.render.brightness.label",
            "Яркость",
        ))
        .with_description(SettingText::new(
            "settings.render.brightness.description",
            "Регулирует яркость изображения.",
        ))
        .with_help(SettingText::new(
            "settings.render.brightness.help",
            "Значение 0.5 соответствует нейтральной яркости.",
        )),
        placement: SettingPlacement::new("render", "color", "main-settings-window"),
        value_type: SettingValueType::Float,
        editor: SettingEditor::Numeric(NumericDescriptor::new(
            NumericRange::Float { min: 0.0, max: 1.0 },
            NumericStep::Float(0.01),
            None,
        )),
        access: SettingAccess::ReadWrite,
        default_behavior: DefaultBehavior::FromDefaultDocument,
        route: SettingRouteId::from("render"),
        apply_mode: SettingApplyMode::ImmediatePreview,
    }
}

fn schema_descriptor() -> SettingDescriptor {
    SettingDescriptor {
        id: SettingId::from("system.schema_version"),
        path: "schema_version".into(),
        text: SettingDescriptorText::new(SettingText::new(
            "settings.system.schema_version.label",
            "Версия схемы",
        )),
        placement: SettingPlacement::new("system", "metadata", "main-settings-window"),
        value_type: SettingValueType::Integer,
        editor: SettingEditor::ReadOnly,
        access: SettingAccess::ReadOnly,
        default_behavior: DefaultBehavior::NoReset,
        route: SettingRouteId::from("system"),
        apply_mode: SettingApplyMode::CommittedApply,
    }
}

fn title_descriptor() -> SettingDescriptor {
    SettingDescriptor {
        id: SettingId::from("ui.title"),
        path: "ui.title".into(),
        text: SettingDescriptorText::new(SettingText::new(
            "settings.ui.title.label",
            "Название окна",
        )),
        placement: SettingPlacement::new("ui", "window", "main-settings-window"),
        value_type: SettingValueType::Text,
        editor: SettingEditor::Text(TextDescriptor::new(TextFormat::SingleLine)),
        access: SettingAccess::ReadWrite,
        default_behavior: DefaultBehavior::FromDefaultDocument,
        route: SettingRouteId::from("ui"),
        apply_mode: SettingApplyMode::CommittedApply,
    }
}

fn rgb_descriptor() -> SettingDescriptor {
    SettingDescriptor {
        id: SettingId::from("render.rgb"),
        path: "render.rgb".into(),
        text: SettingDescriptorText::new(SettingText::new("settings.render.rgb.label", "RGB")),
        placement: SettingPlacement::new("render", "color", "main-settings-window"),
        value_type: SettingValueType::NumericVector,
        editor: SettingEditor::Vector(VectorDescriptor::new(
            NumericDescriptor::new(
                NumericRange::Float { min: 0.0, max: 1.0 },
                NumericStep::Float(0.01),
                None,
            ),
            vec![
                SettingText::new("settings.render.rgb.red", "Красный"),
                SettingText::new("settings.render.rgb.green", "Зелёный"),
                SettingText::new("settings.render.rgb.blue", "Синий"),
            ],
            3,
        )),
        access: SettingAccess::ReadWrite,
        default_behavior: DefaultBehavior::FromDefaultDocument,
        route: SettingRouteId::from("render"),
        apply_mode: SettingApplyMode::ImmediatePreview,
    }
}
