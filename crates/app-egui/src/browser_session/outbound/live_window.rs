//! Окно шкалы HLS странице: `live_window` (протокол 1.2, сессия 10a3, решение 232) и
//! `recording_window` (протокол 1.11, сессия 16d1, решения 325–327).
//!
//! Плеер сам ведёт HLS и знает честное окно перемотки, страница — нет. Тень страницы
//! показывает по этим сообщениям `seekable` и прижимает перемотку страницы к краям окна.
//! Всё — во времени плеера, том же, что у `clock.position_us` (`playback::live_window`).
//!
//! **Эфир или запись решает плеер** (решение 327, бэклог п. 41): пока шкала — эфир, уходит
//! `live_window`; когда шкала — запись (эфир кончился `EXT-X-ENDLIST` или плейлист был
//! записью с открытия), — `recording_window` с краем записи. Тень эфира по нему становится
//! записью (конечная длительность странице), тень файла его не замечает. Это одно «окно
//! шкалы» с одним учётом: переход эфира в запись — просто изменённое окно, предел
//! частоты общий.
//!
//! **Когда окно уходит** (договор `limits::MIN_LIVE_WINDOW_INTERVAL_MS`):
//! - только странице, чья общая версия протокола включает версию сообщения
//!   ([`LIVE_WINDOW_SINCE`], [`RECORDING_WINDOW_SINCE`]): декодер старой страницы отверг бы
//!   незнакомое сообщение и закрыл бы сеанс;
//! - только когда окно есть (эфир с известным краем) и изменилось с прошлого раза;
//! - не чаще раза в [`LIVE_WINDOW_INTERVAL`] на поколение; первое окно поколения —
//!   сразу. Частые изменения сводятся: в отправку встаёт то окно, которое плеер видит в
//!   момент, когда срок истёк, — последнее, промежуточные не нужны никому;
//! - чьё это окно и показано ли поколение, решает вызывающий (медиа-часть): сюда
//!   приходит уже окно показанного поколения.
//!
//! **Почему последнее окно не теряется без своего таймера.** Учёт хранит только то, что
//! уже поставлено в отправку. Вызывающий сообщает окно на каждом проходе цикла, а пока
//! поколение загружено, цикл просыпается не реже тика часов ([`super::CLOCK_INTERVAL`],
//! 250 мс — меньше предела, это проверяется при сборке). Изменённое окно, придержанное
//! пределом, на первом проходе после срока отличается от поставленного и уходит.
//!
//! **Доставка.** Окно — единичное событие очереди с местом «live_window»
//! (`replacement_slot`): при полной очереди или переподключении страницы оно ждёт, а
//! неотправленное старое заменяется новым. Учёт частоты ведётся по моменту постановки:
//! поставленное окно уйдёт, и ставить его копию снова незачем.
//!
//! **Узнала ли страница время эфира** ([`PageOutbound::live_window_told`]). Тень
//! страницы сопоставляет своё время со временем плеера только по часам, пришедшим ПОСЛЕ
//! первого `live_window` поколения; до этого её перемотки — в её собственном времени.
//! Поэтому учёт помнит поколение, чьё окно уже ДОСТАВЛЕНО странице (ушло в очередь
//! сервера), а не просто поставлено: окно, застрявшее у отключённой страницы, она ещё
//! не видела.

use std::time::{Duration, Instant};

use browser_bridge_protocol::limits::MIN_LIVE_WINDOW_INTERVAL_MS;
use browser_bridge_protocol::{
    LiveSeekable, MediaTime, PlayerSessionMessage, ProtocolVersion, StreamGeneration,
};

use super::resource_requests::PageProtocol;
use super::{CLOCK_INTERVAL, PageOutbound, clock_position};
use crate::browser_session::playback::live_window::LiveWindowObservation;

/// С какой версии протокола есть `live_window` (`PlayerSessionMessage::introduced_in`;
/// равенство закреплено тестом).
pub(crate) const LIVE_WINDOW_SINCE: ProtocolVersion = ProtocolVersion::V1_2;

/// С какой версии протокола есть `recording_window` (равенство закреплено тестом).
pub(crate) const RECORDING_WINDOW_SINCE: ProtocolVersion = ProtocolVersion::V1_11;

