//! План действия таймлайна: какие шаги получит плеер (или зритель-перемотка).
//!
//! Жест таймлайна (`ui::timeline`) даёт намерения — «начать живой скраб в T»,
//! «очередная цель», «отпустил в T», «клик в T». Здесь они становятся шагами для
//! плеера, а исполняет шаги `AppState` (`state::timeline_dispatch`). План — чистая
//! функция без egui и без плеера, поэтому правила проверяются тестами напрямую.
//!
//! Два охвата живого скраба ([`LiveScrubScope`]):
//! - **везде** (обычное окно): намерение = шаг, как было всегда;
//! - **только по скачанному** (окно браузерного режима, «кадры бегут по полке»):
//!   - цель на скачанном — шаг плееру как в обычном окне, кадры бегут;
//!   - цель вне скачанного — плееру НИЧЕГО (кадр стоит, бегунок едет): перемотка
//!     плеера туда стала бы просьбой странице;
//!   - «начать» откладывается до первой цели на скачанном: живой скраб у плеера
//!     начинается только тогда, когда ему есть что показать, — жест целиком вне
//!     скачанного не трогает плеер вовсе и кончается одной перемоткой;
//!   - отпускание на скачанном — завершить скраб у плеера (странице ничего, позицию
//!     она увидит по часам);
//!   - отпускание вне скачанного — завершить скраб у плеера (если он начат) и ОДНА
//!     обычная точная перемотка — она уходит зрительской перемоткой через сеанс, и
//!     источник сам пошлёт странице не больше одной просьбы.
//!
//! Почему отпускание вне скачанного — «завершить по последней цели» и только потом
//! перемотка, а не финальная цель в сам скраб: плеер садит скраб на последнюю цель,
//! уже показанную на скачанном, повторно её не перематывая (тот же проход живого
//! скраба), а следующая перемотка заменяет эту посадку и наследует «играл/пауза» до
//! жеста (player-core: `in_flight_seek_resume_intent`). Финальная цель вне скачанного
//! внутри скраба тоже дала бы одну просьбу странице, но в обход пути сеанса (тот
//! знает про открывающееся поколение и про волю зрителя) — поэтому перемотка.

use media_core::MediaTime;
use player_core::ScrubCommitPolicy;

use super::timeline_dispatch::{TimelineCommandRoute, live_scrub_release_policy_from_action};
use crate::downloaded_range::DownloadedRange;
use crate::ui::timeline::TimelineAction;
use crate::window_view_profile::LiveScrubScope;

/// Начат ли в этом жесте живой скраб у плеера (ушло ли ему «начать»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlayerScrubStanding {
    /// Плеер о жесте ещё не знает.
    NotStarted,
    /// Плеер в живом скрабе этого жеста.
    Started,
}

/// Что сделать с финальной целью при завершении живого скраба.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScrubRelease {
    /// Сначала довести скраб точно до точки отпускания (если она ещё не отправлена).
    LandExactlyAt(MediaTime),
    /// Завершить на том, что плеер уже получил (финальная цель плееру не уходит).
    KeepLatestDispatched,
}

/// Шаг для плеера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimelineStep {
    /// Обычная точная перемотка (в окне браузерного режима — перемотка зрителя).
    ExactSeek {
        /// Куда.
        target: MediaTime,
        /// Маршрут для журнала.
        route: TimelineCommandRoute,
    },
    /// Начать живой скраб у плеера и сразу показать кадр цели.
    BeginLiveScrub(MediaTime),
    /// Очередная цель живого скраба (через троттлинг диспетчера жеста).
    PreviewLiveScrub(MediaTime),
    /// Завершить живой скраб у плеера.
    EndLiveScrub {
        /// Как плеер выбирает точку посадки.
        policy: ScrubCommitPolicy,
        /// Что сделать с финальной целью.
        release: ScrubRelease,
    },
}

/// Что известно в момент действия: охват, состояние скраба у плеера, скачанное.
#[derive(Debug, Clone, Copy)]
pub(super) struct TimelinePlanContext<'range> {
    /// Охват живого скраба у вида окна.
    pub(super) scope: LiveScrubScope,
    /// Начат ли скраб у плеера в этом жесте.
    pub(super) standing: PlayerScrubStanding,
    /// Что скачано у показанного медиа.
    pub(super) downloaded: &'range DownloadedRange,
}

impl TimelinePlanContext<'_> {
    /// Лежит ли цель там, куда живой скраб может вести плеер.
    fn previews_locally(&self, target: MediaTime) -> bool {
        match self.scope {
            LiveScrubScope::Anywhere => true,
            LiveScrubScope::DownloadedRangeOnly => self.downloaded.covers(target.as_duration()),
        }
    }
}

