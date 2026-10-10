//! Многокадровое применение одного committed route (C2).
//!
//! Только два маршрута могут ждать результата между кадрами: `MediaService` (пересборка источника
//! после смены сети/кодеков) и `Player` (шаг `media_pipeline`). Остальные завершаются в `begin`.
//! Ожидающий маршрут хранит всё, что нужно для итога (snapshot, механизм, отчёты уже сделанных
//! шагов), и завершается через [`SettingsRuntimeRouteAppliers::finish_pending_route`] ровно так же,
//! как синхронный путь: snapshot владельца обновляется только после успеха.

use super::*;

/// Итог начала применения одного route.
pub(in crate::settings_runtime) enum RouteApplyStart {
    /// Route завершён сразу (подавляющее большинство).
    Finished(AppRouteApplyReport),
    /// Route ждёт пересборки media; её результат передаётся в `finish_pending_route`.
    Pending(PendingRoute),
}

/// Маршрут, который ждёт результата пересборки media.
pub(in crate::settings_runtime) enum PendingRoute {
    MediaService(Box<PendingMediaServiceRoute>),
    Player(Box<PendingPlayerRoute>),
}

/// Media-service route: snapshot применится только после успеха пересборки.
pub(in crate::settings_runtime) struct PendingMediaServiceRoute {
    route: RuntimeCommittedRoute,
    mechanism: ApplyMechanism,
    next_snapshot: MediaServiceRuntimeSnapshot,
}

/// Player route: audio-шаг уже сделан, worker/event-policy шаги ждут пересборку.
pub(in crate::settings_runtime) struct PendingPlayerRoute {
    progress: PlayerRouteProgress,
    update: PlayerCommittedSettingsUpdate,
    continuation: PlayerMediaContinuation,
}

impl SettingsRuntimeRouteAppliers {
    /// Начинает применение route; пересборку media запускает без блокировки вызывающего.
    pub(in crate::settings_runtime) fn begin_committed_route_stepwise<A>(
        &mut self,
        route: RuntimeCommittedRoute,
        target_policy: SettingsRouteTargetPolicy,
        runtime_adapter: &mut A,
    ) -> SettingsResult<RouteApplyStart>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        match route.update.clone() {
            RuntimeCommittedUpdate::MediaService(update) => {
                self.begin_media_service_route(route, &update, target_policy, runtime_adapter)
            }
            RuntimeCommittedUpdate::Player(update) => {
                Ok(self.begin_player_route(route, *update, target_policy, runtime_adapter))
            }
            _ => self
                .apply_committed_route_with_render_adapter(route, target_policy, runtime_adapter)
                .map(RouteApplyStart::Finished),
        }
    }

    /// Завершает ожидающий route итогом пересборки: тот же хвост, что у синхронного пути.
    pub(in crate::settings_runtime) fn finish_pending_route(
        &mut self,
        pending: PendingRoute,
        rebuild_result: AppRouteApplyResult,
        reconfigure_host: &mut dyn SettingsRuntimeReconfigureHost,
    ) -> AppRouteApplyReport {
        match pending {
            PendingRoute::MediaService(pending) => {
                self.finish_media_service_route(*pending, rebuild_result)
            }
            PendingRoute::Player(pending) => {
                let PendingPlayerRoute {
                    progress,
                    update,
                    continuation,
                } = *pending;
                let player_core_result = player_core_result_from(
                    reconfigure_host.finish_player_runtime_settings(continuation, rebuild_result),
                );
                self.finish_player_route(progress, &update, player_core_result)
            }
        }
    }

    fn begin_media_service_route(
        &mut self,
        route: RuntimeCommittedRoute,
        update: &MediaServiceRuntimeSettingsUpdate,
        target_policy: SettingsRouteTargetPolicy,
        reconfigure_host: &mut dyn SettingsRuntimeReconfigureHost,
    ) -> SettingsResult<RouteApplyStart> {
        let mechanism = media_service_apply_mechanism(&route.affected_settings)?;
        let next_snapshot = MediaServiceRuntimeSnapshot::from_update(update);
        if self.media_service == next_snapshot {
            return Ok(RouteApplyStart::Finished(Self::route_report(
                route,
                AppRouteApplyResult::Noop,
                mechanism,
            )));
        }
        let progress = reconfigure_host.begin_media_service_runtime_settings(
            update,
            &route.affected_settings,
            target_policy,
        );
        let pending = PendingMediaServiceRoute {
            route,
            mechanism,
            next_snapshot,
        };
        Ok(match progress {
            MediaRebuildProgress::Finished(result) => {
                RouteApplyStart::Finished(self.finish_media_service_route(pending, result))
            }
            MediaRebuildProgress::Pending => {
                RouteApplyStart::Pending(PendingRoute::MediaService(Box::new(pending)))
            }
        })
    }

    /// Snapshot media-service policy обновляется только после успешного итога.
    fn finish_media_service_route(
        &mut self,
        pending: PendingMediaServiceRoute,
        result: AppRouteApplyResult,
    ) -> AppRouteApplyReport {
        let PendingMediaServiceRoute {
            route,
            mechanism,
            next_snapshot,
        } = pending;
        if result.is_success() {
            self.media_service = next_snapshot;
        }
        Self::route_report(route, result, mechanism)
    }

    fn begin_player_route(
        &mut self,
        route: RuntimeCommittedRoute,
        update: PlayerCommittedSettingsUpdate,
        target_policy: SettingsRouteTargetPolicy,
        reconfigure_host: &mut dyn SettingsRuntimeReconfigureHost,
    ) -> RouteApplyStart {
        let progress = self.start_player_route(route, &update);
        let core_start = if !progress.audio_output_device_result.is_success() {
            Err(audio_device_failed_core_not_started())
        } else if update.player_core.is_empty()
            && update.event_policy_settings.is_empty()
            && update.media_pipeline.is_none()
        {
            Err(AppRouteApplyResult::Noop)
        } else {
            Ok(reconfigure_host.begin_player_runtime_settings(&update, target_policy))
        };
        match core_start {
            Err(player_core_result) => RouteApplyStart::Finished(self.finish_player_route(
                progress,
                &update,
                player_core_result,
            )),
            Ok(PlayerApplyStart::Finished(result)) => {
                let player_core_result = player_core_result_from(result);
                RouteApplyStart::Finished(self.finish_player_route(
                    progress,
                    &update,
                    player_core_result,
                ))
            }
            Ok(PlayerApplyStart::AwaitingMediaRebuild(continuation)) => {
                RouteApplyStart::Pending(PendingRoute::Player(Box::new(PendingPlayerRoute {
                    progress,
                    update,
                    continuation: *continuation,
                })))
            }
        }
    }
}