/// Наименьший промежуток между двумя `live_window` одного поколения (договор протокола).
pub(crate) const LIVE_WINDOW_INTERVAL: Duration =
    Duration::from_millis(MIN_LIVE_WINDOW_INTERVAL_MS);

// Последнее окно уходит без своего таймера, только если цикл просыпается чаще предела
// (описание модуля). Сборка не пройдёт, если кто-то сделает тик часов реже.
const _: () = assert!(CLOCK_INTERVAL.as_millis() < LIVE_WINDOW_INTERVAL.as_millis());

/// Окно эфира словами протокола: край и куда можно перемотать, в микросекундах.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PageLiveWindow {
    /// Край эфира.
    edge_us: MediaTime,
    /// Окно перемотки или «перемотки нет».
    seekable: LiveSeekable,
}

/// Окно записи словами протокола: край записи и куда можно перемотать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PageRecordingWindow {
    /// Край записи — её длительность.
    end_us: MediaTime,
    /// Окно перемотки или «перемотки нет».
    seekable: LiveSeekable,
}

/// Окно шкалы, которое страница должна знать: эфир или запись.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageTimelineWindow {
    /// Шкала — эфир (`live_window`).
    Live(PageLiveWindow),
    /// Шкала — запись (`recording_window`).
    Recording(PageRecordingWindow),
}

impl PageTimelineWindow {
    /// Наблюдение → слова протокола; `None` — окна нет (медиа нет или край неизвестен).
    ///
    /// Перевод в микросекунды у всех моментов один и тот же (`clock_position`, насыщение у
    /// 2⁵³), поэтому порядок `start ≤ end ≤ край`, гарантированный ядром, сохраняется.
    /// Нарушенный порядок не прячется: проверка протокола при отправке отвергнет
    /// сообщение и запишет дефект в журнал (`PageOutbound::deliver`).
    fn from_observation(window: LiveWindowObservation) -> Option<Self> {
        match window {
            LiveWindowObservation::NotLive | LiveWindowObservation::LiveEdgeUnknown => None,
            LiveWindowObservation::LiveWithoutDvr { edge } => Some(Self::Live(PageLiveWindow {
                edge_us: clock_position(edge),
                seekable: LiveSeekable::None {},
            })),
            LiveWindowObservation::LiveWithDvr { edge, start, end } => {
                Some(Self::Live(PageLiveWindow {
                    edge_us: clock_position(edge),
                    seekable: seekable_range(start, end),
                }))
            }
            LiveWindowObservation::Recording { end, seekable } => {
                Some(Self::Recording(PageRecordingWindow {
                    end_us: clock_position(end),
                    seekable: seekable.map_or(LiveSeekable::None {}, |range| {
                        seekable_range(range.start, range.end)
                    }),
                }))
            }
        }
    }

    /// С какой версии протокола страница знает это сообщение.
    const fn introduced_in(self) -> ProtocolVersion {
        match self {
            Self::Live(_) => LIVE_WINDOW_SINCE,
            Self::Recording(_) => RECORDING_WINDOW_SINCE,
        }
    }

    /// Сообщение протокола для поколения `generation`.
    const fn message(self, generation: StreamGeneration) -> PlayerSessionMessage {
        match self {
            Self::Live(window) => PlayerSessionMessage::LiveWindow {
                generation,
                edge_us: window.edge_us,
                seekable: window.seekable,
            },
            Self::Recording(window) => PlayerSessionMessage::RecordingWindow {
                generation,
                end_us: window.end_us,
                seekable: window.seekable,
            },
        }
    }
}

/// Диапазон перемотки словами протокола.
fn seekable_range(start: Duration, end: Duration) -> LiveSeekable {
    LiveSeekable::Range {
        start_us: clock_position(start),
        end_us: clock_position(end),
    }
}

/// Окно, поставленное в отправку последним.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueuedLiveWindow {
    /// Поколение, к которому окно относится.
    generation: StreamGeneration,
    /// Само окно.
    window: PageTimelineWindow,
    /// Когда поставлено (от этого момента считается предел частоты).
    queued_at: Instant,
}

