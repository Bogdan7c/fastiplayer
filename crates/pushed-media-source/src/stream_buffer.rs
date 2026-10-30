//! Буфер потока: полки и подача всех полос под одним замком.
//!
//! Владелец полок, курсоров подачи, текущих эпизодов, позиции плеера и учёта
//! байтов. Одно состояние на поток, потому что к нему ходят с разных потоков:
//! - писатель (поток соединения): приём сегментов, `changeType`, смещение, конец;
//! - монитор (поток приложения): позиция плеера и «буферизовано / полон»;
//! - перемотка (поток плеера, синхронно на ходе владельца сессии) — тот же
//!   курсор и та же полка (модуль `seek`); живой эфир публикует по полке
//!   динамическую шкалу времени (модуль `live_timeline`).
//!
//! Замок держится коротко: внутри нет ожидания ввода-вывода и `join` потоков
//! (запуск эпизода — это создание потока, без ожидания; эпизоды, выброшенные
//! перемоткой, уничтожаются вызывающим уже ВНЕ замка). Порядок замков всегда
//! «буфер → ящик эпизодов / труба / шкала эфира → учёт». Сторона плеера берёт
//! замок буфера только в перемотке и в вопросах «можно ли перематывать», и
//! только не держа других замков источника; поток демуксера эпизода замок
//! буфера не берёт никогда.
//!
//! Подача и предел ждущих эпизодов: подача с полки не начинает новый эпизод,
//! если ящик полосы полон (`feed::FeedAdmission`) — сегменты ждут на полке, а
//! подача продолжается позже: с каждым принятым сегментом, с каждой позицией
//! плеера и по просьбе стороны плеера, забравшей эпизод
//! ([`StreamBuffer::try_catch_up_feeding`] — без ожидания замка).
//!
//! Учёт (жёсткий потолок `max_held_stream_bytes`): каждый принятый init и
//! медиасегмент зачитывается один раз при приёме и освобождается, когда исчезает
//! последняя копия — на полке, в трубе эпизода, в ключе эпизода или у полосы
//! (текущий init). Полки и трубы делят память (`Bytes`), поэтому не считаются
//! дважды. Полосы без меток времени (MPEG-TS) держат сегмент только в трубе.
//! Недособранный хвост сканера в этот учёт не входит — его считает писатель.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::Duration;

use bytes::Bytes;

use crate::container::PushedContainer;
use crate::episode::EpisodeFactory;
use crate::error::PushedStreamError;
use crate::feed::{EpisodeContinuity, EpisodeKey, FeedAdmission, LaneFeed};
use crate::framing::{MediaTime, TimedSpan};
use crate::intake::{ChargedSegment, IntakeLedger};
use crate::lane::{LaneId, LaneMailbox};
use crate::live_timeline::LiveTimelineFeed;
use crate::mailbox::FeedBacklog;
use crate::page_seek::PageSeekBook;
use crate::shelf::{LaneShelf, ShelfEntry, ShelfRules};

mod eviction;
mod live;
mod position;
mod seek;

use position::StalePositionGate;
use seek::LaneSeekState;
pub(crate) use seek::{LaneLanding, StreamSeekError};

/// Размечает ли контейнер полосы время сегментов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaneTimeline {
    /// fMP4 и WebM: сегменты лежат на полке по времени.
    Timed,
    /// MPEG-TS: время не размечается, сегменты идут в трубу в порядке прихода
    /// (поведение 03a: без полки, перемотки и вклада в «буферизовано»).
    Untimed,
}

impl LaneTimeline {
    /// Временная ось полосы по её контейнеру.
    const fn of(container: PushedContainer) -> Self {
        if container.has_initialization_segment() {
            Self::Timed
        } else {
            Self::Untimed
        }
    }
}

/// Полоса внутри буфера.
#[derive(Debug)]
struct BufferedLane {
    /// Есть ли у полосы время.
    timeline: LaneTimeline,
    /// Полка (у полосы без времени всегда пуста).
    shelf: LaneShelf,
    /// Подача в эпизоды.
    feed: LaneFeed,
    /// Идёт ли подача по курсору или ждёт цель перемотки.
    seek: LaneSeekState,
    /// Куда полосу поставила последняя перемотка (для ответа плееру).
    landing: Option<LaneLanding>,
}

