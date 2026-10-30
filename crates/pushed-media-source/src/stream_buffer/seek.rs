//! Перемотка потока на стороне буфера: полка, курсоры, эпохи, запрос странице.
//!
//! Механизм (подтверждён владельцем):
//! 1. **Цель на полке у всех полос** — каждая полоса заканчивает текущий эпизод,
//!    выгребает из ящика эпизоды старой позиции (новая эпоха), и подача начинается
//!    заново с сегмента, внутри которого лежит цель: он и всё после него снова
//!    «ждут», курсор ставится заново (единственный путь курсора назад).
//!    Страницу не трогаем.
//! 2. **Цели на полке нет** — источник просит страницу перемотаться (запрос в
//!    журнале [`crate::page_seek`], последний побеждает), а полоса без цели ждёт:
//!    пришедшие сегменты ложатся на полку, но не подаются, пока не придёт сегмент
//!    с целью (или начинающийся не дальше наибольшей дыры после неё — плееры
//!    страниц прижимают перемотку к границе сегмента). Полоса, у которой цель
//!    на полке есть, начинает с полки сразу.
//! 3. **Перемотку начала страница** — приложение заранее отметило её цель; тот
//!    же путь, только без запроса.
//! 4. **После `endOfStream` цель за концом полки** (перемотка на длительность, в
//!    последний кадр с незаписанной длительностью) — полоса садится на последний
//!    сегмент и доигрывает его до конца потока; вход не открывается заново и
//!    страницу не просят: она уже сказала, что байтов больше нет.
//!
//! После перемотки позиции, далёкие от цели, какое-то время не принимаются
//! (модуль `position`): позиция старого места может прийти позже перемотки.
//!
//! Перемотка вызывается с хода владельца сессии плеера и не блокируется: замок
//! буфера, запуск эпизода (создание потока) и ничего больше. Выгребенные эпизоды
//! возвращаются вызывающему ([`StreamSeek::stale`]) и уничтожаются вне замка.

use std::time::Duration;

use super::{BufferState, LaneTimeline, StreamBuffer};
use crate::error::PushedStreamError;
use crate::feed::FeedAdmission;
use crate::framing::MediaTime;
use crate::lane::LaneId;
use crate::mailbox::{FeedBacklog, StaleEpisodes};
use crate::page_seek::{PageSeekDecision, PageSeekNeed, PageSeekRequest};

/// Как полоса подаёт сегменты.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LaneSeekState {
    /// Обычная подача: всё, что продолжает курсор.
    Following,
    /// После перемотки: ждём сегмент с целью, до него ничего не подаём.
    AwaitingTarget(MediaTime),
}

/// Куда перемотка поставила полосу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaneLanding {
    /// Подача началась с полки, с начала сегмента, внутри которого цель.
    FromShelf(MediaTime),
    /// Цели на полке нет: полоса ждёт байтов страницы с цели.
    AwaitingPage(MediaTime),
}

impl LaneLanding {
    /// Позиция, с которой плеер получит пакеты: не позже цели.
    ///
    /// С полки — начало сегмента (у честного фрагмента это ключевой кадр; если
    /// нет — демуксер полосы всё равно ждёт ключевой кадр). Если цель попала в
    /// маленькую дыру перед сегментом, его начало чуть позже цели — тогда
    /// отвечаем самой целью: ответ плееру всегда не позже цели. Без полки —
    /// сама цель: плеер декодирует вперёд до цели и прячет всё, что раньше.
    pub(crate) fn decode_start(self, target: MediaTime) -> MediaTime {
        match self {
            Self::FromShelf(start) => start.min(target),
            Self::AwaitingPage(_) => target,
        }
    }
}

/// Перемотка невозможна или сломала поток.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamSeekError {
    /// Есть полоса без меток времени (MPEG-TS): на полке нечего искать.
    UntimedLane {
        /// Полоса.
        lane: LaneId,
    },
    /// Ошибка потока (например, не запустился эпизод) — первая ошибка потока.
    Stream(PushedStreamError),
}

/// Итог перемотки буфера.
#[derive(Debug)]
pub(crate) struct StreamSeek {
    /// Эпизоды старой позиции: уничтожить вне замков.
    pub(crate) stale: StaleEpisodes,
    /// Что решено насчёт страницы.
    pub(crate) page: PageSeekDecision,
}

