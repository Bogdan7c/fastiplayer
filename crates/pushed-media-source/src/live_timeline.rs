//! Табло шкалы потока: что из эфира уже лежит на полке и где эфир страницы точно есть.
//!
//! **Табло есть у каждого потока** (мост 16d1, решения 318–320): вид табло — эфир, запись или
//! «пока неизвестно» — следует за длительностью, которую назвала страница, и меняется на ходу
//! (правила вида — `crate::timeline_view`). Ниже — как устроен вид «эфир».
//!
//! У живого потока (MSE `duration = +Infinity`, [`crate::PushedDuration::Live`])
//! нет конца и нет длительности. Плееру нужна динамическая шкала
//! (`media_core::DynamicMediaTimelinePort`): где «край эфира», куда можно
//! перематываться (DVR) и с какой позиции начинать. Источник байтов не качает
//! сам, поэтому всё это он знает только по своей полке и по словам страницы:
//! - **диапазон перемотки** — непрерывный кусок полки вокруг позиции (по потоку —
//!   пересечение полос с метками времени: то, что можно сыграть без страницы),
//!   расширенный до мест, где эфир страницы есть наверняка (якоря, ниже);
//! - **край эфира** — конец этого диапазона: дальше плеер играть не сможет;
//! - **позиция входа** — начало первого поданного сегмента: с него плеер реально
//!   получает пакеты, и часы плеера стартуют ровно оттуда.
//!
//! **Якоря: хозяин шкалы — страница** (сессия 09b2, решение 194). Страница может стереть
//! всю полку и перезапустить эфир в том же `MediaSource` с новой шкалой (Twitch на паузе:
//! `remove()` всего и `currentTime = 0`, на play — байты с ~0,09 с). Перемотка самой
//! страницы — её слово «моя шкала здесь есть»: окно, посчитанное только по полке, отвергло
//! бы эту перемотку у плеера как «вне DVR» (`SeekTargetExpired`), хотя по правилу моста
//! перемотка ждёт страницу, пока та жива (решение 150). Поэтому окно всегда держит:
//! - **цель перемотки страницы**, ещё не встреченную перемоткой плеера — плеер пойдёт туда
//!   следом, и его перемотка не должна быть отвергнута. Только перемотки страницы, пока
//!   поток уже у плеера (шкала открыта): раньше открытия место выбирает установка
//!   (эфир встаёт на вход), и плеер за такой целью не пойдёт — якорь навсегда растянул бы
//!   окно назад;
//! - **позицию плеера** (во время перемотки — её цель): пока байты цели в пути, окно,
//!   пересчитанное по полке, не должно «просрочить» уже идущую перемотку.
//!
//! Если позиция полкой не покрыта (перемотка ждёт байтов страницы, страница стёрла полку),
//! основой окна остаётся последнее опубликованное: старое состояние честнее пустого — край
//! эфира и диапазон не исчезли, пока страница не прислала новые байты.
//!
//! Публикация идёт при каждом изменении полки и при перемотке страницы под замком буфера;
//! одинаковое состояние публикатор не рассылает повторно (`Unchanged`), а будильник
//! потребителя схлопывается — частые публикации дёшевы.

use std::num::NonZeroU64;

use media_core::{
    DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial, DynamicMediaTimelinePort,
    DynamicMediaTimelinePortGeneration, DynamicMediaTimelinePublishError,
    DynamicMediaTimelinePublisher, DynamicMediaTimelineState, DynamicMediaTimelineView,
    DynamicMediaTimelineViewKind, TimelineRange, dynamic_media_timeline,
};

use crate::PushedDuration;
use crate::framing::{MediaTime, MediaTimeSpan};
use crate::timeline_view::{next_view, view_for};

/// Поколение порта: у потока ровно один порт на всю жизнь (новое поколение
/// `MediaSource` — новый поток и новый порт), поэтому номер постоянный.
const PORT_GENERATION: DynamicMediaTimelinePortGeneration =
    DynamicMediaTimelinePortGeneration::new(NonZeroU64::MIN);
/// Эпоха источника: одна на поток. Перезапуск эфира страницей в том же `MediaSource`
/// эпоху не меняет — плеер принимает его как обычную перемотку страницы (якоря выше).
const SOURCE_EPOCH: DynamicMediaTimelineEpoch = DynamicMediaTimelineEpoch::new(0);

/// Что известно об эфире в момент публикации.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveShelfView {
    /// Непрерывный кусок полки вокруг позиции (по всем полосам с метками времени);
    /// `None` — позиция полкой не покрыта или позиции ещё нет.
    pub(crate) covered: Option<MediaTimeSpan>,
    /// Позиция плеера (во время перемотки — её цель): якорь окна.
    pub(crate) player_position: Option<MediaTime>,
    /// Начало первого поданного сегмента.
    pub(crate) join: Option<MediaTime>,
}

