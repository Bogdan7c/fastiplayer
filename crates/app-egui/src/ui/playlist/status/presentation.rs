//! Pure privacy-safe presentation проблем Playlist status.

use std::sync::Arc;

use playlist_core::PlaylistItemId;

use crate::playlist_runtime::{
    PlaylistInteractionModel, PlaylistManualAddEventId, PlaylistManualAddWarning,
    PlaylistManualAddWarningKind, PlaylistNavigationView, PlaylistProbeView,
    PlaylistSafeFeedbackGeneration, PlaylistSaveView, PlaylistStartupWarningView,
    PlaylistViewModel, SiblingDiscoveryScopeId,
};

use super::super::actions::PlaylistAction;

/// Snapshot содержит только согласованные warning/error и не владеет временем.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlaylistStatusPresentation {
    /// Каждая строка является одной независимо живущей проблемой.
    rows: Vec<PlaylistStatusRow>,
}

impl PlaylistStatusPresentation {
    /// Собирает bounded snapshot без URL, filesystem path и обычных lifecycle-сообщений.
    pub(super) fn from_models(
        model: &PlaylistViewModel,
        interaction: &PlaylistInteractionModel,
    ) -> Option<Self> {
        let mut presentation = Self { rows: Vec::new() };

        // Startup warning является проблемой, но loading/tombstone/progress сюда не входят.
        presentation.push_startup_warning(model.startup_warning());
        // Terminal sibling warning получает scope identity вместо форматированной строки.
        presentation.push_probe_warning(model.probe());
        // Успешный Manual Add отфильтрован read model-ом; остаётся только partial result.
        presentation.push_manual_add_warning(interaction.manual_add_warning.as_ref());
        // Повтор одинакового safe текста различается монотонным поколением runtime owner-а.
        presentation.push_safe_feedback(interaction);
        // Persistence сохраняет Retry, block и fatal distinctions.
        presentation.push_persistence(model.save());
        // Обычные wait/cancel/exhausted navigation lifecycle намеренно отсутствуют.
        presentation.push_navigation(model.navigation());

        (!presentation.rows.is_empty()).then_some(presentation)
    }

    /// Lifetime owner читает owned rows и никогда не сравнивает форматированный текст.
    pub(super) fn into_rows(self) -> Vec<PlaylistStatusRow> {
        self.rows
    }

    /// Renderer получает immutable newest-first snapshot.
    pub(super) fn rows(&self) -> &[PlaylistStatusRow] {
        &self.rows
    }

    /// Создаёт presentation из уже отобранных lifetime owner-ом проблем.
    pub(super) fn from_rows(rows: Vec<PlaylistStatusRow>) -> Option<Self> {
        (!rows.is_empty()).then_some(Self { rows })
    }

    fn push_startup_warning(&mut self, warning: PlaylistStartupWarningView) {
        if matches!(warning, PlaylistStartupWarningView::Present) {
            self.rows.push(PlaylistStatusRow::new(
                PlaylistStatusProblemIdentity::StartupWarning,
                PlaylistStatusRetention::WhilePresent,
                "Сохранённая очередь восстановлена с предупреждением",
                StatusTone::Warning,
                StatusRowKind::Normal,
                None,
            ));
        }
    }

    fn push_probe_warning(&mut self, probe: PlaylistProbeView) {
        if matches!(probe, PlaylistProbeView::Warning { .. }) {
            self.rows.push(PlaylistStatusRow::new(
                PlaylistStatusProblemIdentity::Probe(probe),
                PlaylistStatusRetention::Event,
                "Добавлена только доступная часть очереди; проверка завершилась с предупреждением",
                StatusTone::Warning,
                StatusRowKind::Normal,
                None,
            ));
        }
    }

