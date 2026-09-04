//! Testable production bridge controlled same-item switch-а к strong media lifecycle.

use render_wgpu_shell::Renderer;

use crate::media_open::{MediaOpenRequestId, MediaOpenSourceRequest};
use crate::playlist_runtime::{ActiveMediaIdentity, PlaylistRuntime};
use crate::web_media_stream_model::{
    UrlSidebarPendingSelection, UrlSidebarSafeError, UrlSidebarTransitionError,
    WebMediaStreamGeneration,
};

use super::{
    ActiveMediaSource, AppState, PendingSameItemSwitch, SameItemSwitchError, SameItemSwitchKind,
    StrongMediaOpenError, StrongMediaOpenPoll, UrlSidebarActionApplyOutcome,
    safe_error_for_start_failure, safe_error_for_terminal_failure,
};

/// Полностью подготовленный app-level start без renderer/player implementation details.
pub(super) struct SameItemSwitchAppStart {
    /// Физический reopen request уже собран владельцем source/config.
    pub(super) source_request: MediaOpenSourceRequest,
    /// Exact active item/lineage, которую strong protocol обязан сохранить.
    pub(super) expected_active: ActiveMediaIdentity,
    /// Stable Play/Pause intent берётся из свежего player snapshot-а.
    pub(super) playback_intent: player_core::PlaybackIntent,
    /// Post-Installed effect остаётся app-owned и не утекает в lifecycle port.
    pub(super) kind: SameItemSwitchKind,
}

/// Узкий start port владеет только запуском strong same-lineage lifecycle.
pub(super) trait SameItemSwitchLifecycleStartPort {
    /// Запускает lifecycle, не публикуя selector/preference effects.
    fn begin_same_lineage(
        &mut self,
        source_request: MediaOpenSourceRequest,
        expected_active: ActiveMediaIdentity,
        playback_intent: player_core::PlaybackIntent,
    ) -> Result<MediaOpenRequestId, StrongMediaOpenError>;
}

/// Installed evidence содержит только то, что app path обязан валидировать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct InstalledSameItemSwitchEvidence {
    /// `None` означает нарушение инварианта: strong lifecycle установил source без stream projection.
    generation: Option<WebMediaStreamGeneration>,
    /// Component switch разрешён только с freshly Installed component catalog.
    component_catalog_installed: bool,
}

/// Correlated poll result не раскрывает app path-у worker, renderer или installed handles.
pub(super) enum SameItemSwitchLifecyclePoll {
    /// Strong lifecycle ещё не достиг terminal state.
    Pending,
    /// Exact request установлен; app path может проверить generation и применить effect.
    Installed(InstalledSameItemSwitchEvidence),
    /// Exact request завершился typed failure до либо после barrier.
    Failed(StrongMediaOpenError),
    /// App pending transaction больше не совпадает с authoritative strong request.
    StaleRequest,
}

/// Узкий poll port владеет только request correlation и продвижением lifecycle.
pub(super) trait SameItemSwitchLifecyclePollPort {
    /// Продвигает exact request не более чем на один lifecycle step.
    fn poll_same_lineage(&mut self, request_id: MediaOpenRequestId) -> SameItemSwitchLifecyclePoll;
}

/// App-owned terminal effects отделены от lifecycle implementation.
pub(super) trait SameItemSwitchCompletionOwner {
    /// Возвращает generation всё ещё видимого source для generation-scoped safe error-а.
    fn visible_generation(
        &self,
        previous_generation: WebMediaStreamGeneration,
    ) -> WebMediaStreamGeneration;

    /// Сохраняет item-scoped picker preference только после strong `Installed`.
    fn remember_picker_target(
        &mut self,
        item_id: playlist_core::PlaylistItemId,
        target: crate::web_media_catalog::WebMediaSelectionTarget,
    );

    /// Убирает fallback notice только после успешной установки выбранного target-а.
    fn clear_web_media_fallback_notice(&mut self);
}