/// Состояние под замком.
#[derive(Debug)]
struct BufferState {
    /// Полосы.
    lanes: BTreeMap<LaneId, BufferedLane>,
    /// Позиция, которую сообщил плеер.
    reported_position: Option<MediaTime>,
    /// Начало первого поданного сегмента: позиция, пока плеер её не сообщил.
    first_fed_start: Option<MediaTime>,
    /// Разговор со страницей о перемотке.
    page_seek: PageSeekBook,
    /// Шкала живого эфира (только у потока, открытого как эфир).
    live_timeline: Option<LiveTimelineFeed>,
    /// После перемотки: позиции старого места ещё могут прийти от приложения.
    stale_position_gate: Option<StalePositionGate>,
}

impl BufferState {
    /// Пустое состояние.
    fn new(page_seek: PageSeekBook) -> Self {
        Self {
            lanes: BTreeMap::new(),
            reported_position: None,
            first_fed_start: None,
            page_seek,
            live_timeline: None,
            stale_position_gate: None,
        }
    }

    /// Позиция плеера (или начало первого поданного, пока он молчит).
    fn position(&self) -> Option<MediaTime> {
        self.reported_position.or(self.first_fed_start)
    }

    /// Полоса буфера.
    fn lane_mut(&mut self, lane: LaneId) -> Result<&mut BufferedLane, PushedStreamError> {
        self.lanes
            .get_mut(&lane)
            .ok_or(PushedStreamError::UnknownLane { lane })
    }

    /// Полосы с метками времени.
    fn timed_lanes_mut(&mut self) -> impl Iterator<Item = &mut BufferedLane> {
        self.lanes
            .values_mut()
            .filter(|lane| lane.timeline == LaneTimeline::Timed)
    }
}

/// Медиасегмент с временем, готовый лечь на полку.
#[derive(Debug)]
pub(crate) struct TimedSegment {
    /// Время показа после смещения полосы и точность его конца.
    pub(crate) timing: TimedSpan,
    /// Байты (ещё не зачтённые).
    pub(crate) media: Bytes,
    /// С чем его играть.
    pub(crate) key: EpisodeKey,
}

/// Буфер потока (общий для писателя, монитора и — в пакете F — перемотки).
#[derive(Debug)]
pub(crate) struct StreamBuffer {
    /// Полки, подача, позиция.
    state: Mutex<BufferState>,
    /// Учёт всех байтов потока.
    ledger: Arc<IntakeLedger>,
    /// Запуск эпизодов.
    episodes: EpisodeFactory,
    /// Окно и допуски полки.
    rules: ShelfRules,
    /// Удерживаемые байты, с которых источник говорит «полон».
    full_at_held_bytes: usize,
    /// Допуск совпадения цели перемотки с позицией (и с целью страницы).
    seek_match_tolerance: Duration,
    /// Сколько после перемотки не верить позициям далеко от цели.
    stale_position_window: Duration,
}

/// Настройки буфера потока (из [`crate::PushedSourcePolicy`]).
#[derive(Debug, Clone, Copy)]
pub(crate) struct StreamBufferRules {
    /// Окно и допуски полки.
    pub(crate) shelf: ShelfRules,
    /// Допуск совпадения цели плеера с целью перемотки страницы.
    pub(crate) page_seek_match_tolerance: Duration,
    /// Удерживаемые байты, с которых источник говорит «полон» (меньше жёсткого
    /// потолка: честная страница останавливается раньше, чем поток умрёт).
    pub(crate) full_at_held_bytes: usize,
    /// Сколько после перемотки не верить позициям далеко от цели.
    pub(crate) stale_position_window: Duration,
}

impl StreamBuffer {
    /// Пустой буфер.
    pub(crate) fn new(
        ledger: Arc<IntakeLedger>,
        episodes: EpisodeFactory,
        rules: StreamBufferRules,
    ) -> Self {
        Self {
            state: Mutex::new(BufferState::new(PageSeekBook::new(
                rules.page_seek_match_tolerance,
            ))),
            ledger,
            episodes,
            rules: rules.shelf,
            full_at_held_bytes: rules.full_at_held_bytes,
            seek_match_tolerance: rules.page_seek_match_tolerance,
            stale_position_window: rules.stale_position_window,
        }
    }

    /// Заводит полосу.
    pub(crate) fn add_lane(
        &self,
        lane: LaneId,
        container: PushedContainer,
        mailbox: Arc<LaneMailbox>,
        max_pending_episodes: usize,
    ) {
        let buffered = BufferedLane {
            timeline: LaneTimeline::of(container),
            shelf: LaneShelf::default(),
            feed: LaneFeed::new(lane, mailbox, max_pending_episodes),
            seek: LaneSeekState::Following,
            landing: None,
        };
        self.lock().lanes.insert(lane, buffered);
    }

