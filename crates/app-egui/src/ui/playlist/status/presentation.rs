//! Owned privacy-safe представление единой Playlist status-области.

use std::sync::Arc;

use crate::playlist_runtime::{
    PlaylistInteractionModel, PlaylistLoadingView, PlaylistNavigationView, PlaylistProbeView,
    PlaylistProgressModel, PlaylistSaveView, PlaylistStartupWarningView, PlaylistViewModel,
    PlaylistWaitDirection,
};

use super::super::actions::PlaylistAction;

/// Полностью owned snapshot коротких строк, пригодный для остаточной отрисовки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlaylistStatusPresentation {
    /// Строки уже расположены в продуктовом порядке, поэтому renderer не знает runtime policy.
    rows: Vec<PlaylistStatusRow>,
}

impl PlaylistStatusPresentation {
    /// Собирает один bounded snapshot без URL, filesystem path и mutable runtime state.
    pub(super) fn from_models(
        model: &PlaylistViewModel,
        interaction: &PlaylistInteractionModel,
    ) -> Option<Self> {
        let mut presentation = Self { rows: Vec::new() };

        // Lifecycle первым объясняет состояние всей области.
        presentation.push_lifecycle(model);
        // Foreground progress и его результаты идут раньше фоновой persistence.
        presentation.push_progress_and_results(interaction, model.probe());
        // Persistence сохраняет отдельные retryable, blocked и fatal состояния.
        presentation.push_persistence(interaction, model.save());
        // Navigation завершает блок, поскольку зависит от уже понятного состояния очереди.
        presentation.push_navigation(interaction, model.navigation());

        (!presentation.rows.is_empty()).then_some(presentation)
    }

    /// Возвращает упорядоченные строки только renderer-у status owner-а.
    pub(super) fn rows(&self) -> &[PlaylistStatusRow] {
        &self.rows
    }

    /// Даёт renderer tests одну tombstone-строку без создания runtime tombstone.
    #[cfg(test)]
    pub(super) fn tombstone_for_test() -> Self {
        Self {
            rows: vec![PlaylistStatusRow::message(
                "Сейчас играет — удалено из очереди",
                StatusTone::Normal,
                StatusRowKind::Tombstone,
            )],
        }
    }

    /// Добавляет tombstone/loading/structural/startup состояния без runtime деталей.
    fn push_lifecycle(&mut self, model: &PlaylistViewModel) {
        if model.has_active_tombstone() {
            self.rows.push(PlaylistStatusRow::message(
                "Сейчас играет — удалено из очереди",
                StatusTone::Normal,
                StatusRowKind::Tombstone,
            ));
        }
        if matches!(model.loading(), PlaylistLoadingView::Loading) {
            self.rows.push(PlaylistStatusRow::message(
                "Загрузка сохранённой очереди…",
                StatusTone::Normal,
                StatusRowKind::Loading,
            ));
        }
        if model
            .structural_action_availability()
            .requires_status_notice()
        {
            self.rows.push(PlaylistStatusRow::message(
                "Изменения очереди недоступны",
                StatusTone::Normal,
                StatusRowKind::Weak,
            ));
        }
        if matches!(model.startup_warning(), PlaylistStartupWarningView::Present) {
            self.rows.push(PlaylistStatusRow::message(
                "Сохранённая очередь восстановлена с предупреждением",
                StatusTone::Warning,
                StatusRowKind::Normal,
            ));
        }
    }