/// Selector owner выражает UI intent и не раскрывает controller storage lifecycle port-у.
pub(super) trait SameItemSwitchSelectorOwner {
    /// Публикует pending selector до lifecycle start-а.
    fn record_switch_started(
        &mut self,
        pending: UrlSidebarPendingSelection,
    ) -> Result<(), UrlSidebarTransitionError>;

    /// Откатывает selector, когда lifecycle start не был принят.
    fn record_switch_failed(
        &mut self,
        pending: &UrlSidebarPendingSelection,
        generation: WebMediaStreamGeneration,
        error: UrlSidebarSafeError,
    );

    /// Восстанавливает selector после terminal failure либо invalid Installed evidence.
    fn record_switch_terminal_failed(
        &mut self,
        pending: &UrlSidebarPendingSelection,
        generation: WebMediaStreamGeneration,
        error: UrlSidebarSafeError,
    );

    /// Публикует test-only candidate item override после Installed.
    #[cfg(test)]
    fn record_candidate_switch_installed(
        &mut self,
        generation: WebMediaStreamGeneration,
        item_id: Option<playlist_core::PlaylistItemId>,
        preferred_height: Option<u32>,
    );

    /// Снимает component/picker pending projection после Installed.
    fn record_component_switch_installed(&mut self);
}

/// Start context composition не добавляет lifecycle port-у app-owned операции.
pub(super) trait SameItemSwitchStartContext:
    SameItemSwitchLifecycleStartPort + SameItemSwitchSelectorOwner
{
}

impl<Context> SameItemSwitchStartContext for Context where
    Context: SameItemSwitchLifecycleStartPort + SameItemSwitchSelectorOwner
{
}

/// Poll context composition сохраняет три узких owner boundary раздельными.
pub(super) trait SameItemSwitchPollContext:
    SameItemSwitchLifecyclePollPort + SameItemSwitchCompletionOwner + SameItemSwitchSelectorOwner
{
}

impl<Context> SameItemSwitchPollContext for Context where
    Context: SameItemSwitchLifecyclePollPort
        + SameItemSwitchCompletionOwner
        + SameItemSwitchSelectorOwner
{
}

/// Наблюдаемый результат одного production poll step-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SameItemSwitchAppPoll {
    /// App transaction отсутствовала.
    Idle,
    /// Exact transaction остаётся pending.
    Pending,
    /// Strong Installed прошёл app-level validation и post-install effects опубликованы.
    Installed,
    /// Selector восстановлен с bounded safe error.
    Failed(UrlSidebarSafeError),
}

/// Единый production action path, используемый AppState и functional fake lifecycle tests.
pub(super) struct SameItemSwitchAppPath {
    /// Exact pending transaction остаётся app-owned.
    pending: Option<PendingSameItemSwitch>,
}

impl SameItemSwitchAppPath {
    /// Временно вынимает pending; selector остаётся в `AppState` во время strong call-а.
    ///
    /// Это важно: strong Installed path вызывает `record_installed_media_source`, который сам
    /// обновляет URL controller. Scoped protocol гарантирует возврат pending при обычном return;
    /// восстановление после panic не является его контрактом.
    pub(super) fn take(pending: &mut Option<PendingSameItemSwitch>) -> Self {
        Self {
            pending: pending.take(),
        }
    }

    /// Возвращает app-owned pending после обычного завершения scoped lifecycle call-а.
    pub(super) fn restore(self, pending: &mut Option<PendingSameItemSwitch>) {
        *pending = self.pending;
    }

    /// Запускает exact action через тот же lifecycle port, который использует production.
    pub(super) fn start(
        &mut self,
        start: SameItemSwitchAppStart,
        context: &mut impl SameItemSwitchStartContext,
    ) -> Result<UrlSidebarActionApplyOutcome, SameItemSwitchError> {
        if self.pending.is_some() {
            return Err(SameItemSwitchError::Busy);
        }
        let parent_generation = start.kind.parent_generation();
        let pending_selection = start.kind.pending_selection();
        context
            .record_switch_started(pending_selection.clone())
            .map_err(|UrlSidebarTransitionError::Busy| SameItemSwitchError::Busy)?;
        let request_id = match context.begin_same_lineage(
            start.source_request,
            start.expected_active,
            start.playback_intent,
        ) {
            Ok(request_id) => request_id,
            Err(error) => {
                context.record_switch_failed(
                    &pending_selection,
                    parent_generation,
                    safe_error_for_start_failure(&error),
                );
                return Err(SameItemSwitchError::Strong(error));
            }
        };
        self.pending = Some(PendingSameItemSwitch {
            request_id,
            expected_active: start.expected_active,
            kind: start.kind,
        });
        Ok(UrlSidebarActionApplyOutcome::Started)
    }