impl StreamBuffer {
    /// Перематывает все полосы к `target` (правила — в описании модуля).
    pub(crate) fn seek(&self, target: Duration) -> Result<StreamSeek, StreamSeekError> {
        let target = MediaTime::from_player_position(target);
        let mut state = self.lock();
        if let Some((lane, _)) = state
            .lanes
            .iter()
            .find(|(_, lane)| lane.timeline == LaneTimeline::Untimed)
        {
            return Err(StreamSeekError::UntimedLane { lane: *lane });
        }
        let rules = self.rules;
        let need = if state
            .lanes
            .values()
            .all(|lane| lane.shelf.holds(target, &rules, lane.feed.incoming()))
        {
            PageSeekNeed::OnShelf
        } else {
            PageSeekNeed::OffShelf
        };
        let page = state.page_seek.record_player_seek(target, need);
        // Плеер встаёт на цель: от неё считается «буферизовано», и до первого
        // сообщения приложения позади неё ничего не выбрасывается раньше времени.
        state.reported_position = Some(target);
        Self::arm_stale_position_gate(&mut state, target);
        let mut stale = StaleEpisodes::default();
        let lanes: Vec<LaneId> = state.lanes.keys().copied().collect();
        for lane in lanes {
            stale.absorb(self.restart_lane(&mut state, lane, target)?);
        }
        self.evict_behind_window(&mut state);
        self.publish_live_timeline(&mut state);
        Ok(StreamSeek { stale, page })
    }

    /// Куда последняя перемотка поставила полосу (`None` — перемотки не было).
    pub(crate) fn lane_landing(&self, lane: LaneId) -> Option<LaneLanding> {
        self.lock().lanes.get(&lane).and_then(|lane| lane.landing)
    }

    /// Можно ли перематывать по меткам времени: все полосы их размечают.
    pub(crate) fn all_lanes_timed(&self) -> bool {
        self.lock()
            .lanes
            .values()
            .all(|lane| lane.timeline == LaneTimeline::Timed)
    }

    /// Страница сама начала перемотку к `target` (в порядке прихода с байтами).
    pub(crate) fn note_page_seek(&self, target: Duration) {
        self.lock()
            .page_seek
            .note_page_seek(MediaTime::from_player_position(target));
    }

    /// Забирает запрос странице «перемотай на T» (последний; не более раза).
    pub(crate) fn take_page_seek_request(&self) -> Option<PageSeekRequest> {
        self.lock().page_seek.take_request()
    }

    /// Начинает подачу полосы заново от цели.
    fn restart_lane(
        &self,
        state: &mut BufferState,
        lane: LaneId,
        target: MediaTime,
    ) -> Result<StaleEpisodes, StreamSeekError> {
        let buffered = state.lane_mut(lane).map_err(StreamSeekError::Stream)?;
        let stale = buffered.feed.restart_for_seek();
        buffered.seek = LaneSeekState::AwaitingTarget(target);
        buffered.landing = Some(LaneLanding::AwaitingPage(target));
        if !buffered
            .shelf
            .holds(target, &self.rules, buffered.feed.incoming())
        {
            // Байты с цели придут от страницы, даже если она уже сказала
            // `endOfStream` (перемотка назад после конца): до них плеер ждёт, а не
            // получает конец потока.
            buffered.feed.reopen_input();
        }
        self.resume_if_target_arrived(state, lane)
            .map_err(StreamSeekError::Stream)?;
        Ok(stale)
    }

    /// Полоса ждёт цель перемотки: если сегмент с целью уже на полке, подача
    /// начинается с него в новом эпизоде, и дальше — всё непрерывное.
    pub(super) fn resume_if_target_arrived(
        &self,
        state: &mut BufferState,
        lane: LaneId,
    ) -> Result<(), PushedStreamError> {
        let rules = self.rules;
        let buffered = state.lane_mut(lane)?;
        let LaneSeekState::AwaitingTarget(target) = buffered.seek else {
            return Ok(());
        };
        let incoming = buffered.feed.incoming();
        let Some(choice) = buffered.shelf.prepare_restart(target, &rules, incoming) else {
            return Ok(());
        };
        // Перемотка уже выгребла ящик, так что место под новый эпизод почти
        // всегда есть; если нет — ждём, как и обычная подача.
        let admission = buffered
            .shelf
            .key_of(choice)
            .map(|key| buffered.feed.admission(key, choice.continuity()));
        if admission == Some(FeedAdmission::Deferred) {
            buffered.feed.note_backlog(FeedBacklog::Pending);
            return Ok(());
        }
        let Some(fed) = buffered.shelf.take_for_feeding(choice, &rules) else {
            return Ok(());
        };
        buffered.seek = LaneSeekState::Following;
        buffered.landing = Some(LaneLanding::FromShelf(fed.span.start()));
        buffered
            .feed
            .deliver(fed.media, &fed.key, fed.continuity, &self.episodes)?;
        buffered.feed.advance_cursor(fed.fed_to);
        // Дальше — всё непрерывное; после `endOfStream` эпизод в конце
        // дочитывается до конца (это делает сама подача).
        self.feed_contiguous(state, lane)
    }
}