    fn push_manual_add_warning(&mut self, warning: Option<&PlaylistManualAddWarning>) {
        let Some(warning) = warning else {
            return;
        };
        self.rows.push(PlaylistStatusRow::new(
            PlaylistStatusProblemIdentity::ManualAdd(warning.event_id),
            PlaylistStatusRetention::Event,
            manual_add_warning_message(warning),
            StatusTone::Warning,
            StatusRowKind::Normal,
            None,
        ));
    }

    fn push_safe_feedback(&mut self, interaction: &PlaylistInteractionModel) {
        let Some(feedback) = interaction.safe_feedback.as_ref() else {
            return;
        };
        self.rows.push(PlaylistStatusRow::new(
            PlaylistStatusProblemIdentity::SafeFeedback(feedback.generation),
            PlaylistStatusRetention::Event,
            Arc::clone(&feedback.message),
            StatusTone::Warning,
            StatusRowKind::Normal,
            None,
        ));
    }

    fn push_persistence(&mut self, save: PlaylistSaveView) {
        let Some((message, tone, kind, action)) = save_problem(save) else {
            return;
        };
        self.rows.push(PlaylistStatusRow::new(
            PlaylistStatusProblemIdentity::Save(save),
            PlaylistStatusRetention::WhilePresent,
            message,
            tone,
            kind,
            action,
        ));
    }

    fn push_navigation(&mut self, navigation: PlaylistNavigationView) {
        let Some((identity, message, action)) = navigation_problem(navigation) else {
            return;
        };
        self.rows.push(PlaylistStatusRow::new(
            PlaylistStatusProblemIdentity::Navigation(identity),
            PlaylistStatusRetention::WhilePresent,
            message,
            StatusTone::Error,
            StatusRowKind::Normal,
            action,
        ));
    }
}

/// Источник проблемы задаёт dedup slot; разные истории одного owner-а не копятся.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaylistStatusProblemSlot {
    StartupWarning,
    Probe,
    ManualAdd,
    SafeFeedback,
    Save,
    Navigation,
}

/// Typed identity события/состояния принципиально не содержит presentation text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaylistStatusProblemIdentity {
    StartupWarning,
    Probe(PlaylistProbeView),
    ManualAdd(PlaylistManualAddEventId),
    SafeFeedback(PlaylistSafeFeedbackGeneration),
    Save(PlaylistSaveView),
    Navigation(PlaylistNavigationProblemIdentity),
}

/// Navigation identity отделяет failed state от action tooltip presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaylistNavigationProblemIdentity {
    AwaitingUserAfterFailure { item_id: PlaylistItemId },
    Fatal { scope_id: SiblingDiscoveryScopeId },
}

impl PlaylistStatusProblemIdentity {
    /// Slot используется только для replacement/dedup одного runtime owner-а.
    pub(super) const fn slot(self) -> PlaylistStatusProblemSlot {
        match self {
            Self::StartupWarning => PlaylistStatusProblemSlot::StartupWarning,
            Self::Probe(_) => PlaylistStatusProblemSlot::Probe,
            Self::ManualAdd(_) => PlaylistStatusProblemSlot::ManualAdd,
            Self::SafeFeedback(_) => PlaylistStatusProblemSlot::SafeFeedback,
            Self::Save(_) => PlaylistStatusProblemSlot::Save,
            Self::Navigation(_) => PlaylistStatusProblemSlot::Navigation,
        }
    }
}

/// Event переживает исчезновение runtime snapshot-а до deadline; state исчезает при resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaylistStatusRetention {
    Event,
    WhilePresent,
}

/// Одна problem-строка отделяет typed lifetime semantics от egui layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlaylistStatusRow {
    /// Identity управляет lifetime/dedup, но никогда не показывается пользователю.
    identity: PlaylistStatusProblemIdentity,
    /// Retention определяет реакцию на явное resolved-состояние.
    retention: PlaylistStatusRetention,
    /// Текст уже privacy-safe и не участвует в identity.
    message: Arc<str>,
    /// Tone выбирает theme-owned foreground без hardcoded RGB.
    tone: StatusTone,
    /// Kind выражает обычную либо ослабленную строку.
    kind: StatusRowKind,
    /// Action остаётся узким status-only intent.
    action: Option<PlaylistStatusAction>,
}