/// Шаги для одного действия таймлайна (правила — в описании модуля).
pub(super) fn plan_timeline_action(
    action: TimelineAction,
    context: &TimelinePlanContext<'_>,
) -> Vec<TimelineStep> {
    match action {
        TimelineAction::ClickSeek(target) => vec![TimelineStep::ExactSeek {
            target,
            route: TimelineCommandRoute::ClickSeek,
        }],
        TimelineAction::CommitDragSeek(target) => vec![TimelineStep::ExactSeek {
            target,
            route: TimelineCommandRoute::DragSeek,
        }],
        TimelineAction::BeginLiveScrub(target) => {
            if context.previews_locally(target) {
                vec![TimelineStep::BeginLiveScrub(target)]
            } else {
                // «Начать» ждёт первой цели на скачанном.
                Vec::new()
            }
        }
        TimelineAction::PreviewLiveScrub(target) => plan_preview(target, context),
        TimelineAction::EndLiveScrubAtLatestTarget(target)
        | TimelineAction::EndLiveScrubAtVisiblePreview(target) => {
            // Политику несёт само действие (клик — последняя цель, drag — видимый кадр).
            let Some(policy) = live_scrub_release_policy_from_action(action) else {
                return Vec::new();
            };
            plan_release(target, policy, context)
        }
        TimelineAction::CancelLiveScrub => match context.standing {
            // Потеря фокуса: завершить на том, что плеер уже получил. Вне скачанного
            // плеер ничего не получал — значит и сеанс, и страница остаются в покое.
            PlayerScrubStanding::Started => vec![TimelineStep::EndLiveScrub {
                policy: ScrubCommitPolicy::CommitLatestTarget,
                release: ScrubRelease::KeepLatestDispatched,
            }],
            PlayerScrubStanding::NotStarted => match context.scope {
                // Обычное окно — как всегда: завершение уходит плееру, даже если
                // диспетчер жеста уже сброшен.
                LiveScrubScope::Anywhere => vec![TimelineStep::EndLiveScrub {
                    policy: ScrubCommitPolicy::CommitLatestTarget,
                    release: ScrubRelease::KeepLatestDispatched,
                }],
                LiveScrubScope::DownloadedRangeOnly => Vec::new(),
            },
        },
    }
}

/// Очередная цель жеста.
fn plan_preview(target: MediaTime, context: &TimelinePlanContext<'_>) -> Vec<TimelineStep> {
    if !context.previews_locally(target) {
        // Кадр стоит, бегунок едет; диспетчер жеста эту цель не видит и её посадки не ждёт.
        return Vec::new();
    }
    match (context.scope, context.standing) {
        // Первая цель на скачанном в браузерном окне — отложенное «начать».
        (LiveScrubScope::DownloadedRangeOnly, PlayerScrubStanding::NotStarted) => {
            vec![TimelineStep::BeginLiveScrub(target)]
        }
        _ => vec![TimelineStep::PreviewLiveScrub(target)],
    }
}

/// Отпускание в `target`.
fn plan_release(
    target: MediaTime,
    policy: ScrubCommitPolicy,
    context: &TimelinePlanContext<'_>,
) -> Vec<TimelineStep> {
    let land_here = TimelineStep::EndLiveScrub {
        policy,
        release: ScrubRelease::LandExactlyAt(target),
    };
    if context.previews_locally(target) {
        return match (context.scope, context.standing) {
            // Весь жест был вне скачанного, а отпустили на нём: скраб начинается и
            // сразу садится в точку отпускания — странице по-прежнему ничего.
            (LiveScrubScope::DownloadedRangeOnly, PlayerScrubStanding::NotStarted) => {
                vec![TimelineStep::BeginLiveScrub(target), land_here]
            }
            _ => vec![land_here],
        };
    }
    let viewer_seek = TimelineStep::ExactSeek {
        target,
        route: TimelineCommandRoute::ReleaseOutsideDownloaded,
    };
    match context.standing {
        PlayerScrubStanding::Started => vec![
            TimelineStep::EndLiveScrub {
                policy: ScrubCommitPolicy::CommitLatestTarget,
                release: ScrubRelease::KeepLatestDispatched,
            },
            viewer_seek,
        ],
        PlayerScrubStanding::NotStarted => vec![viewer_seek],
    }
}

#[cfg(test)]
#[path = "timeline_scrub_plan_tests.rs"]
mod tests;