    /// Продвигает exact action и публикует preference только после validated Installed.
    pub(super) fn poll<Context>(&mut self, context: &mut Context) -> SameItemSwitchAppPoll
    where
        Context: SameItemSwitchPollContext,
    {
        let Some(pending) = self.pending.take() else {
            return SameItemSwitchAppPoll::Idle;
        };
        let previous_generation = pending.kind.parent_generation();
        let pending_selection = pending.kind.pending_selection();
        match context.poll_same_lineage(pending.request_id) {
            SameItemSwitchLifecyclePoll::Pending => {
                self.pending = Some(pending);
                SameItemSwitchAppPoll::Pending
            }
            SameItemSwitchLifecyclePoll::Installed(evidence) => Self::finish_installed(
                pending,
                pending_selection,
                previous_generation,
                evidence,
                context,
            ),
            SameItemSwitchLifecyclePoll::Failed(error) => {
                let safe_error = safe_error_for_terminal_failure(&error);
                let visible_generation = context.visible_generation(previous_generation);
                context.record_switch_terminal_failed(
                    &pending_selection,
                    visible_generation,
                    safe_error,
                );
                tracing::warn!(
                    request_id = ?pending.request_id,
                    error = %error,
                    "Same-item media switch завершился ошибкой"
                );
                SameItemSwitchAppPoll::Failed(safe_error)
            }
            SameItemSwitchLifecyclePoll::StaleRequest => {
                context.record_switch_terminal_failed(
                    &pending_selection,
                    previous_generation,
                    UrlSidebarSafeError::SameItemSwitchStale,
                );
                tracing::error!(
                    request_id = ?pending.request_id,
                    "Same-item media switch потерял matching strong request"
                );
                SameItemSwitchAppPoll::Failed(UrlSidebarSafeError::SameItemSwitchStale)
            }
        }
    }

    /// Проверяет Installed evidence до единственного app-owned commit point-а.
    fn finish_installed(
        pending: PendingSameItemSwitch,
        pending_selection: crate::web_media_stream_model::UrlSidebarPendingSelection,
        previous_generation: WebMediaStreamGeneration,
        evidence: InstalledSameItemSwitchEvidence,
        owner: &mut impl SameItemSwitchPollContext,
    ) -> SameItemSwitchAppPoll {
        let Some(installed_generation) = evidence.generation else {
            return Self::reject_invalid_installed(
                &pending,
                &pending_selection,
                previous_generation,
                "installed_source_not_yt_dlp",
                owner,
            );
        };
        if !installed_generation.has_same_source_lineage(previous_generation) {
            return Self::reject_invalid_installed(
                &pending,
                &pending_selection,
                installed_generation,
                "installed_source_lineage_mismatch",
                owner,
            );
        }
        if matches!(pending.kind, SameItemSwitchKind::Component(_))
            && !evidence.component_catalog_installed
        {
            return Self::reject_invalid_installed(
                &pending,
                &pending_selection,
                installed_generation,
                "component_catalog_not_installed",
                owner,
            );
        }
        match pending.kind {
            #[cfg(test)]
            SameItemSwitchKind::Candidate {
                preferred_height, ..
            } => {
                owner.record_candidate_switch_installed(
                    installed_generation,
                    pending.expected_active.item_id(),
                    preferred_height,
                );
            }
            SameItemSwitchKind::Component(_) => {
                owner.record_component_switch_installed();
            }
            SameItemSwitchKind::Picker { target, .. }
            | SameItemSwitchKind::AutomaticPicker { target, .. } => {
                owner.record_component_switch_installed();
                if let Some(item_id) = pending.expected_active.item_id() {
                    owner.remember_picker_target(item_id, target);
                }
                owner.clear_web_media_fallback_notice();
            }
            SameItemSwitchKind::AdaptiveQuality { .. } => {
                // Runtime adaptation меняет только installed exact target. Persisted item
                // preference остаётся `Automatic`, иначе следующий reopen стал бы ручным.
                owner.record_component_switch_installed();
                owner.clear_web_media_fallback_notice();
            }
        }
        SameItemSwitchAppPoll::Installed
    }