    /// `changeType`: другой контейнер — текущий эпизод полосы закончен.
    ///
    /// Смена «с временем → без времени» (fMP4/WebM → MPEG-TS): сегменты TS идут в
    /// трубу в порядке прихода, полка полосе больше не нужна. Сначала подаётся
    /// всё, что продолжает поданное (как MSE доиграл бы буферизованное), затем
    /// полка и курсор отпускаются целиком: ждущие сегменты (за дырой или за
    /// полным ящиком) больше никто не подаст, а поданные дочитает труба. Иначе
    /// они висели бы в учёте навсегда — выброс смотрит только полосы с временем.
    pub(crate) fn change_lane_container(
        &self,
        lane: LaneId,
        container: PushedContainer,
    ) -> Result<(), PushedStreamError> {
        let next_timeline = LaneTimeline::of(container);
        let released = {
            let mut state = self.lock();
            let was_timed = state.lane_mut(lane)?.timeline == LaneTimeline::Timed;
            let released = if was_timed && next_timeline == LaneTimeline::Untimed {
                self.feed_contiguous(&mut state, lane)?;
                let buffered = state.lane_mut(lane)?;
                buffered.feed.forget_cursor();
                buffered.feed.note_backlog(FeedBacklog::Clear);
                buffered.shelf.take_all()
            } else {
                Vec::new()
            };
            let buffered = state.lane_mut(lane)?;
            buffered.timeline = next_timeline;
            buffered.feed.finish_running();
            released
        };
        // Учёт освобождается вне замка буфера (замок учёта короткий, но порядок
        // замков от этого только проще).
        drop(released);
        Ok(())
    }

    /// Новый init или смещение: текущий эпизод больше не получает байтов (его
    /// демуксер дочитает и закончит сразу, а не со следующим сегментом).
    pub(crate) fn finish_running_episode(&self, lane: LaneId) -> Result<(), PushedStreamError> {
        self.lock().lane_mut(lane)?.feed.finish_running();
        Ok(())
    }

    /// Зачитывает байты в учёт (init полосы).
    pub(crate) fn charge(&self, bytes: Bytes) -> Result<ChargedSegment, PushedStreamError> {
        let mut state = self.lock();
        self.charge_locked(&mut state, bytes)
    }

    /// Сегмент с временем: на полку, затем подать всё, что стало непрерывным.
    pub(crate) fn accept_timed(
        &self,
        lane: LaneId,
        segment: TimedSegment,
    ) -> Result<(), PushedStreamError> {
        let mut state = self.lock();
        // Сначала снимаем то, что новый сегмент заменит: его место освобождается
        // до проверки потолка (повтор того же времени не требует двойной памяти).
        let replaced = state
            .lane_mut(lane)?
            .shelf
            .take_overlapped_by(segment.timing, &self.rules);
        drop(replaced);
        let media = self.charge_locked(&mut state, segment.media)?;
        state
            .lane_mut(lane)?
            .shelf
            .insert(ShelfEntry::waiting(segment.timing, media, segment.key));
        // Полоса после перемотки ждёт сегмент с целью; иначе — подача по курсору.
        // Заодно догоняются полосы, у которых подача стояла на полном ящике.
        self.resume_if_target_arrived(&mut state, lane)?;
        self.feed_contiguous(&mut state, lane)?;
        self.catch_up_backlogged_lanes(&mut state)?;
        self.evict_behind_window(&mut state);
        self.publish_live_timeline(&mut state);
        Ok(())
    }

    /// Сегмент без времени (MPEG-TS): сразу в трубу, в порядке прихода.
    pub(crate) fn accept_untimed(
        &self,
        lane: LaneId,
        media: Bytes,
        key: &EpisodeKey,
    ) -> Result<(), PushedStreamError> {
        let mut state = self.lock();
        let media = self.charge_locked(&mut state, media)?;
        state.lane_mut(lane)?.feed.deliver(
            media,
            key,
            EpisodeContinuity::MayContinue,
            &self.episodes,
        )
    }

    /// Конец входа полосы (`endOfStream`): поданное доиграется, дальше — конец.
    pub(crate) fn end_lane_input(&self, lane: LaneId) -> Result<(), PushedStreamError> {
        self.lock().lane_mut(lane)?.feed.end_input();
        Ok(())
    }

    /// Вход полосы снова открыт (`appendBuffer` после `endOfStream`).
    pub(crate) fn reopen_lane_input(&self, lane: LaneId) -> Result<(), PushedStreamError> {
        self.lock().lane_mut(lane)?.feed.reopen_input();
        Ok(())
    }

