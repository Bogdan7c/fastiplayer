//! Исполнение действий таймлайна: шаги плана → команды плееру.
//!
//! `AppState` — composition root: жест (`ui::timeline`) даёт намерение, план
//! (`state::timeline_scrub_plan`) раскладывает его на шаги по охвату живого скраба
//! вида окна, [`timeline_step_commands`] превращает шаги в `PlayerCommand` (здесь же
//! диспетчер жеста — троттлинг и ожидание посадки, `ui::timeline::live_scrub`), а
//! `AppState` отправляет их через единую точку `send_transport_command` (в окне
//! браузерного режима точная перемотка там становится перемоткой зрителя, а команды
//! скраба идут плееру окна напрямую). Политика посадки — у player-core.
//!
//! Вынесено из `state/ui_runtime.rs` (сессия 04a-fix): тот был у предела размера
//! модуля, а логика отправки скраба выросла охватом «только по скачанному».

use std::time::Instant;

use media_core::MediaTime;
use player_core::{PlayerCommand, ScrubCommitPolicy, SeekRequest};
use tracing::{debug, warn};

use super::AppState;
use super::timeline_scrub_plan::{
    PlayerScrubStanding, ScrubRelease, TimelinePlanContext, TimelineStep, plan_timeline_action,
};
use crate::ui::timeline::{TimelineAction, TimelineLiveScrubSettingsSnapshot, TimelineUiState};
use crate::window_view_profile::LiveScrubScope;

/// Diagnostic route пользовательского timeline intent-а на границе app-egui -> player-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimelineCommandRoute {
    /// Одиночный click ушёл в exact final seek без scrub generation.
    ClickSeek,

    /// Pointer drag release ушёл как exact final seek без scrub generation.
    DragSeek,

    /// Отпускание живого скраба вне скачанного (окно браузерного режима): одна
    /// обычная точная перемотка зрителя.
    ReleaseOutsideDownloaded,

    /// Pointer-down начал live scrub gesture.
    LiveScrubBegin,

    /// Live scrub latest target ушёл в preview route.
    LiveScrubPreview,

    /// Release завершил active live scrub route.
    LiveScrubEnd,
}

impl TimelineCommandRoute {
    /// Возвращает стабильное имя route-а для logs/diagnostics.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::ClickSeek => "click-seek",
            Self::DragSeek => "drag-seek",
            Self::ReleaseOutsideDownloaded => "release-outside-downloaded",
            Self::LiveScrubBegin => "live-scrub-begin",
            Self::LiveScrubPreview => "live-scrub-preview",
            Self::LiveScrubEnd => "live-scrub-end",
        }
    }
}

/// Возвращает player-owned commit policy для конкретного release intent-а timeline.
pub(super) const fn live_scrub_release_policy_from_action(
    action: TimelineAction,
) -> Option<ScrubCommitPolicy> {
    match action {
        TimelineAction::EndLiveScrubAtLatestTarget(_) => {
            Some(ScrubCommitPolicy::CommitLatestTarget)
        }
        TimelineAction::EndLiveScrubAtVisiblePreview(_) => {
            Some(ScrubCommitPolicy::CommitVisiblePreview)
        }
        TimelineAction::ClickSeek(_)
        | TimelineAction::CommitDragSeek(_)
        | TimelineAction::BeginLiveScrub(_)
        | TimelineAction::PreviewLiveScrub(_)
        | TimelineAction::CancelLiveScrub => None,
    }
}

/// Начат ли живой скраб у плеера в текущем жесте: диспетчер жеста существует ровно
/// с «начать» до «завершить».
pub(super) const fn player_scrub_standing(timeline: &TimelineUiState) -> PlayerScrubStanding {
    if timeline.has_live_scrub_dispatch() {
        PlayerScrubStanding::Started
    } else {
        PlayerScrubStanding::NotStarted
    }
}

