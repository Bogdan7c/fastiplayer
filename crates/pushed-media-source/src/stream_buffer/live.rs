//! Шкала живого эфира поверх полок буфера (правила — в `crate::live_timeline`).

use media_core::DynamicMediaTimelinePort;

use super::{BufferState, StreamBuffer};
use crate::framing::MediaTime;
use crate::framing::MediaTimeSpan;
use crate::live_timeline::{LiveShelfView, LiveTimelineFeed};

impl StreamBuffer {
    /// Открывает шкалу эфира: порт — плееру; дальше буфер публикует в неё при
    /// каждом изменении полки. Повторное открытие заменяет публикатор (старый
    /// порт замирает на последнем состоянии).
    pub(crate) fn open_live_timeline(&self) -> DynamicMediaTimelinePort {
        let mut state = self.lock();
        let (feed, port) = LiveTimelineFeed::open(self.live_view(&state));
        state.live_timeline = Some(feed);
        port
    }

    /// Публикует состояние эфира, если поток открыт как эфир.
    pub(super) fn publish_live_timeline(&self, state: &mut BufferState) {
        let view = self.live_view(state);
        let Some(feed) = state.live_timeline.as_mut() else {
            return;
        };
        if let Err(error) = feed.publish(view) {
            // Плеер уничтожил порт (поток закрыт) — публиковать больше некуда.
            tracing::debug!(%error, "Pushed live timeline stopped publishing");
            state.live_timeline = None;
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