    /// Плеер закрыл поток: полка полосы и её эпизод больше никому не нужны.
    pub(crate) fn discard_lane(&self, lane: LaneId) -> Result<(), PushedStreamError> {
        let mut state = self.lock();
        let buffered = state.lane_mut(lane)?;
        buffered.shelf.clear();
        buffered.feed.finish_running();
        Ok(())
    }

    /// Сколько байтов держит поток (полки, трубы, init; без хвостов сканеров).
    pub(crate) fn held_segment_bytes(&self) -> usize {
        self.ledger.held_bytes()
    }

    /// Сторона плеера забрала эпизод, а у полосы есть недоподанное: продолжить
    /// подачу, если замок буфера свободен прямо сейчас.
    ///
    /// Поток плеера на замке буфера НЕ ждёт (`try_lock`): занят — значит, его
    /// держит писатель, монитор или перемотка, и плеер спросит снова на
    /// следующем шаге (ящик до тех пор отвечает «подожди», а не «конец»). Под
    /// замком — только подача: запуск эпизода (создание потока) без ожидания.
    pub(crate) fn try_catch_up_feeding(&self, lane: LaneId) -> Result<(), PushedStreamError> {
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return Ok(()),
        };
        self.catch_up_lane(&mut state, lane)
    }

    /// Догоняет подачу всех полос, у которых она стояла на полном ящике.
    fn catch_up_backlogged_lanes(&self, state: &mut BufferState) -> Result<(), PushedStreamError> {
        let backlogged: Vec<LaneId> = state
            .lanes
            .iter()
            .filter(|(_, lane)| lane.timeline == LaneTimeline::Timed && lane.feed.has_backlog())
            .map(|(lane, _)| *lane)
            .collect();
        for lane in backlogged {
            self.catch_up_lane(state, lane)?;
        }
        Ok(())
    }

    /// Продолжает подачу одной полосы (после перемотки — с цели).
    fn catch_up_lane(
        &self,
        state: &mut BufferState,
        lane: LaneId,
    ) -> Result<(), PushedStreamError> {
        if state.lane_mut(lane)?.timeline != LaneTimeline::Timed {
            return Ok(());
        }
        self.resume_if_target_arrived(state, lane)?;
        self.feed_contiguous(state, lane)
    }

    /// Подаёт с полки всё, что продолжает курсор, по порядку времени.
    ///
    /// Останавливается, если следующий сегмент требует нового эпизода, а ящик
    /// полон: сегмент ждёт на полке, полоса помечается «есть недоподанное».
    /// После `endOfStream` текущий эпизод в конце дочитывается до конца (новых
    /// байтов не будет; продолжение подачи начнёт новый эпизод).
    fn feed_contiguous(
        &self,
        state: &mut BufferState,
        lane: LaneId,
    ) -> Result<(), PushedStreamError> {
        let BufferState {
            lanes,
            first_fed_start,
            ..
        } = state;
        let buffered = lanes
            .get_mut(&lane)
            .ok_or(PushedStreamError::UnknownLane { lane })?;
        if buffered.seek != LaneSeekState::Following {
            // Полоса ждёт цель перемотки: курсора нет, и «первый ждущий» сегмент
            // полки — вовсе не продолжение. Подачу начнёт сегмент с целью.
            return Ok(());
        }
        let mut backlog = FeedBacklog::Clear;
        while let Some(choice) = buffered
            .shelf
            .next_to_feed(buffered.feed.cursor(), &self.rules)
        {
            let admission = buffered
                .shelf
                .key_of(choice)
                .map(|key| buffered.feed.admission(key, choice.continuity()));
            if admission == Some(FeedAdmission::Deferred) {
                backlog = FeedBacklog::Pending;
                break;
            }
            // Выбранный сегмент отмечается поданным — цикл всегда продвигается.
            let Some(fed) = buffered.shelf.take_for_feeding(choice, &self.rules) else {
                break;
            };
            buffered
                .feed
                .deliver(fed.media, &fed.key, fed.continuity, &self.episodes)?;
            buffered.feed.advance_cursor(fed.fed_to);
            first_fed_start.get_or_insert(fed.span.start());
        }
        buffered.feed.note_backlog(backlog);
        buffered.feed.finish_running_if_input_ended();
        Ok(())
    }

    /// Захватывает состояние; отравленный mutex не мешает работе.
    fn lock(&self) -> MutexGuard<'_, BufferState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
#[path = "stream_buffer_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "stream_buffer_limits_tests.rs"]
mod limits_tests;