    /// Добавляет foreground progress, terminal discovery и safe feedback без дублей.
    fn push_progress_and_results(
        &mut self,
        interaction: &PlaylistInteractionModel,
        probe: PlaylistProbeView,
    ) {
        if let Some(progress) = &interaction.progress {
            self.rows.push(PlaylistStatusRow::with_action(
                progress_text(progress),
                StatusTone::Normal,
                StatusRowKind::Normal,
                PlaylistStatusAction::CancelProgress(progress.cancel_scope),
            ));
        }

        // Interaction progress уже выражает active discovery точнее read-only probe summary.
        let progress_replaces_probe = interaction.progress.is_some()
            && matches!(
                probe,
                PlaylistProbeView::Enumerating
                    | PlaylistProbeView::Probing { .. }
                    | PlaylistProbeView::ManualProbe { .. }
            );
        if !progress_replaces_probe && let Some((message, tone)) = probe_message(probe) {
            self.rows.push(PlaylistStatusRow::message(
                message,
                tone,
                StatusRowKind::Normal,
            ));
        }

        if let Some(summary) = &interaction.completion_summary {
            self.rows.push(PlaylistStatusRow::message(
                Arc::clone(summary),
                StatusTone::Normal,
                StatusRowKind::Normal,
            ));
        }
        if let Some(details) = &interaction.completion_details {
            self.rows.push(PlaylistStatusRow::message(
                Arc::clone(details),
                StatusTone::Normal,
                StatusRowKind::Small,
            ));
        }
        if let Some(feedback) = &interaction.safe_feedback {
            self.rows.push(PlaylistStatusRow::message(
                Arc::clone(feedback),
                StatusTone::Warning,
                StatusRowKind::Normal,
            ));
        }
    }

    /// Добавляет persistence status, заменяя retryable read-only warning действием Retry.
    fn push_persistence(&mut self, interaction: &PlaylistInteractionModel, save: PlaylistSaveView) {
        if interaction.save_retry_available {
            self.rows.push(PlaylistStatusRow::with_action(
                "Не удалось сохранить плейлист",
                StatusTone::Warning,
                StatusRowKind::Normal,
                PlaylistStatusAction::RetrySave,
            ));
        }

        // Retry-кнопка уже полностью объясняет тот же retryable warning.
        let retry_replaces_warning = interaction.save_retry_available
            && matches!(save, PlaylistSaveView::WarningRetryAvailable { .. });
        if !retry_replaces_warning && let Some((message, tone)) = save_message(save) {
            self.rows.push(PlaylistStatusRow::message(
                message,
                tone,
                StatusRowKind::Weak,
            ));
        }
    }

    /// Добавляет direction-specific wait, terminal navigation и доступную отмену.
    fn push_navigation(
        &mut self,
        interaction: &PlaylistInteractionModel,
        navigation: PlaylistNavigationView,
    ) {
        if let Some(direction) = interaction.wait_direction {
            self.rows.push(PlaylistStatusRow::message(
                wait_message(direction),
                StatusTone::Normal,
                StatusRowKind::Normal,
            ));
        }

        // Ручное направление точнее общего WaitingForCandidate и заменяет только его.
        let direction_replaces_generic_wait = interaction.wait_direction.is_some()
            && matches!(navigation, PlaylistNavigationView::WaitingForCandidate);
        if !direction_replaces_generic_wait
            && let Some((message, tone)) = navigation_message(navigation)
        {
            self.rows.push(PlaylistStatusRow::message(
                message,
                tone,
                StatusRowKind::Normal,
            ));
        }

        if interaction.navigation_cancel_available {
            self.rows.push(PlaylistStatusRow::action_only(
                PlaylistStatusAction::CancelNavigation {
                    origin_already_ended: interaction.awaiting_failure_origin_ended,
                },
            ));
        }
    }
}

/// Одна status-строка отделяет текстовую семантику от egui layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlaylistStatusRow {
    /// `None` допустим только для отдельной action-row вроде Cancel Navigation.
    message: Option<Arc<str>>,
    /// Tone выбирает theme-owned foreground без hardcoded RGB.
    tone: StatusTone,
    /// Kind выражает spinner/strong/small/weak presentation.
    kind: StatusRowKind,
    /// Action остаётся узким status-only intent и позже переводится в существующий API.
    action: Option<PlaylistStatusAction>,
}

impl PlaylistStatusRow {
    /// Создаёт обычную read-only строку.
    fn message(message: impl Into<Arc<str>>, tone: StatusTone, kind: StatusRowKind) -> Self {
        Self {
            message: Some(message.into()),
            tone,
            kind,
            action: None,
        }
    }