    /// Восстанавливает selector при malformed Installed terminal без partial preference commit-а.
    fn reject_invalid_installed(
        pending: &PendingSameItemSwitch,
        pending_selection: &crate::web_media_stream_model::UrlSidebarPendingSelection,
        visible_generation: WebMediaStreamGeneration,
        invariant: &'static str,
        selector: &mut impl SameItemSwitchSelectorOwner,
    ) -> SameItemSwitchAppPoll {
        selector.record_switch_terminal_failed(
            pending_selection,
            visible_generation,
            UrlSidebarSafeError::SameItemSwitchStale,
        );
        tracing::error!(
            request_id = ?pending.request_id,
            invariant,
            "Same-item media switch получил invalid Installed evidence"
        );
        SameItemSwitchAppPoll::Failed(UrlSidebarSafeError::SameItemSwitchStale)
    }
}

/// Production start context вызывает существующий strong start без альтернативного protocol-а.
pub(super) struct ProductionSameItemSwitchStartContext<'app, 'runtime, 'renderer> {
    app_state: &'app mut AppState,
    playlist_runtime: &'runtime mut PlaylistRuntime,
    renderer: &'renderer Renderer,
}

/// Внутренний projection даёт общему selector impl доступ только к его AppState owner-у.
trait ProductionSameItemSwitchSelectorContext {
    /// Возвращает app owner исключительно для intent-shaped selector операций.
    fn app_state_for_selector(&mut self) -> &mut AppState;
}

impl<Context> SameItemSwitchSelectorOwner for Context
where
    Context: ProductionSameItemSwitchSelectorContext,
{
    fn record_switch_started(
        &mut self,
        pending: UrlSidebarPendingSelection,
    ) -> Result<(), UrlSidebarTransitionError> {
        self.app_state_for_selector()
            .url_sidebar_controller
            .record_switch_started(pending)
    }

    fn record_switch_failed(
        &mut self,
        pending: &UrlSidebarPendingSelection,
        generation: WebMediaStreamGeneration,
        error: UrlSidebarSafeError,
    ) {
        let _cleared = self
            .app_state_for_selector()
            .url_sidebar_controller
            .record_switch_failed(pending, generation, error);
    }

    fn record_switch_terminal_failed(
        &mut self,
        pending: &UrlSidebarPendingSelection,
        generation: WebMediaStreamGeneration,
        error: UrlSidebarSafeError,
    ) {
        let _restored = self
            .app_state_for_selector()
            .url_sidebar_controller
            .record_switch_terminal_failed(pending, generation, error);
    }

    #[cfg(test)]
    fn record_candidate_switch_installed(
        &mut self,
        generation: WebMediaStreamGeneration,
        item_id: Option<playlist_core::PlaylistItemId>,
        preferred_height: Option<u32>,
    ) {
        self.app_state_for_selector()
            .url_sidebar_controller
            .record_candidate_switch_installed(generation, item_id, preferred_height);
    }

    fn record_component_switch_installed(&mut self) {
        self.app_state_for_selector()
            .url_sidebar_controller
            .record_component_switch_installed();
    }
}

impl<'app, 'runtime, 'renderer> ProductionSameItemSwitchStartContext<'app, 'runtime, 'renderer> {
    /// Собирает scoped adapter после временного извлечения app path state.
    pub(super) fn new(
        app_state: &'app mut AppState,
        playlist_runtime: &'runtime mut PlaylistRuntime,
        renderer: &'renderer Renderer,
    ) -> Self {
        Self {
            app_state,
            playlist_runtime,
            renderer,
        }
    }
}

