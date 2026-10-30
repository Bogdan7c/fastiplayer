//! Шкала живого эфира поверх полок буфера (правила — в `crate::live_timeline`).

use media_core::DynamicMediaTimelinePort;

use super::{BufferState, StreamBuffer};
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
        let Some(feed) = &state.live_timeline else {
            return;
        };
        if let Err(error) = feed.publish(self.live_view(state)) {
            // Плеер уничтожил порт (поток закрыт) — публиковать больше некуда.
            tracing::debug!(%error, "Pushed live timeline stopped publishing");
            state.live_timeline = None;
        }
    }

    /// Непрерывный кусок полки вокруг позиции, общий для всех полос с метками
    /// времени (пересечение кусков полос), и позиция входа.
    fn live_view(&self, state: &BufferState) -> Option<LiveShelfView> {
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
        covered.map(|covered| LiveShelfView {
            covered,
            join: state.first_fed_start,
        })
    }
}
