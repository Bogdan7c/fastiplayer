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
//! 5. **У полосы нет данных у цели, а у других полос есть** (звук начинается
//!    позже видео; страница прислала звук только со своего сегмента после цели) —
//!    полоса вступает со своего первого сегмента после цели, как на обычном
//!    старте, если другие полосы непрерывно покрывают время от цели до него
//!    ([`crate::shelf::LateJoinWindow`]). Плеер садится на цель по видео, а звук
//!    подхватывается в своё время. Без этого правила полоса вечно ждала бы
//!    сегмент с целью, которого в медиа нет. Опора — только полосы, у которых
//!    цель есть: если цели нет ни у одной полосы, данные после неё могут быть
//!    просто ещё не присланным временем, и полосы ждут страницу (правило 2).
//!    Окно проверяется заново с каждым принятым сегментом любой полосы: звук,
//!    пришедший раньше видео, вступает, когда видео покроет его начало. Данные
//!    полосы внутри куска опор раньше точки вступления — это дыра посреди
//!    полосы, а не позднее начало: вступления нет, просим страницу. Если цель
//!    попросили у страницы, точкой вступления годится только сегмент, принятый
//!    ПОСЛЕ перемотки (ответ страницы): лежавшее на полке позже цели — время
//!    прежних мест (перемотка чуть назад от уже скачанного), и вступив с него,
//!    полоса пропустила бы данные цели, которые страница ещё несёт.
//!    Ограничение: если страница, у которой эти байты уже есть в её буфере, не
//!    пришлёт их заново, такая полоса ждёт, пока страница жива (решение 150: срок
//!    перемотки плеера честное ожидание живой страницы не засчитывает).
//!
//! После перемотки позиции, далёкие от цели, не принимаются, пока плеер не сообщит
//! цель, но не дольше окна успокоения от момента посадки (модуль `position`):
//! позиция старого места может прийти позже перемотки, а плеер стоит на старом
//! месте до своего коммита.
//!
//! Перемотка вызывается с хода владельца сессии плеера и не блокируется: замок
//! буфера, запуск эпизода (создание потока) и ничего больше. Выгребенные эпизоды
//! возвращаются вызывающему ([`StreamSeek::stale`]) и уничтожаются вне замка.

use std::time::{Duration, Instant};

use super::{BufferState, LaneTimeline, StreamBuffer};
use crate::error::PushedStreamError;
use crate::feed::FeedAdmission;
use crate::framing::{MediaTime, MediaTimeSpan};
use crate::lane::LaneId;
use crate::mailbox::{FeedBacklog, StaleEpisodes};
use crate::page_seek::{PageSeekDecision, PageSeekNeed, PageSeekRequest};
use crate::shelf::{LateJoinCandidates, LateJoinWindow, ShelfRules};

/// Как полоса подаёт сегменты.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LaneSeekState {
    /// Обычная подача: всё, что продолжает курсор.
    Following,
    /// После перемотки: ждём сегмент с целью (или точку позднего вступления,
    /// правило 5), до него ничего не подаём.
    AwaitingTarget {
        /// Цель перемотки.
        target: MediaTime,
        /// Годятся ли для позднего вступления сегменты, лежавшие на полке до
        /// перемотки: да, если полоса могла начать с полки уже при перемотке.
        late_join: LateJoinCandidates,
    },
}

/// Куда перемотка поставила полосу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaneLanding {
    /// Подача началась с полки: с начала сегмента, внутри которого цель, или
    /// (полоса вступает позже, правило 5) с первого сегмента после цели.
    FromShelf(MediaTime),
    /// Цели на полке нет: полоса ждёт байтов страницы с цели.
    AwaitingPage(MediaTime),
}

impl LaneLanding {
    /// Позиция, с которой плеер получит пакеты: не позже цели.
    ///
    /// С полки — начало сегмента (у честного фрагмента это ключевой кадр; если
    /// нет — демуксер полосы всё равно ждёт ключевой кадр). Если цель попала в
    /// маленькую дыру перед сегментом или полоса вступает позже (правило 5), его
    /// начало позже цели — тогда отвечаем самой целью: ответ плееру всегда не
    /// позже цели (плеер садится на цель, звук, начинающийся позже, дополняет
    /// тишиной). Без полки — сама цель: плеер декодирует вперёд до цели и прячет
    /// всё, что раньше.
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
        let need = if state
            .lanes
            .keys()
            .all(|lane| can_land_from_shelf(&state, *lane, target, &self.rules))
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
        // Все полосы сели с полки сразу — окно успокоения считается отсюда.
        Self::note_seek_landing_if_landed(&mut state, Instant::now());
        self.relieve_memory_pressure(&mut state);
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