/// Команды плееру для шагов плана, по порядку, вместе с маршрутом для журнала.
///
/// Здесь же двигается диспетчер жеста: «начать» заводит его со снимком настроек
/// `settings`, цель проходит через его троттлинг (цель, которую он придержал, плееру
/// не уходит), «завершить» его сбрасывает.
pub(super) fn timeline_step_commands(
    steps: &[TimelineStep],
    timeline: &mut TimelineUiState,
    settings: TimelineLiveScrubSettingsSnapshot,
    now: Instant,
) -> Vec<(PlayerCommand, TimelineCommandRoute)> {
    let mut commands = Vec::new();
    for step in steps {
        match *step {
            TimelineStep::ExactSeek { target, route } => {
                commands.push((PlayerCommand::Seek(SeekRequest::absolute(target)), route));
            }
            TimelineStep::BeginLiveScrub(target) => {
                let live_scrub = timeline.begin_live_scrub_dispatch(settings, now, target);
                commands.push((
                    PlayerCommand::begin_live_scrub(live_scrub),
                    TimelineCommandRoute::LiveScrubBegin,
                ));
                commands.push((
                    PlayerCommand::preview_live_scrub(SeekRequest::absolute(target), live_scrub),
                    TimelineCommandRoute::LiveScrubPreview,
                ));
            }
            TimelineStep::PreviewLiveScrub(target) => {
                if let Some(target) = timeline.live_scrub_preview_dispatch_target(now, target) {
                    commands.push((
                        preview_command(timeline, target),
                        TimelineCommandRoute::LiveScrubPreview,
                    ));
                }
            }
            TimelineStep::EndLiveScrub { policy, release } => {
                if let ScrubRelease::LandExactlyAt(release_target) = release
                    && let Some(target) =
                        timeline.live_scrub_release_dispatch_target(now, release_target)
                {
                    commands.push((
                        preview_command(timeline, target),
                        TimelineCommandRoute::LiveScrubPreview,
                    ));
                }
                let end = match timeline.live_scrub_diagnostics() {
                    Some(live_scrub) => PlayerCommand::end_live_scrub(policy, live_scrub),
                    None => PlayerCommand::end_scrub(policy),
                };
                commands.push((end, TimelineCommandRoute::LiveScrubEnd));
                timeline.clear_live_scrub_dispatch();
            }
        }
    }
    commands
}

/// Команда цели скраба: с диагностикой жеста, если диспетчер его ведёт.
fn preview_command(timeline: &TimelineUiState, target: MediaTime) -> PlayerCommand {
    match timeline.live_scrub_diagnostics() {
        Some(live_scrub) => {
            PlayerCommand::preview_live_scrub(SeekRequest::absolute(target), live_scrub)
        }
        None => PlayerCommand::preview_scrub(SeekRequest::absolute(target)),
    }
}

impl AppState {
    /// Включён ли живой жест таймлайна в этом проходе.
    ///
    /// Настройка пользователя главнее; вид окна только сужает её: в окне браузерного
    /// режима живой жест есть, лишь пока известно, что скачано у показанного медиа, —
    /// иначе перетаскивание как без живого скраба (одна перемотка на отпускании).
    pub(super) fn live_scrub_gesture_enabled(&self) -> bool {
        if !self.committed_config_snapshot.live_scrub_enabled() {
            return false;
        }
        match self.view_profile().live_scrub_scope() {
            LiveScrubScope::Anywhere => true,
            LiveScrubScope::DownloadedRangeOnly => self.downloaded_range().is_known(),
        }
    }

    /// Конвертирует pointer timeline action в typed player command(s).
    ///
    /// Обычные click/disabled-drag actions остаются exact `Seek`. Live drag
    /// идёт через Begin/Preview/End scrub route и не отправляет ordinary seek —
    /// кроме отпускания вне скачанного в окне браузерного режима (план —
    /// `timeline_scrub_plan`).
    pub(super) fn send_timeline_action(&mut self, action: TimelineAction) {
        self.clear_timeline_inline_status_for_action();
        let steps = plan_timeline_action(
            action,
            &TimelinePlanContext {
                scope: self.view_profile().live_scrub_scope(),
                standing: player_scrub_standing(&self.timeline_ui_state),
                downloaded: self.downloaded_range(),
            },
        );
        let settings = self.live_scrub_settings_snapshot();
        let commands = timeline_step_commands(
            &steps,
            &mut self.timeline_ui_state,
            settings,
            Instant::now(),
        );
        for (command, route) in commands {
            self.send_timeline_player_command(action, route, command);
        }
    }

    /// Отправляет одну timeline command с единым diagnostics/log path.
    fn send_timeline_player_command(
        &mut self,
        action: TimelineAction,
        route: TimelineCommandRoute,
        command: PlayerCommand,
    ) {
        let command_for_diagnostics = command.clone();

        if let Err(error) = self.send_transport_command(command) {
            warn!(error = %error, "Could not send the timeline command");
            return;
        }

        debug!(
            action = ?action,
            command = ?command_for_diagnostics,
            route = route.as_str(),
            "Timeline action sent to player worker"
        );
        self.mark_pending_worker_redraw();
    }
}

#[cfg(test)]
#[path = "timeline_dispatch_tests.rs"]
mod tests;
