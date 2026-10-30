//! Шкала живого эфира: что из эфира уже лежит на полке.
//!
//! У живого потока (MSE `duration = +Infinity`, [`crate::PushedDuration::Live`])
//! нет конца и нет длительности. Плееру нужна динамическая шкала
//! (`media_core::DynamicMediaTimelinePort`): где «край эфира», куда можно
//! перематываться (DVR) и с какой позиции начинать. Источник байтов не качает
//! сам, поэтому всё это он знает только по своей полке:
//! - **край эфира** — конец непрерывного куска полки вокруг позиции (по потоку —
//!   самый короткий из полос с метками времени: дальше плеер играть не сможет);
//! - **диапазон перемотки** — сам этот кусок: то, что можно сыграть без страницы
//!   (позади позиции он ограничен окном «назад» полки);
//! - **позиция входа** — начало первого поданного сегмента: с него плеер реально
//!   получает пакеты, и часы плеера стартуют ровно оттуда.
//!
//! Публикация идёт при каждом изменении полки под замком буфера; одинаковое
//! состояние публикатор не рассылает повторно (`Unchanged`), а будильник
//! потребителя схлопывается — частые публикации дёшевы.

use std::num::NonZeroU64;

use media_core::{
    DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial, DynamicMediaTimelinePort,
    DynamicMediaTimelinePortGeneration, DynamicMediaTimelinePublishError,
    DynamicMediaTimelinePublisher, DynamicMediaTimelineState, TimelineRange,
    dynamic_media_timeline,
};

use crate::framing::{MediaTime, MediaTimeSpan};

/// Поколение порта: у потока ровно один порт на всю жизнь (новое поколение
/// `MediaSource` — новый поток и новый порт), поэтому номер постоянный.
const PORT_GENERATION: DynamicMediaTimelinePortGeneration =
    DynamicMediaTimelinePortGeneration::new(NonZeroU64::MIN);
/// Эпоха источника: полка не переписывает историю эфира (новый `MediaSource` —
/// новый поток), поэтому эпоха одна.
const SOURCE_EPOCH: DynamicMediaTimelineEpoch = DynamicMediaTimelineEpoch::new(0);

/// Что известно об эфире по полке в момент публикации.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveShelfView {
    /// Непрерывный кусок полки вокруг позиции (по всем полосам с метками времени).
    pub(crate) covered: MediaTimeSpan,
    /// Начало первого поданного сегмента.
    pub(crate) join: Option<MediaTime>,
}

/// Публикатор шкалы эфира (живёт в буфере потока).
#[derive(Debug)]
pub(crate) struct LiveTimelineFeed {
    /// Сторона источника пары «порт — публикатор».
    publisher: DynamicMediaTimelinePublisher,
}

impl LiveTimelineFeed {
    /// Создаёт пару: порт — плееру (`PreparedMedia::with_dynamic_timeline`),
    /// публикатор — буферу.
    pub(crate) fn open(view: Option<LiveShelfView>) -> (Self, DynamicMediaTimelinePort) {
        let (port, publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
            port_generation: PORT_GENERATION,
            source_epoch: SOURCE_EPOCH,
            state: state_for(view),
        });
        (Self { publisher }, port)
    }

    /// Публикует новое состояние эфира.
    ///
    /// `Err` — публиковать больше некуда (плеер уничтожил порт) или нельзя; буфер
    /// тогда перестаёт публиковать.
    pub(crate) fn publish(
        &self,
        view: Option<LiveShelfView>,
    ) -> Result<(), DynamicMediaTimelinePublishError> {
        // Пока позиция не покрыта полкой (перемотка ждёт байтов страницы), старое
        // состояние честнее пустого: край эфира и диапазон не исчезли.
        let Some(view) = view else {
            return Ok(());
        };
        self.publisher
            .publish(SOURCE_EPOCH, state_for(Some(view)))
            .map(|_| ())
    }
}

/// Состояние шкалы по полке (правила — в описании модуля).
fn state_for(view: Option<LiveShelfView>) -> DynamicMediaTimelineState {
    let Some(view) = view else {
        // Полка пуста: эфир «в нуле» и без перемотки — первая публикация по
        // полке это исправит.
        return DynamicMediaTimelineState::without_dvr(media_core::MediaTime::ZERO);
    };
    let start = view.covered.start().to_player_time();
    let live_edge = view.covered.end().to_player_time();
    let join = view
        .join
        .map_or(start, MediaTime::to_player_time)
        .min(live_edge);
    let base = TimelineRange::new(start, live_edge)
        .and_then(|range| DynamicMediaTimelineState::with_dvr(live_edge, range).ok())
        .unwrap_or_else(|| DynamicMediaTimelineState::without_dvr(live_edge));
    // Вход не позже края эфира — проверено выше; иначе остаётся вход на краю.
    base.with_join_position(join).unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{LiveShelfView, LiveTimelineFeed};
    use crate::framing::{MediaTime, MediaTimeSpan};

    fn at_ms(milliseconds: i64) -> MediaTime {
        MediaTime::from_micros(milliseconds * 1_000)
    }

    fn view(start_ms: i64, end_ms: i64, join_ms: i64) -> LiveShelfView {
        LiveShelfView {
            covered: MediaTimeSpan::new(at_ms(start_ms), at_ms(end_ms)).expect("span"),
            join: Some(at_ms(join_ms)),
        }
    }

    fn player_ms(milliseconds: u64) -> media_core::MediaTime {
        media_core::MediaTime::from_duration(Duration::from_millis(milliseconds))
    }

    #[test]
    fn covered_shelf_is_the_dvr_range_and_its_end_is_the_live_edge() {
        let (feed, port) = LiveTimelineFeed::open(Some(view(10_000, 12_000, 10_000)));
        let first = port.observe().snapshot;
        assert_eq!(first.state.live_edge(), player_ms(12_000));
        assert_eq!(first.state.join_position(), player_ms(10_000));
        let range = first.state.seekable_range().expect("dvr");
        assert_eq!(
            (range.start, range.end),
            (player_ms(10_000), player_ms(12_000))
        );

        // Новые сегменты: край эфира идёт вперёд, ревизия растёт.
        feed.publish(Some(view(10_000, 14_000, 10_000)))
            .expect("published");
        let second = port.observe().snapshot;
        assert_eq!(second.state.live_edge(), player_ms(14_000));
        assert!(second.revision > first.revision);
    }

    #[test]
    fn uncovered_position_keeps_the_last_published_state() {
        let (feed, port) = LiveTimelineFeed::open(Some(view(0, 2_000, 0)));
        let before = port.observe().snapshot;
        feed.publish(None).expect("nothing to publish");
        assert_eq!(port.observe().snapshot, before);
    }

    #[test]
    fn negative_shelf_times_are_clamped_to_zero() {
        let (_feed, port) = LiveTimelineFeed::open(Some(view(-500, 1_000, -500)));
        let state = port.observe().snapshot.state;
        assert_eq!(state.join_position(), player_ms(0));
        assert_eq!(state.seekable_range().expect("dvr").start, player_ms(0));
    }

    #[test]
    fn publishing_after_the_player_dropped_the_port_is_an_error_not_a_hang() {
        let (feed, port) = LiveTimelineFeed::open(Some(view(0, 2_000, 0)));
        drop(port);
        assert!(feed.publish(Some(view(0, 4_000, 0))).is_err());
    }
}