/// Учёт «что и когда уже сказано странице про окно шкалы» одного сеанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct LiveWindowCadence {
    /// Последнее поставленное окно (`None` — в этом сеансе ещё ни одного).
    last_queued: Option<QueuedLiveWindow>,
    /// Последнее поколение, чьё окно (эфира или записи) уже доставлено странице (`None` —
    /// ни одного): тень сопоставляет своё время со временем плеера по любому из них.
    told_generation: Option<StreamGeneration>,
}

/// Знает ли страница время эфира поколения (получила ли его `live_window`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveWindowTold {
    /// Ещё нет: её перемотки пока в её собственном времени.
    NotYet,
    /// Да: дальше тень переводит перемотки страницы во время плеера.
    Told,
}

/// Что стало с окном, о котором сообщила медиа-часть.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveWindowNote {
    /// Поставлено в отправку.
    Queued,
    /// Такое же окно этого поколения уже поставлено: повторять незачем.
    Unchanged,
    /// Окно изменилось, но предел частоты ещё не истёк: уйдёт то окно, которое плеер
    /// увидит на первом проходе после срока.
    Throttled,
    /// Окна нет: медиа нет, край эфира ещё неизвестен или у записи нет длины.
    NoWindow,
    /// Страница ещё не подключилась: версия неизвестна, окно уйдёт после `Attached`.
    NotAttachedYet,
    /// Общая версия ниже версии сообщения (1.2 у эфира, 1.11 у записи): страница такого
    /// сообщения не знает, не шлём никогда.
    ProtocolTooOld,
}

impl PageOutbound {
    /// Окно эфира показанного поколения `generation`: ставится в отправку по правилам
    /// модуля. Звать на каждом проходе, пока поколение показано и страница в его времени.
    pub(crate) fn note_live_window(
        &mut self,
        generation: StreamGeneration,
        window: LiveWindowObservation,
        now: Instant,
    ) -> LiveWindowNote {
        let Some(window) = PageTimelineWindow::from_observation(window) else {
            return LiveWindowNote::NoWindow;
        };
        match self.page_protocol {
            PageProtocol::NotAttachedYet => return LiveWindowNote::NotAttachedYet,
            PageProtocol::Agreed(version) if !version.includes(window.introduced_in()) => {
                return LiveWindowNote::ProtocolTooOld;
            }
            PageProtocol::Agreed(_) => {}
        }
        if let Some(last) = self.live_window.last_queued
            && last.generation == generation
        {
            if last.window == window {
                return LiveWindowNote::Unchanged;
            }
            if now.saturating_duration_since(last.queued_at) < LIVE_WINDOW_INTERVAL {
                return LiveWindowNote::Throttled;
            }
        }
        // Первое окно поколения (или сеанса) — сразу: у новой шкалы окна у страницы нет.
        self.live_window.last_queued = Some(QueuedLiveWindow {
            generation,
            window,
            queued_at: now,
        });
        self.queue_event_replacing_same_kind(window.message(generation));
        LiveWindowNote::Queued
    }

    /// Получила ли страница хоть одно окно эфира поколения `generation`.
    pub(crate) fn live_window_told(&self, generation: StreamGeneration) -> LiveWindowTold {
        if self.live_window.told_generation == Some(generation) {
            LiveWindowTold::Told
        } else {
            LiveWindowTold::NotYet
        }
    }

    /// Страница (снова) подключилась: всё, что ушло прежнему соединению, могло
    /// потеряться в обрыве (окно «доставлено» в очередь сервера, а не странице). Учёт
    /// забывается: текущее окно уйдёт на ближайшем проходе сразу, даже если край стоит,
    /// а до его доставки перемотки страницы эфира считаются перемотками в её времени.
    pub(crate) fn forget_live_window_on_attach(&mut self) {
        self.live_window = LiveWindowCadence::default();
    }

    /// Сообщение ушло странице: если это окно шкалы (эфира или записи) — его поколение
    /// теперь «сказано».
    pub(super) fn note_sent_live_window(&mut self, sent: &PlayerSessionMessage) {
        if let PlayerSessionMessage::LiveWindow { generation, .. }
        | PlayerSessionMessage::RecordingWindow { generation, .. } = sent
        {
            self.live_window.told_generation = Some(*generation);
        }
    }
}

#[cfg(test)]
#[path = "live_window_tests.rs"]
mod tests;