    /// Создаёт строку, где действие относится ровно к показанному сообщению.
    fn with_action(
        message: impl Into<Arc<str>>,
        tone: StatusTone,
        kind: StatusRowKind,
        action: PlaylistStatusAction,
    ) -> Self {
        Self {
            message: Some(message.into()),
            tone,
            kind,
            action: Some(action),
        }
    }

    /// Создаёт отдельную action-row без выдуманного повторного сообщения.
    fn action_only(action: PlaylistStatusAction) -> Self {
        Self {
            message: None,
            tone: StatusTone::Normal,
            kind: StatusRowKind::Normal,
            action: Some(action),
        }
    }

    /// Возвращает privacy-safe видимый текст либо отсутствие message.
    pub(super) fn text(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Возвращает theme tone строки.
    pub(super) const fn tone(&self) -> StatusTone {
        self.tone
    }

    /// Возвращает layout-kind строки.
    pub(super) const fn kind(&self) -> StatusRowKind {
        self.kind
    }

    /// Возвращает status-only action для authoritative renderer-а.
    pub(super) const fn action(&self) -> Option<PlaylistStatusAction> {
        self.action
    }
}

/// Theme tone не содержит конкретных цветов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui::playlist) enum StatusTone {
    Normal,
    Warning,
    Error,
}

/// Визуальная роль строки остаётся понятной на месте render-вызова.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusRowKind {
    Normal,
    Weak,
    Small,
    Tombstone,
    Loading,
}

/// Узкий набор действий гарантирует, что retained status не может хранить URL draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaylistStatusAction {
    CancelProgress(crate::playlist_runtime::PlaylistProgressCancelScope),
    RetrySave,
    CancelNavigation { origin_already_ended: bool },
}

impl PlaylistStatusAction {
    /// Возвращает короткую локализованную подпись кнопки.
    pub(super) const fn button_label(self) -> &'static str {
        match self {
            Self::CancelProgress(_) => "Отмена",
            Self::RetrySave => "Повторить",
            Self::CancelNavigation { .. } => "Отменить переход",
        }
    }

    /// Возвращает tooltip только там, где важно объяснить lifecycle после отмены.
    pub(super) const fn tooltip(self) -> Option<&'static str> {
        match self {
            Self::CancelNavigation {
                origin_already_ended: true,
            } => Some("Отменить переход; завершившееся воспроизведение останется остановленным"),
            Self::CancelNavigation {
                origin_already_ended: false,
            } => Some("Отменить только ожидающий переход"),
            Self::CancelProgress(_) | Self::RetrySave => None,
        }
    }

    /// Переводит status-only intent в уже существующий post-render action API.
    pub(super) const fn into_playlist_action(self) -> PlaylistAction {
        match self {
            Self::CancelProgress(cancel_scope) => PlaylistAction::CancelProgress(cancel_scope),
            Self::RetrySave => PlaylistAction::RetrySave,
            Self::CancelNavigation { .. } => PlaylistAction::CancelNavigation,
        }
    }
}

/// Форматирует bounded progress без locator-ов и произвольных runtime payload.
fn progress_text(progress: &PlaylistProgressModel) -> String {
    progress.total.map_or_else(
        || format!("{}: {}", progress.stage, progress.processed),
        |total| format!("{}: {} из {total}", progress.stage, progress.processed),
    )
}

/// Форматирует probe/discovery и сохраняет terminal success/warning.
fn probe_message(probe: PlaylistProbeView) -> Option<(String, StatusTone)> {
    match probe {
        PlaylistProbeView::Idle => None,
        PlaylistProbeView::Enumerating => {
            Some(("Поиск файлов рядом…".to_owned(), StatusTone::Normal))
        }
        PlaylistProbeView::Probing { processed, total } => Some((
            format!("Проверка файлов: {processed} из {total}"),
            StatusTone::Normal,
        )),
        PlaylistProbeView::ManualProbe { processed, total } => Some((
            format!("Проверка добавляемых файлов: {processed} из {total}"),
            StatusTone::Normal,
        )),
        PlaylistProbeView::Completed => {
            Some(("Проверка файлов завершена".to_owned(), StatusTone::Normal))
        }
        PlaylistProbeView::Warning => Some((
            "Добавлена только доступная часть очереди; проверка завершилась с предупреждением"
                .to_owned(),
            StatusTone::Warning,
        )),
    }
}

