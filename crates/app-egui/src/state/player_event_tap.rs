//! Копия событий плеера для второго наблюдателя (окно браузерного режима, мост 04a).
//!
//! События `PlayerWorker` забираются один раз (`drain_events`), и единственный их
//! читатель — `AppState` (кадр, `frame_prepare::record_worker_events`). Окну браузерного
//! режима нужны те же события, чтобы приписать отказ своей загрузке и вести журнал
//! (`Browser playback summary`). Второго читателя у плеера быть не может, поэтому
//! `AppState` сам кладёт копии событий плеера в «отвод», если его включили при сборке.
//!
//! Отвод ограничен: если его долго не забирают (окно свёрнуто и наблюдатель не идёт),
//! старые копии вытесняются и считаются — ни памяти без предела, ни тихой потери.

use std::collections::VecDeque;

use player_core::{CorrelatedPlayerEvent, PlayerWorkerEvent};

/// Сколько копий событий держать до того, как их заберут.
///
/// События плеера — единицы в секунду (состояние, ошибки, запросы декодера); 1024 —
/// минуты без наблюдателя.
pub(crate) const PLAYER_EVENT_TAP_CAPACITY: usize = 1024;

/// Что наблюдатель забрал из отвода.
#[derive(Debug, Default)]
pub(crate) struct TappedPlayerEvents {
    /// Копии событий плеера в порядке плеера.
    pub(crate) events: Vec<CorrelatedPlayerEvent>,
    /// Сколько самых старых копий вытеснено с прошлого забора (отвод был полон).
    pub(crate) evicted: u64,
}

/// Ограниченный отвод копий событий плеера.
#[derive(Debug)]
pub(crate) struct PlayerEventTap {
    copies: VecDeque<CorrelatedPlayerEvent>,
    capacity: usize,
    evicted: u64,
}

impl PlayerEventTap {
    /// Отвод на `capacity` копий (не меньше одной).
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            copies: VecDeque::with_capacity(capacity.min(64)),
            capacity,
            evicted: 0,
        }
    }

    /// Кладёт копии событий плеера из только что забранной пачки. Прочие события
    /// worker-а (скраб, ошибки рендера) окну браузерного режима не нужны.
    pub(super) fn copy_player_events(&mut self, events: &[PlayerWorkerEvent]) {
        for event in events {
            if let PlayerWorkerEvent::Player(correlated) = event {
                if self.copies.len() == self.capacity {
                    self.copies.pop_front();
                    self.evicted = self.evicted.saturating_add(1);
                }
                self.copies.push_back(correlated.clone());
            }
        }
    }

    /// Забирает накопленное и обнуляет счёт вытесненных.
    pub(super) fn take(&mut self) -> TappedPlayerEvents {
        TappedPlayerEvents {
            events: self.copies.drain(..).collect(),
            evicted: std::mem::take(&mut self.evicted),
        }
    }
}

#[cfg(test)]
mod tests {
    use player_core::{PlaybackState, PlayerEvent, PlayerRenderError, PlayerRenderErrorKind};

    use super::*;

    fn state_event(state: PlaybackState) -> PlayerWorkerEvent {
        PlayerWorkerEvent::Player(CorrelatedPlayerEvent {
            media_instance_id: None,
            event: PlayerEvent::PlaybackStateChanged(state),
        })
    }

    #[test]
    fn only_player_events_are_copied_in_order() {
        let mut tap = PlayerEventTap::with_capacity(8);
        tap.copy_player_events(&[
            state_event(PlaybackState::Opening),
            PlayerWorkerEvent::RenderError(PlayerRenderError {
                kind: PlayerRenderErrorKind::UnsupportedFrameFormat,
                render_generation: None,
                frame_handle: None,
                message: String::new(),
            }),
            state_event(PlaybackState::Playing),
        ]);
        let taken = tap.take();
        let states: Vec<_> = taken
            .events
            .iter()
            .map(|correlated| correlated.event.clone())
            .collect();
        assert_eq!(
            states,
            vec![
                PlayerEvent::PlaybackStateChanged(PlaybackState::Opening),
                PlayerEvent::PlaybackStateChanged(PlaybackState::Playing),
            ]
        );
        assert_eq!(taken.evicted, 0);
        assert!(tap.take().events.is_empty(), "taking empties the tap");
    }

    #[test]
    fn a_full_tap_evicts_the_oldest_copies_and_counts_them() {
        let mut tap = PlayerEventTap::with_capacity(2);
        tap.copy_player_events(&[
            state_event(PlaybackState::Opening),
            state_event(PlaybackState::Buffering),
            state_event(PlaybackState::Playing),
        ]);
        let taken = tap.take();
        assert_eq!(taken.evicted, 1);
        assert_eq!(
            taken
                .events
                .last()
                .map(|correlated| correlated.event.clone()),
            Some(PlayerEvent::PlaybackStateChanged(PlaybackState::Playing))
        );
        assert_eq!(taken.events.len(), 2);
        assert_eq!(tap.take().evicted, 0, "the eviction count is reported once");
    }
}
