//! Табло шкалы поверх полок буфера (правила — в `crate::live_timeline` и `crate::timeline_view`).

use media_core::{DynamicMediaTimelinePort, DynamicMediaTimelinePublishError};

use super::{BufferState, StreamBuffer};
use crate::PushedDuration;
use crate::framing::MediaTime;
use crate::framing::MediaTimeSpan;
use crate::live_timeline::{LiveShelfView, LiveTimelineFeed};

impl StreamBuffer {
    /// Открывает табло шкалы: порт — плееру; дальше буфер публикует в него при
    /// каждом изменении полки и длительности. `declared` — длительность, названная
    /// страницей к открытию. Повторное открытие заменяет публикатор (старый порт
    /// замирает на последнем состоянии).
    pub(crate) fn open_live_timeline(&self, declared: PushedDuration) -> DynamicMediaTimelinePort {
        let mut state = self.lock();
        let (feed, port) =
            LiveTimelineFeed::open(self.live_view(&state), declared, state.furthest_timed_end);
        state.live_timeline = Some(feed);
        port
    }

    /// Страница назвала новую длительность (`duration`): табло публикуется сразу.
    pub(crate) fn note_declared_duration(&self, declared: PushedDuration) {
        let mut state = self.lock();
        if let Some(feed) = state.live_timeline.as_mut() {
            feed.note_declared_duration(declared);
        }
        self.publish_live_timeline(&mut state);
    }

    /// Публикует состояние табло, если у потока оно открыто.
    pub(super) fn publish_live_timeline(&self, state: &mut BufferState) {
        // Вид полки обходит полки, а публикация зовётся на каждый принятый сегмент: его
        // считает только эфир. Записи хватает дальнего конца (O(1)) — на тысячах мелких
        // фрагментов обход полки на каждом сегменте задушил бы приём (16d1).
        let follows_the_shelf = match state.live_timeline.as_ref() {
            None => return,
            Some(feed) => feed.follows_the_shelf(),
        };
        let shelf = follows_the_shelf.then(|| self.live_view(state));
        let downloaded_end = state.furthest_timed_end;
        let Some(feed) = state.live_timeline.as_mut() else {
            return;
        };
        match feed.publish(shelf, downloaded_end) {
            Ok(()) => {}
            Err(DynamicMediaTimelinePublishError::ConsumerDisconnected) => {
                // Плеер уничтожил порт (поток закрыт) — публиковать больше некуда.
                tracing::debug!("Pushed timeline stopped publishing: the player dropped the port");
                state.live_timeline = None;
            }
            Err(error) => {
                // Остальные отказы — дефект источника (эпоха, переполнение ревизий, запрещённый
                // вид мимо `next_view`): публикатор остаётся, плеер видит прежнее табло.
                tracing::warn!(%error, "Pushed timeline publication refused");
            }
        }
    }

    /// Страница сама перематывается к `target`, пока поток у плеера: окно эфира держит
    /// цель до перемотки плеера следом (`crate::live_timeline`) и публикуется сразу —
    /// раньше, чем приложение пошлёт плееру команду.
    pub(super) fn note_live_page_seek(&self, state: &mut BufferState, target: MediaTime) {
        if let Some(feed) = state.live_timeline.as_mut() {
            feed.note_page_seek(target);
        }
        self.publish_live_timeline(state);
    }

    /// Плеер перемотался: цель страницы окну эфира больше не нужна (его цель держит
    /// позиция). Публикует перемотка сама, после перестановки полос.
    pub(super) fn note_live_player_seek(state: &mut BufferState) {
        if let Some(feed) = state.live_timeline.as_mut() {
            feed.note_player_seek();
        }
    }

    /// Что эфир знает о полке и где плеер (правила — в `crate::live_timeline`).
    fn live_view(&self, state: &BufferState) -> LiveShelfView {
        LiveShelfView {
            covered: self.covered_around_position(state),
            player_position: state.position(),
            join: state.first_fed_start,
        }
    }

    /// Непрерывный кусок полки вокруг позиции, общий для всех полос с метками
    /// времени (пересечение кусков полос); `None` — позиции нет или она не покрыта.
    fn covered_around_position(&self, state: &BufferState) -> Option<MediaTimeSpan> {
        let position = state.position()?;
        let rules = self.rules;
        let mut covered: Option<MediaTimeSpan> = None;
        for lane in state.lanes.values() {
            if lane.timeline != super::LaneTimeline::Timed {
                continue;
            }
            let run = lane.shelf.covered_run(position, &rules)?;
            covered = Some(match covered {
                None => run,
                Some(common) => MediaTimeSpan::new(
                    common.start().max(run.start()),
                    common.end().min(run.end()),
                )?,
            });
        }
        covered
    }
}