    /// Одна ли это перемотка: цели совпадают в пределах допуска совпадения цели
    /// плеера с целью страницы (то же правило, что у журнала разговора со страницей).
    pub(crate) fn is_same_seek_target(&self, first: Duration, second: Duration) -> bool {
        first.abs_diff(second) <= self.seek_match_tolerance
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
        let lands_from_shelf = can_land_from_shelf(state, lane, target, &self.rules);
        let buffered = state.lane_mut(lane).map_err(StreamSeekError::Stream)?;
        let stale = buffered.feed.restart_for_seek();
        buffered.shelf.note_seek();
        // Полоса, которая не может начать с полки сейчас, ждёт страницу: позднее
        // вступление — только сегментом из её ответа (правило 5).
        let late_join = if lands_from_shelf {
            LateJoinCandidates::AnyOnShelf
        } else {
            LateJoinCandidates::ArrivedSinceSeek
        };
        buffered.seek = LaneSeekState::AwaitingTarget { target, late_join };
        buffered.landing = Some(LaneLanding::AwaitingPage(target));
        if !lands_from_shelf {
            // Байты с цели придут от страницы, даже если она уже сказала
            // `endOfStream` (перемотка назад после конца): до них плеер ждёт, а не
            // получает конец потока.
            buffered.feed.reopen_input();
        }
        self.resume_if_target_arrived(state, lane)
            .map_err(StreamSeekError::Stream)?;
        Ok(stale)
    }

    /// Каждая полоса, ждущая цель перемотки, начинает подачу, если может.
    ///
    /// Зовётся с каждым принятым сегментом: сегмент одной полосы может открыть
    /// позднее вступление ДРУГОЙ (правило 5 — видео покрыло начало звука).
    pub(super) fn resume_lanes_awaiting_target(
        &self,
        state: &mut BufferState,
    ) -> Result<(), PushedStreamError> {
        let awaiting: Vec<LaneId> = state
            .lanes
            .iter()
            .filter(|(_, lane)| {
                lane.timeline == LaneTimeline::Timed
                    && matches!(lane.seek, LaneSeekState::AwaitingTarget { .. })
            })
            .map(|(lane, _)| *lane)
            .collect();
        for lane in awaiting {
            self.resume_if_target_arrived(state, lane)?;
        }
        Ok(())
    }

    /// Полоса ждёт цель перемотки: если сегмент с целью уже на полке, подача
    /// начинается с него в новом эпизоде, и дальше — всё непрерывное; если цели у
    /// полосы нет, а другие полосы её покрывают — с первого сегмента после цели
    /// (правило 5).
    pub(super) fn resume_if_target_arrived(
        &self,
        state: &mut BufferState,
        lane: LaneId,
    ) -> Result<(), PushedStreamError> {
        let rules = self.rules;
        let LaneSeekState::AwaitingTarget { target, late_join } = state.lane_mut(lane)?.seek else {
            return Ok(());
        };
        // Окно считается до изменений полосы: оно зависит только от полок других полос.
        let late_join_window = late_join_window(state, lane, target, &rules);
        let buffered = state.lane_mut(lane)?;
        let incoming = buffered.feed.incoming();
        let choice = buffered
            .shelf
            .prepare_restart(target, &rules, incoming)
            .or_else(|| {
                late_join_window
                    .and_then(|window| buffered.shelf.prepare_late_join(window, late_join))
            });
        let Some(choice) = choice else {
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
        // Полоса дождалась цели: если она последняя, с этого момента считается окно
        // успокоения позиций (модуль `position`).
        Self::note_seek_landing_if_landed(state, Instant::now());
        // Дальше — всё непрерывное; после `endOfStream` эпизод в конце
        // дочитывается до конца (это делает сама подача).
        self.feed_contiguous(state, lane)
    }
}

/// Может ли полоса после перемотки к `target` начать подачу с того, что уже
/// лежит на полке: цель у неё есть (правила 1–4) или она вступает позже по
/// другим полосам (правило 5). Иначе её байты должна прислать страница.
pub(super) fn can_land_from_shelf(
    state: &BufferState,
    lane: LaneId,
    target: MediaTime,
    rules: &ShelfRules,
) -> bool {
    let Some(buffered) = state.lanes.get(&lane) else {
        return false;
    };
    buffered
        .shelf
        .holds(target, rules, buffered.feed.incoming())
        || late_join_window(state, lane, target, rules).is_some_and(|window| {
            buffered
                .shelf
                .can_join_late(window, LateJoinCandidates::AnyOnShelf)
        })
}

/// Окно позднего вступления полосы `lane` (правило 5): непрерывные данные
/// ДРУГИХ полос, покрывающие цель; у нескольких опор — их общая часть (вступать
/// можно только туда, где данные есть у всех опор).
///
/// `None` — ни одна другая полоса цель не покрывает: опоры нет, и данные полосы
/// после цели могут быть просто ещё не присланным временем.
fn late_join_window(
    state: &BufferState,
    lane: LaneId,
    target: MediaTime,
    rules: &ShelfRules,
) -> Option<LateJoinWindow> {
    let covered = state
        .lanes
        .iter()
        .filter(|(other, buffered)| **other != lane && buffered.timeline == LaneTimeline::Timed)
        .filter_map(|(_, buffered)| buffered.shelf.covered_run(target, rules))
        .reduce(|common, run| {
            // Пересечение кусков опор (каждый покрывает цель, так что оно не пусто
            // по смыслу; пустое — значит общего времени нет, `new` откажет).
            let start = common.start().max(run.start());
            let end = common.end().min(run.end());
            MediaTimeSpan::new(start, end).unwrap_or_else(|| MediaTimeSpan::empty_at(start))
        })?;
    LateJoinWindow::new(target, covered)
}