impl SameItemSwitchLifecycleStartPort for ProductionSameItemSwitchStartContext<'_, '_, '_> {
    fn begin_same_lineage(
        &mut self,
        source_request: MediaOpenSourceRequest,
        expected_active: ActiveMediaIdentity,
        playback_intent: player_core::PlaybackIntent,
    ) -> Result<MediaOpenRequestId, StrongMediaOpenError> {
        self.app_state.begin_same_lineage_source_media_strong(
            self.playlist_runtime,
            self.renderer,
            source_request,
            expected_active,
            playback_intent,
        )
    }
}

impl ProductionSameItemSwitchSelectorContext for ProductionSameItemSwitchStartContext<'_, '_, '_> {
    fn app_state_for_selector(&mut self) -> &mut AppState {
        self.app_state
    }
}

/// Production poll context объединяет два раздельных boundary: lifecycle и app effects.
pub(super) struct ProductionSameItemSwitchPollContext<'app, 'runtime> {
    app_state: &'app mut AppState,
    playlist_runtime: &'runtime mut PlaylistRuntime,
}

impl<'app, 'runtime> ProductionSameItemSwitchPollContext<'app, 'runtime> {
    /// Создаёт scoped poll adapter без renderer dependency.
    pub(super) fn new(
        app_state: &'app mut AppState,
        playlist_runtime: &'runtime mut PlaylistRuntime,
    ) -> Self {
        Self {
            app_state,
            playlist_runtime,
        }
    }
}

impl SameItemSwitchLifecyclePollPort for ProductionSameItemSwitchPollContext<'_, '_> {
    fn poll_same_lineage(&mut self, request_id: MediaOpenRequestId) -> SameItemSwitchLifecyclePoll {
        let matching_request = self
            .app_state
            .pending_strong_media_open
            .as_ref()
            .is_some_and(|pending| pending.request_id() == request_id);
        if !matching_request {
            return SameItemSwitchLifecyclePoll::StaleRequest;
        }
        match self
            .app_state
            .poll_prepared_media_strong(self.playlist_runtime)
        {
            StrongMediaOpenPoll::Pending => SameItemSwitchLifecyclePoll::Pending,
            StrongMediaOpenPoll::Installed(installed) => {
                let installed_configuration = installed
                    .source
                    .physical_source()
                    .web_media_stream_configuration();
                SameItemSwitchLifecyclePoll::Installed(InstalledSameItemSwitchEvidence {
                    generation: installed_configuration.map(
                        crate::web_media_stream_model::WebMediaStreamConfiguration::generation,
                    ),
                    component_catalog_installed: installed_configuration.is_some_and(
                        |configuration| {
                            matches!(
                                configuration.component_variant_projection(),
                                crate::web_media_stream_model::component_variants::WebMediaComponentVariantProjection::Installed(_)
                            )
                        },
                    ),
                })
            }
            StrongMediaOpenPoll::Failed(error) => SameItemSwitchLifecyclePoll::Failed(*error),
        }
    }
}

impl ProductionSameItemSwitchSelectorContext for ProductionSameItemSwitchPollContext<'_, '_> {
    fn app_state_for_selector(&mut self) -> &mut AppState {
        self.app_state
    }
}

impl SameItemSwitchCompletionOwner for ProductionSameItemSwitchPollContext<'_, '_> {
    fn visible_generation(
        &self,
        previous_generation: WebMediaStreamGeneration,
    ) -> WebMediaStreamGeneration {
        self.app_state
            .active_media_source
            .as_ref()
            .and_then(ActiveMediaSource::web_media_stream_generation)
            .unwrap_or(previous_generation)
    }

    fn remember_picker_target(
        &mut self,
        item_id: playlist_core::PlaylistItemId,
        target: crate::web_media_catalog::WebMediaSelectionTarget,
    ) {
        if let Some(preference) = target.remembered() {
            self.playlist_runtime
                .remember_web_media_preference(item_id, preference);
        }
    }

    fn clear_web_media_fallback_notice(&mut self) {
        self.app_state.web_media_fallback_notice = false;
    }
}

#[cfg(test)]
mod tests;
