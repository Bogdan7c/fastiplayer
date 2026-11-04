//! Противодавление единой очереди: «есть ли место» для потока соединения.
//!
//! Зачем (живая проверка 09b2, Twitch VOD в Firefox). Журнал страницы (решение 176,
//! до 64 МиБ) — тысячи однокадровых фрагментов, и экстеншен отдаёт его одним всплеском
//! на скорости провода. Очередь ограничена ([`super::QueueLimits`]), а единственный тормоз
//! для провода был «полон» источника — он про полку, не про очередь. Любой разбор
//! медленнее провода (старое железо, отладочная сборка) переполнял очередь, и сеанс
//! закрывался кодом 4003, хотя страница ничего не нарушала.
//!
//! Теперь у очереди две отметки ниже потолков: над отметкой «придержать» поток
//! соединения сокет не читает ([`browser_bridge_session::SessionByteSink::wait_for_room`]),
//! байты страницы остаются в буферах TCP, браузер видит растущий `bufferedAmount` и
//! придерживает отправку сам. Поток приёма забирает очередь целиком
//! ([`OrderedSessionQueue::wait_batch`]) — после забора место есть снова.
//!
//! Решение 98 остаётся в силе: очередь, которую никто не забирает (разбор встал),
//! держится не дольше [`super::QueueLimits::hold_without_progress_limit`] — затем это
//! переполнение, экземпляр закрывается кодом 4003 тем же путём, что и при потолке.
//! Потолки [`super::QueueLimits::max_items`] / [`super::QueueLimits::max_payload_bytes`] остаются
//! последним рубежом (данные мимо противодавления: другой экземпляр, рамки файла).

use std::time::{Duration, Instant};

use browser_bridge_session::{IntakeRoom, SessionRef};

use super::{OrderedSessionQueue, QueueState};

/// Очередь над отметкой «придержать», и поток приёма её ещё не забирал.
#[derive(Debug, Clone, Copy)]
pub(super) struct Hold {
    /// Когда поток соединения впервые упёрся в отметку.
    since: Instant,
}

impl OrderedSessionQueue {
    /// Ответ потоку соединения: читать ли следующую рамку (описание модуля).
    ///
    /// Ждёт забора очереди не дольше `patience`; экземпляр, чьи данные уже
    /// выбрасываются (переполнен), не придерживается — ему нужно дочитать до закрытия.
    pub(crate) fn wait_for_room(&self, session: &SessionRef, patience: Duration) -> IntakeRoom {
        let patience_ends = Instant::now() + patience;
        let verdict = {
            let mut state = self.lock_state();
            loop {
                if state.shut_down || state.overflowed.contains(&session.instance) {
                    break RoomVerdict::Available;
                }
                if !self.above_hold_mark(&state) {
                    state.hold = None;
                    break RoomVerdict::Available;
                }
                let now = Instant::now();
                let hold = *state.hold.get_or_insert(Hold { since: now });
                let progress_deadline = hold.since + self.limits.hold_without_progress_limit;
                if now >= progress_deadline {
                    // Разбор встал (решение 98): это переполнение, экземпляр закрывается.
                    state.hold = None;
                    Self::mark_overflow_without_progress(&mut state, session);
                    break RoomVerdict::OverflowedWithoutProgress;
                }
                let wait_until = patience_ends.min(progress_deadline);
                if now >= wait_until {
                    break RoomVerdict::Exhausted;
                }
                state = self
                    .drained
                    .wait_timeout(state, wait_until - now)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
            }
        };
        match verdict {
            RoomVerdict::Available => IntakeRoom::Available,
            RoomVerdict::Exhausted => IntakeRoom::Exhausted,
            RoomVerdict::OverflowedWithoutProgress => {
                // Просьба закрыть — вне замка очереди (как у переполнения потолком).
                self.request_overflow_close(session.clone());
                IntakeRoom::Available
            }
        }
    }

    /// Над отметкой «придержать» хотя бы по одной мере.
    fn above_hold_mark(&self, state: &QueueState) -> bool {
        state.items.len() >= self.limits.hold_above_items
            || state.payload_bytes >= self.limits.hold_above_payload_bytes
    }

    /// Очередь держалась без забора дольше предела: как переполнение у потолка —
    /// данные экземпляра выбрасываются и считаются; просьбу закрыть шлёт вызывающий
    /// вне замка.
    fn mark_overflow_without_progress(state: &mut QueueState, session: &SessionRef) {
        state.overflowed.insert(session.instance);
        state.counters.overflow_closes = state.counters.overflow_closes.saturating_add(1);
        state.counters.holds_without_progress =
            state.counters.holds_without_progress.saturating_add(1);
        Self::purge_overflowed_instance(state, session.instance);
    }
}

/// Итог ожидания под замком.
enum RoomVerdict {
    /// Место есть (или придерживать незачем).
    Available,
    /// Места нет, терпение вышло.
    Exhausted,
    /// Разбор встал: экземпляр помечен переполненным, просьба закрыть ещё не послана.
    OverflowedWithoutProgress,
}

#[cfg(test)]
#[path = "room_tests.rs"]
mod tests;
