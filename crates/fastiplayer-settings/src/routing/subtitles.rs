//! Route настроек субтитров (`subtitles.apply`).
//!
//! Пока у субтитров нет живого runtime-потребителя: route доставляет committed
//! snapshot секции `[subtitles]`, а владелец (app-слой субтитров) читает его при
//! открытии следующего медиа и при пересоздании сессии субтитров. Изменение
//! настроек не трогает ни player, ни renderer, поэтому route не требует preflight.

use fastiplayer_config::{AppConfig, SubtitlesConfig};

use super::{AppRuntimeRoute, RouteInput, RuntimeCommittedRoute, RuntimeCommittedUpdate};

/// Route id, который metadata выводит из `apply = "subtitles.apply"`.
pub const SUBTITLES_ROUTE_ID: &str = "subtitles";

/// Полный typed snapshot секции субтитров для атомарного применения владельцем.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitlesRuntimeSettingsUpdate {
    /// Новая committed конфигурация субтитров.
    pub subtitles: SubtitlesConfig,
}

/// Собирает атомарный payload route субтитров.
pub(super) fn committed_route(current: &AppConfig, input: RouteInput) -> RuntimeCommittedRoute {
    let groups = input.groups();
    RuntimeCommittedRoute {
        route: AppRuntimeRoute::Subtitles,
        source_routes: input.source_routes,
        affected_settings: input.affected_settings,
        groups,
        update: RuntimeCommittedUpdate::Subtitles(SubtitlesRuntimeSettingsUpdate {
            subtitles: current.subtitles.clone(),
        }),
    }
}