/// Форматирует persistence и не показывает обычное фоновое сохранение.
pub(in crate::ui::playlist) fn save_message(
    save: PlaylistSaveView,
) -> Option<(String, StatusTone)> {
    match save {
        PlaylistSaveView::Idle | PlaylistSaveView::Saving => None,
        PlaylistSaveView::WarningRetryAvailable { occurrence_count } => Some((
            format!(
                "Не удалось сохранить очередь (попыток: {occurrence_count}). Повтор сохранения доступен"
            ),
            StatusTone::Warning,
        )),
        PlaylistSaveView::Blocked => Some((
            "Сохранение очереди заблокировано для защиты существующего файла".to_owned(),
            StatusTone::Warning,
        )),
        PlaylistSaveView::Fault(_fault) => Some((
            "Служба сохранения очереди недоступна".to_owned(),
            StatusTone::Error,
        )),
    }
}

/// Форматирует navigation, сохраняя terminal completion/cancel/fault состояния.
pub(in crate::ui::playlist) const fn navigation_message(
    navigation: PlaylistNavigationView,
) -> Option<(&'static str, StatusTone)> {
    match navigation {
        PlaylistNavigationView::Idle => None,
        PlaylistNavigationView::WaitingForCandidate => Some((
            "Ожидание следующего доступного элемента…",
            StatusTone::Normal,
        )),
        PlaylistNavigationView::AwaitingUserAfterFailure => Some((
            "Переход не выполнен. Автоматический переход остановлен; Next, Previous или повтор продолжат сохранённый курсор",
            StatusTone::Error,
        )),
        PlaylistNavigationView::Exhausted => {
            Some(("Подходящих элементов больше нет", StatusTone::Normal))
        }
        PlaylistNavigationView::Cancelled => {
            Some(("Ожидание перехода отменено", StatusTone::Normal))
        }
        PlaylistNavigationView::Fatal => {
            Some(("Переход недоступен из-за ошибки службы", StatusTone::Error))
        }
    }
}