impl PlaylistStatusRow {
    pub(super) fn new(
        identity: PlaylistStatusProblemIdentity,
        retention: PlaylistStatusRetention,
        message: impl Into<Arc<str>>,
        tone: StatusTone,
        kind: StatusRowKind,
        action: Option<PlaylistStatusAction>,
    ) -> Self {
        Self {
            identity,
            retention,
            message: message.into(),
            tone,
            kind,
            action,
        }
    }

    pub(super) const fn identity(&self) -> PlaylistStatusProblemIdentity {
        self.identity
    }

    pub(super) const fn retention(&self) -> PlaylistStatusRetention {
        self.retention
    }

    pub(super) fn text(&self) -> &str {
        &self.message
    }

    pub(super) const fn tone(&self) -> StatusTone {
        self.tone
    }

    pub(super) const fn kind(&self) -> StatusRowKind {
        self.kind
    }

    pub(super) const fn action(&self) -> Option<PlaylistStatusAction> {
        self.action
    }
}

/// Theme tone не содержит конкретных цветов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui::playlist) enum StatusTone {
    Warning,
    Error,
}

/// Визуальная роль строки остаётся понятной на месте render-вызова.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusRowKind {
    Normal,
    Weak,
}

/// Узкий набор действий гарантирует actionless measurement/residual/disabled copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaylistStatusAction {
    RetrySave,
    CancelNavigation { origin_already_ended: bool },
}

impl PlaylistStatusAction {
    pub(super) const fn button_label(self) -> &'static str {
        match self {
            Self::RetrySave => "Повторить",
            Self::CancelNavigation { .. } => "Отменить переход",
        }
    }

    pub(super) const fn tooltip(self) -> Option<&'static str> {
        match self {
            Self::RetrySave => None,
            Self::CancelNavigation {
                origin_already_ended: false,
            } => Some("Отменить сохранённый переход и оставить текущее воспроизведение"),
            Self::CancelNavigation {
                origin_already_ended: true,
            } => Some("Отменить сохранённый переход; завершившееся воспроизведение остановится"),
        }
    }

    pub(super) const fn into_playlist_action(self) -> PlaylistAction {
        match self {
            Self::RetrySave => PlaylistAction::RetrySave,
            Self::CancelNavigation { .. } => PlaylistAction::CancelNavigation,
        }
    }
}

/// Частичный batch сохраняет bounded accounting и не раскрывает имена файлов.
fn manual_add_warning_message(warning: &PlaylistManualAddWarning) -> String {
    if matches!(warning.kind, PlaylistManualAddWarningKind::Failed) {
        return "Не удалось добавить выбранные файлы".to_owned();
    }

    let mut reasons = Vec::with_capacity(4);
    if warning.unsupported_container > 0 {
        reasons.push(format!(
            "неподдерживаемый контейнер: {}",
            warning.unsupported_container
        ));
    }
    if warning.no_audio_video_tracks > 0 {
        reasons.push(format!(
            "без аудио/видео дорожек: {}",
            warning.no_audio_video_tracks
        ));
    }
    if warning.probe_failed > 0 {
        reasons.push(format!("ошибка проверки: {}", warning.probe_failed));
    }
    if warning.capacity_rejected > 0 {
        reasons.push(format!("не поместилось: {}", warning.capacity_rejected));
    }

    let summary = format!("Добавлено {} из {}", warning.added, warning.requested);
    if reasons.is_empty() {
        summary
    } else {
        format!("{summary}. {}", reasons.join("; "))
    }
}