/// Публикатор табло шкалы (живёт в буфере потока).
#[derive(Debug)]
pub(crate) struct LiveTimelineFeed {
    /// Сторона источника пары «порт — публикатор».
    publisher: DynamicMediaTimelinePublisher,
    /// Длительность, которую последней назвала страница: по ней выбирается вид табло.
    declared: PushedDuration,
    /// Вид последней публикации: запрещённые контрактом переходы не публикуются.
    published_kind: DynamicMediaTimelineViewKind,
    /// Окно перемотки последней публикации: основа окна, пока позиция полкой не
    /// покрыта. `None` — по полке ещё ничего не публиковали.
    last_window: Option<MediaTimeSpan>,
    /// Цель перемотки самой страницы, за которой плеер ещё не перемотался (якорь окна).
    page_seek_target: Option<MediaTime>,
}

impl LiveTimelineFeed {
    /// Создаёт пару: порт — плееру (`PreparedMedia::with_dynamic_timeline`),
    /// публикатор — буферу. `declared` — длительность, названная страницей к открытию.
    pub(crate) fn open(
        view: LiveShelfView,
        declared: PushedDuration,
        downloaded_end: Option<MediaTime>,
    ) -> (Self, DynamicMediaTimelinePort) {
        let window = seek_window(view, None, None);
        let initial = view_for(declared, || state_for(window, view.join), downloaded_end);
        let (port, publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
            port_generation: PORT_GENERATION,
            source_epoch: SOURCE_EPOCH,
            state: initial,
        });
        let feed = Self {
            publisher,
            declared,
            published_kind: initial.kind(),
            last_window: window,
            page_seek_target: None,
        };
        (feed, port)
    }

    /// Страница назвала новую длительность: вид табло пересчитает следующая публикация
    /// (буфер публикует сразу же).
    pub(crate) fn note_declared_duration(&mut self, declared: PushedDuration) {
        self.declared = declared;
    }

    /// Нужен ли публикации вид полки (`LiveShelfView`): только эфиру — его окно строится по
    /// полке; записи и «неизвестно» хватает дальнего конца скачанного.
    pub(crate) fn follows_the_shelf(&self) -> bool {
        self.declared == PushedDuration::Live
    }

    /// Страница сама перематывается к `target`: плеер пойдёт следом, окно держит цель
    /// до его перемотки (публикует буфер следующим шагом).
    pub(crate) fn note_page_seek(&mut self, target: MediaTime) {
        self.page_seek_target = Some(target);
    }

    /// Плеер перемотался (за страницей или в своё место): дальше его цель держит якорь
    /// позиции, а цель страницы больше ничего не значит.
    pub(crate) fn note_player_seek(&mut self) {
        self.page_seek_target = None;
    }

    /// Публикует новое состояние табло.
    ///
    /// `Err` — публиковать больше некуда (плеер уничтожил порт) или нельзя; буфер
    /// тогда перестаёт публиковать.
    ///
    /// `shelf` — вид полки: его считает буфер, только если [`Self::follows_the_shelf`];
    /// `downloaded_end` — самый дальний конец скачанного.
    pub(crate) fn publish(
        &mut self,
        shelf: Option<LiveShelfView>,
        downloaded_end: Option<MediaTime>,
    ) -> Result<(), DynamicMediaTimelinePublishError> {
        let candidate = match (self.declared, shelf) {
            (PushedDuration::Live, Some(view)) => {
                // Ни полки вокруг позиции, ни прошлой публикации: строить окно не из чего —
                // остаётся прежнее состояние.
                let Some(window) = seek_window(view, self.last_window, self.page_seek_target)
                else {
                    return Ok(());
                };
                self.last_window = Some(window);
                DynamicMediaTimelineView::Live(state_for(Some(window), view.join))
            }
            // Эфир без вида полки: буфер его не посчитал — публиковать нечего.
            (PushedDuration::Live, None) => return Ok(()),
            (declared, _) => view_for(declared, || state_for(None, None), downloaded_end),
        };
        let Some(next) = next_view(self.published_kind, candidate) else {
            // Запись → эфир и возврат в «неизвестно» контракт не допускает: табло прежнее.
            tracing::debug!(
                published = ?self.published_kind,
                declared = ?self.declared,
                "Pushed timeline keeps its view: the page named a duration the timeline cannot go back to"
            );
            return Ok(());
        };
        self.publisher.publish_view(SOURCE_EPOCH, next)?;
        self.published_kind = next.kind();
        Ok(())
    }
}