/// Форматирует ручное направление D50 без общего двусмысленного ожидания.
const fn wait_message(direction: PlaylistWaitDirection) -> &'static str {
    match direction {
        PlaylistWaitDirection::Next => "Ищу следующий трек…",
        PlaylistWaitDirection::Previous => "Ищу предыдущий трек…",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use playlist_core::{
        CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind, PlaylistQueue,
    };

    use super::*;
    use crate::playlist_runtime::{PlaylistProgressCancelScope, PlaylistProgressModel};

    /// Создаёт interaction snapshot с одним foreground progress.
    fn progress_interaction() -> PlaylistInteractionModel {
        PlaylistInteractionModel {
            progress: Some(PlaylistProgressModel {
                stage: Arc::from("Проверка файлов"),
                processed: 3,
                total: Some(7),
                cancel_scope: PlaylistProgressCancelScope::ManualAdd,
            }),
            ..PlaylistInteractionModel::default()
        }
    }

    /// Собирает все непустые строки для точных suppression assertions.
    fn visible_messages(presentation: &PlaylistStatusPresentation) -> Vec<&str> {
        presentation
            .rows()
            .iter()
            .filter_map(PlaylistStatusRow::text)
            .collect()
    }

    #[test]
    fn interaction_progress_replaces_only_active_probe_rows() {
        for probe in [
            PlaylistProbeView::Enumerating,
            PlaylistProbeView::Probing {
                processed: 3,
                total: 7,
            },
            PlaylistProbeView::ManualProbe {
                processed: 3,
                total: 7,
            },
        ] {
            let mut presentation = PlaylistStatusPresentation { rows: Vec::new() };
            presentation.push_progress_and_results(&progress_interaction(), probe);

            assert_eq!(
                visible_messages(&presentation),
                vec!["Проверка файлов: 3 из 7"]
            );
        }

        // Terminal discovery не теряется даже при соседнем operation result.
        let mut terminal = PlaylistStatusPresentation { rows: Vec::new() };
        terminal.push_progress_and_results(&progress_interaction(), PlaylistProbeView::Completed);
        assert_eq!(
            visible_messages(&terminal),
            vec!["Проверка файлов: 3 из 7", "Проверка файлов завершена"]
        );
    }

    #[test]
    fn retry_and_direction_replace_only_their_read_only_duplicates() {
        let retry_interaction = PlaylistInteractionModel {
            save_retry_available: true,
            wait_direction: Some(PlaylistWaitDirection::Previous),
            ..PlaylistInteractionModel::default()
        };
        let mut presentation = PlaylistStatusPresentation { rows: Vec::new() };

        presentation.push_persistence(
            &retry_interaction,
            PlaylistSaveView::WarningRetryAvailable {
                occurrence_count: 4,
            },
        );
        presentation.push_navigation(
            &retry_interaction,
            PlaylistNavigationView::WaitingForCandidate,
        );

        assert_eq!(
            visible_messages(&presentation),
            vec!["Не удалось сохранить плейлист", "Ищу предыдущий трек…"]
        );
        assert_eq!(
            presentation
                .rows()
                .iter()
                .filter_map(PlaylistStatusRow::action)
                .collect::<Vec<_>>(),
            vec![PlaylistStatusAction::RetrySave]
        );
    }

    #[test]
    fn terminal_tones_and_navigation_cancel_semantics_are_preserved() {
        let interaction = PlaylistInteractionModel {
            navigation_cancel_available: true,
            awaiting_failure_origin_ended: true,
            ..PlaylistInteractionModel::default()
        };
        let mut presentation = PlaylistStatusPresentation { rows: Vec::new() };

        presentation.push_progress_and_results(
            &PlaylistInteractionModel::default(),
            PlaylistProbeView::Warning,
        );
        presentation.push_persistence(
            &PlaylistInteractionModel::default(),
            PlaylistSaveView::Fault(
                crate::playlist_runtime::PlaylistPersistenceFault::WorkerDisconnected,
            ),
        );
        presentation.push_navigation(&interaction, PlaylistNavigationView::Fatal);

        assert_eq!(presentation.rows()[0].tone(), StatusTone::Warning);
        assert_eq!(presentation.rows()[1].tone(), StatusTone::Error);
        assert_eq!(presentation.rows()[2].tone(), StatusTone::Error);
        let cancel = presentation.rows()[3]
            .action()
            .expect("terminal navigation fixture must keep Cancel Navigation");
        assert!(
            cancel
                .tooltip()
                .expect("ended-origin Cancel must explain stopped playback")
                .contains("останется остановленным")
        );
    }

    #[test]
    fn owned_presentation_does_not_retain_local_locator_text() {
        let secret_path = PathBuf::from("/private/media/secret-status-track.mp3");
        let mut queue = PlaylistQueue::new();
        queue
            .append_batch(vec![PlaylistItemDraft::local(
                LocalLocator::Native(secret_path.clone()),
                None,
                CachedPlaylistMetadata::new("Безопасный заголовок", PlaylistMediaKind::Audio),
            )])
            .expect("single status fixture must fit the playlist hard cap");
        let model =
            PlaylistViewModel::for_queue_with_revision(&queue, 1, PlaylistLoadingView::Loading);
        let presentation =
            PlaylistStatusPresentation::from_models(&model, &PlaylistInteractionModel::default())
                .expect("loading model must build one safe status");
        let retained_debug = format!("{presentation:?}");

        assert!(!retained_debug.contains(secret_path.to_string_lossy().as_ref()));
        assert!(!retained_debug.contains("secret-status-track.mp3"));
    }
}