/// Форматирует persistence и не показывает обычное фоновое сохранение.
#[cfg(test)]
pub(in crate::ui::playlist) fn save_message(
    save: PlaylistSaveView,
) -> Option<(String, StatusTone)> {
    save_problem(save).map(|(message, tone, _, _)| (message, tone))
}

fn save_problem(
    save: PlaylistSaveView,
) -> Option<(
    String,
    StatusTone,
    StatusRowKind,
    Option<PlaylistStatusAction>,
)> {
    match save {
        PlaylistSaveView::Idle | PlaylistSaveView::Saving => None,
        PlaylistSaveView::WarningRetryAvailable { attempt } => Some((
            format!(
                "Не удалось сохранить очередь (попыток: {}). Повтор сохранения доступен",
                attempt.occurrence_count()
            ),
            StatusTone::Warning,
            StatusRowKind::Normal,
            Some(PlaylistStatusAction::RetrySave),
        )),
        PlaylistSaveView::Blocked => Some((
            "Сохранение очереди заблокировано для защиты существующего файла".to_owned(),
            StatusTone::Warning,
            StatusRowKind::Weak,
            None,
        )),
        PlaylistSaveView::Fault(_fault) => Some((
            "Служба сохранения очереди недоступна".to_owned(),
            StatusTone::Error,
            StatusRowKind::Weak,
            None,
        )),
    }
}

/// Форматирует только navigation failures; wait/exhausted/cancelled остаются silent.
pub(in crate::ui::playlist) const fn navigation_message(
    navigation: PlaylistNavigationView,
) -> Option<(&'static str, StatusTone)> {
    match navigation {
        PlaylistNavigationView::Idle => None,
        PlaylistNavigationView::AwaitingUserAfterFailure { .. } => Some((
            "Переход не выполнен. Автоматический переход остановлен; Next, Previous или повтор продолжат сохранённый курсор",
            StatusTone::Error,
        )),
        PlaylistNavigationView::Fatal { .. } => {
            Some(("Переход недоступен из-за ошибки службы", StatusTone::Error))
        }
    }
}