/// Окно перемотки: кусок полки вокруг позиции (не покрыта — прошлое окно), расширенный
/// до якорей — позиции плеера и цели страницы. `None` — основы нет: одна точка не окно.
fn seek_window(
    view: LiveShelfView,
    last_window: Option<MediaTimeSpan>,
    page_seek_target: Option<MediaTime>,
) -> Option<MediaTimeSpan> {
    let base = view.covered.or(last_window)?;
    let widened = [view.player_position, page_seek_target]
        .into_iter()
        .flatten()
        .fold(base, |window, anchor| {
            window.union(MediaTimeSpan::empty_at(anchor))
        });
    Some(widened)
}

/// Состояние шкалы по окну перемотки (правила — в описании модуля).
fn state_for(window: Option<MediaTimeSpan>, join: Option<MediaTime>) -> DynamicMediaTimelineState {
    let Some(window) = window else {
        // Полка пуста: эфир «в нуле» и без перемотки — первая публикация по
        // полке это исправит.
        return DynamicMediaTimelineState::without_dvr(media_core::MediaTime::ZERO);
    };
    let start = window.start().to_player_time();
    let live_edge = window.end().to_player_time();
    let join = join.map_or(start, MediaTime::to_player_time).min(live_edge);
    let base = TimelineRange::new(start, live_edge)
        .and_then(|range| DynamicMediaTimelineState::with_dvr(live_edge, range).ok())
        .unwrap_or_else(|| DynamicMediaTimelineState::without_dvr(live_edge));
    // Вход не позже края эфира — проверено выше; иначе остаётся вход на краю.
    base.with_join_position(join).unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::PushedDuration;

    use super::{LiveShelfView, LiveTimelineFeed};
    use crate::framing::{MediaTime, MediaTimeSpan};

    fn at_ms(milliseconds: i64) -> MediaTime {
        MediaTime::from_micros(milliseconds * 1_000)
    }

    fn view(start_ms: i64, end_ms: i64, join_ms: i64) -> LiveShelfView {
        LiveShelfView {
            covered: MediaTimeSpan::new(at_ms(start_ms), at_ms(end_ms)),
            player_position: Some(at_ms(start_ms)),
            join: Some(at_ms(join_ms)),
        }
    }

    /// Полка вокруг позиции не покрывает её (стёрта или байты цели в пути).
    fn uncovered(player_position_ms: i64) -> LiveShelfView {
        LiveShelfView {
            covered: None,
            player_position: Some(at_ms(player_position_ms)),
            join: Some(at_ms(0)),
        }
    }

    fn seekable_ms(port: &media_core::DynamicMediaTimelinePort) -> (u64, u64) {
        let range = port
            .observe()
            .snapshot
            .state
            .as_live()
            .expect("live view")
            .seekable_range()
            .expect("dvr");
        let millis = |time: media_core::MediaTime| {
            u64::try_from(time.as_duration().as_millis()).expect("millis")
        };
        (millis(range.start), millis(range.end))
    }

    fn player_ms(milliseconds: u64) -> media_core::MediaTime {
        media_core::MediaTime::from_duration(Duration::from_millis(milliseconds))
    }

    #[test]
    fn covered_shelf_is_the_dvr_range_and_its_end_is_the_live_edge() {
        let (mut feed, port) =
            LiveTimelineFeed::open(view(10_000, 12_000, 10_000), PushedDuration::Live, None);
        let first = port.observe().snapshot;
        assert_eq!(
            first.state.as_live().expect("live view").live_edge(),
            player_ms(12_000)
        );
        assert_eq!(
            first.state.as_live().expect("live view").join_position(),
            player_ms(10_000)
        );
        let range = first
            .state
            .as_live()
            .expect("live view")
            .seekable_range()
            .expect("dvr");
        assert_eq!(
            (range.start, range.end),
            (player_ms(10_000), player_ms(12_000))
        );

        // Новые сегменты: край эфира идёт вперёд, ревизия растёт.
        feed.publish(Some(view(10_000, 14_000, 10_000)), None)
            .expect("published");
        let second = port.observe().snapshot;
        assert_eq!(
            second.state.as_live().expect("live view").live_edge(),
            player_ms(14_000)
        );
        assert!(second.revision > first.revision);
    }

    #[test]
    fn uncovered_position_keeps_the_last_published_state() {
        let (mut feed, port) =
            LiveTimelineFeed::open(view(0, 2_000, 0), PushedDuration::Live, None);
        let before = port.observe().snapshot;
        feed.publish(Some(uncovered(1_000)), None)
            .expect("nothing to publish");
        assert_eq!(port.observe().snapshot, before);
    }

    #[test]
    fn negative_shelf_times_are_clamped_to_zero() {
        let (_feed, port) =
            LiveTimelineFeed::open(view(-500, 1_000, -500), PushedDuration::Live, None);
        let state = port.observe().snapshot.state.as_live().expect("live view");
        assert_eq!(state.join_position(), player_ms(0));
        assert_eq!(state.seekable_range().expect("dvr").start, player_ms(0));
    }

    #[test]
    fn publishing_after_the_player_dropped_the_port_is_an_error_not_a_hang() {
        let (mut feed, port) =
            LiveTimelineFeed::open(view(0, 2_000, 0), PushedDuration::Live, None);
        drop(port);
        assert!(feed.publish(Some(view(0, 4_000, 0)), None).is_err());
    }

    /// Twitch на паузе: страница стёрла полку (53,9–56,8 с) и перемоталась на 0 — окно
    /// держит цель страницы, иначе плеер отверг бы её перемотку как «вне DVR».
    #[test]
    fn a_page_seek_outside_the_shelf_stays_inside_the_seek_window() {
        let (mut feed, port) =
            LiveTimelineFeed::open(view(53_900, 56_800, 27_700), PushedDuration::Live, None);
        feed.note_page_seek(at_ms(0));
        feed.publish(Some(uncovered(56_600)), None)
            .expect("published");
        assert_eq!(seekable_ms(&port), (0, 56_800));
        // Край эфира не придуман: его даёт прошлое окно, а не цель страницы.
        assert_eq!(
            port.observe()
                .snapshot
                .state
                .as_live()
                .expect("live view")
                .live_edge(),
            player_ms(56_800)
        );
    }

    /// Перемотка плеера к цели страницы ждёт байтов: новая шкала легла чуть позже цели
    /// (0,089 с) — окно по полке не «просрочивает» уже идущую перемотку к 0.
    #[test]
    fn a_seek_waiting_for_page_bytes_keeps_its_target_when_the_new_timeline_starts_later() {
        let (mut feed, port) =
            LiveTimelineFeed::open(view(53_900, 56_800, 27_700), PushedDuration::Live, None);
        feed.note_page_seek(at_ms(0));
        feed.note_player_seek();
        feed.publish(Some(uncovered(0)), None).expect("published");
        feed.publish(
            Some(LiveShelfView {
                covered: MediaTimeSpan::new(at_ms(89), at_ms(2_089)),
                player_position: Some(at_ms(0)),
                join: Some(at_ms(27_700)),
            }),
            None,
        )
        .expect("published");
        // Окно — новая шкала (старое забыто), расширенная до цели; край — её конец.
        assert_eq!(seekable_ms(&port), (0, 2_089));
        let state = port.observe().snapshot.state.as_live().expect("live view");
        assert_eq!(state.live_edge(), player_ms(2_089));
        // Вход старой шкалы позже нового края — вход на краю, а не в будущем.
        assert_eq!(state.join_position(), player_ms(2_089));
    }

    /// Плеер перемотался — цель страницы больше не растягивает окно: эфир, в котором
    /// плеер ушёл в другое место, публикует ровно полку.
    #[test]
    fn a_player_seek_releases_the_page_target() {
        let (mut feed, port) =
            LiveTimelineFeed::open(view(10_000, 12_000, 10_000), PushedDuration::Live, None);
        feed.note_page_seek(at_ms(2_000));
        feed.publish(Some(view(10_000, 12_000, 10_000)), None)
            .expect("published");
        assert_eq!(seekable_ms(&port), (2_000, 12_000));
        feed.note_player_seek();
        feed.publish(Some(view(10_000, 13_000, 10_000)), None)
            .expect("published");
        assert_eq!(seekable_ms(&port), (10_000, 13_000));
    }

    /// Якоря внутри полки окно не меняют: обычный эфир публикует ровно полку.
    #[test]
    fn anchors_inside_the_shelf_change_nothing() {
        let (mut feed, port) =
            LiveTimelineFeed::open(view(10_000, 12_000, 10_000), PushedDuration::Live, None);
        let before = port.observe().snapshot;
        feed.note_page_seek(at_ms(10_500));
        feed.publish(
            Some(LiveShelfView {
                covered: MediaTimeSpan::new(at_ms(10_000), at_ms(12_000)),
                player_position: Some(at_ms(11_000)),
                join: Some(at_ms(10_000)),
            }),
            None,
        )
        .expect("published");
        assert_eq!(port.observe().snapshot, before);
    }

    /// Без полки и без прошлой публикации окна нет: одна цель — не диапазон.
    #[test]
    fn anchors_without_any_shelf_publish_nothing() {
        let (mut feed, port) = LiveTimelineFeed::open(uncovered(5_000), PushedDuration::Live, None);
        let before = port.observe().snapshot;
        feed.note_page_seek(at_ms(0));
        feed.publish(Some(uncovered(5_000)), None)
            .expect("nothing to publish");
        assert_eq!(port.observe().snapshot, before);
        assert_eq!(
            before.state.as_live().expect("live view").seekable_range(),
            None
        );
    }
}