fn navigation_problem(
    navigation: PlaylistNavigationView,
) -> Option<(
    PlaylistNavigationProblemIdentity,
    &'static str,
    Option<PlaylistStatusAction>,
)> {
    let (message, _) = navigation_message(navigation)?;
    let (identity, action) = match navigation {
        PlaylistNavigationView::AwaitingUserAfterFailure {
            item_id,
            origin_already_ended,
        } => (
            PlaylistNavigationProblemIdentity::AwaitingUserAfterFailure { item_id },
            Some(PlaylistStatusAction::CancelNavigation {
                origin_already_ended,
            }),
        ),
        PlaylistNavigationView::Fatal { scope_id } => {
            (PlaylistNavigationProblemIdentity::Fatal { scope_id }, None)
        }
        PlaylistNavigationView::Idle => return None,
    };
    Some((identity, message, action))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;
    use crate::playlist_runtime::{
        PlaylistActiveOperation, PlaylistInteractionModel, PlaylistManualAddEventId,
        PlaylistManualAddWarning, PlaylistPersistenceFault, PlaylistSafeFeedback,
        PlaylistSafeFeedbackGeneration, PlaylistSaveAttempt, SiblingDiscoveryScopeId,
    };
    use playlist_core::{PlaylistItemId, PlaylistQueue};

    fn model() -> PlaylistViewModel {
        PlaylistViewModel::for_queue_with_revision(&PlaylistQueue::new(), 0)
    }

    #[test]
    fn ordinary_progress_success_and_lifecycle_snapshots_are_silent() {
        let interaction = PlaylistInteractionModel {
            active_operation: Some(PlaylistActiveOperation::ManualAdd),
            ..PlaylistInteractionModel::default()
        };

        assert!(PlaylistStatusPresentation::from_models(&model(), &interaction).is_none());
    }

    #[test]
    fn partial_manual_add_keeps_safe_accounting_and_typed_identity() {
        let warning = PlaylistManualAddWarning {
            event_id: PlaylistManualAddEventId(7),
            kind: PlaylistManualAddWarningKind::PartialResult,
            requested: 5,
            added: 2,
            unsupported_container: 1,
            no_audio_video_tracks: 0,
            probe_failed: 2,
            capacity_rejected: 0,
        };
        let interaction = PlaylistInteractionModel {
            manual_add_warning: Some(warning),
            ..PlaylistInteractionModel::default()
        };
        let presentation = PlaylistStatusPresentation::from_models(&model(), &interaction)
            .expect("partial result is a warning");

        assert_eq!(presentation.rows().len(), 1);
        assert!(presentation.rows()[0].text().contains("Добавлено 2 из 5"));
        assert!(presentation.rows()[0].text().contains("ошибка проверки: 2"));
        assert_eq!(
            presentation.rows()[0].identity(),
            PlaylistStatusProblemIdentity::ManualAdd(PlaylistManualAddEventId(7))
        );
        assert!(!presentation.rows()[0].text().contains("/home/secret"));
    }

    #[test]
    fn every_agreed_problem_scenario_remains_typed_and_action_scoped() {
        let scope_id = SiblingDiscoveryScopeId::from_non_zero(
            NonZeroU64::new(11).expect("fixture identity is non-zero"),
        );
        let item_id =
            PlaylistItemId::from_persistence_value(12).expect("fixture item identity is non-zero");
        let model = model().with_status_for_test(
            PlaylistProbeView::Warning { scope_id },
            PlaylistSaveView::WarningRetryAvailable {
                attempt: PlaylistSaveAttempt::for_test(3),
            },
            PlaylistNavigationView::AwaitingUserAfterFailure {
                item_id,
                origin_already_ended: false,
            },
            PlaylistStartupWarningView::Present,
        );
        let interaction = PlaylistInteractionModel {
            safe_feedback: Some(PlaylistSafeFeedback {
                generation: PlaylistSafeFeedbackGeneration(13),
                message: "Не удалось изменить режим".into(),
            }),
            ..PlaylistInteractionModel::default()
        };
        let presentation = PlaylistStatusPresentation::from_models(&model, &interaction)
            .expect("warning/error fixtures create a presentation");
        let actions: Vec<_> = presentation
            .rows()
            .iter()
            .filter_map(PlaylistStatusRow::action)
            .collect();

        assert_eq!(presentation.rows().len(), 5);
        assert_eq!(
            actions,
            vec![
                PlaylistStatusAction::RetrySave,
                PlaylistStatusAction::CancelNavigation {
                    origin_already_ended: false,
                },
            ]
        );
        assert_eq!(
            presentation.rows().last().map(PlaylistStatusRow::tone),
            Some(StatusTone::Error)
        );
    }

    #[test]
    fn blocked_and_faulted_save_states_keep_warning_error_distinction() {
        let blocked = model().with_status_for_test(
            PlaylistProbeView::Idle,
            PlaylistSaveView::Blocked,
            PlaylistNavigationView::Idle,
            PlaylistStartupWarningView::None,
        );
        let faulted = model().with_status_for_test(
            PlaylistProbeView::Idle,
            PlaylistSaveView::Fault(PlaylistPersistenceFault::WorkerDisconnected),
            PlaylistNavigationView::Idle,
            PlaylistStartupWarningView::None,
        );

        let blocked_row =
            PlaylistStatusPresentation::from_models(&blocked, &PlaylistInteractionModel::default())
                .expect("blocked save is visible");
        let faulted_row =
            PlaylistStatusPresentation::from_models(&faulted, &PlaylistInteractionModel::default())
                .expect("faulted save is visible");

        assert_eq!(blocked_row.rows()[0].tone(), StatusTone::Warning);
        assert_eq!(faulted_row.rows()[0].tone(), StatusTone::Error);
    }
}
