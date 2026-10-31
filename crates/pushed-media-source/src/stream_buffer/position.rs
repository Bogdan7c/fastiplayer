//! Позиция плеера и состояние буфера для страницы («буферизовано», «полон»).
//!
//! **Устаревшие позиции после перемотки.** Приложение сообщает позицию со своего
//! потока, а перемотка идёт на ходе плеера: позиция, снятая ДО перемотки, может
//! прийти ПОСЛЕ неё (например, 100 с после перемотки на 10 с). Поверив ей, буфер
//! отпустил бы с полки как «сыгранное» (стирание страницей, предохранитель
//! памяти) то, что плееру сейчас нужно, и считал бы запас от чужого места. Поэтому после перемотки позиция, далёкая от
//! цели (дальше допуска совпадения перемоток), не принимается, пока не придёт
//! позиция у цели — после перемотки плеер стоит ровно на цели. Чтобы редкие
//! сообщения приложения не заморозили буфер навсегда, недоверие длится не
//! дольше окна устаревания ([`crate::PushedSourcePolicy`]): устаревшая позиция
//! приходит за миллисекунды, а не за секунду.
//!
//! **«Полон».** Странице говорят «полон», когда запас вперёд достиг окна ИЛИ
//! удерживаемые байты достигли доли жёсткого потолка: у потока с большим
//! битрейтом окно в секундах больше потолка памяти, и честная страница,
//! слушающая только секунды, упёрлась бы в потолок (ошибка потока).

use std::time::{Duration, Instant};

use super::{BufferState, StreamBuffer};
use crate::buffer_window::{BufferIntake, BufferStatus};
use crate::framing::MediaTime;

/// Недоверие к позициям после перемотки к `target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StalePositionGate {
    /// Цель перемотки.
    target: MediaTime,
    /// Когда перемотка случилась.
    armed_at: Instant,
}

impl StalePositionGate {
    /// Недоверие с момента перемотки `armed_at`.
    pub(super) const fn new(target: MediaTime, armed_at: Instant) -> Self {
        Self { target, armed_at }
    }

    /// Устаревшая ли позиция: далеко от цели, а окно недоверия ещё не прошло.
    fn rejects(
        self,
        position: MediaTime,
        tolerance: Duration,
        window: Duration,
        now: Instant,
    ) -> bool {
        let distance = position.as_micros().abs_diff(self.target.as_micros());
        let near_target = u128::from(distance) <= tolerance.as_micros();
        let window_open = now.saturating_duration_since(self.armed_at) < window;
        !near_target && window_open
    }
}

impl StreamBuffer {
    /// Позиция плеера: от неё считается «вперёд» и «сыграно» (стирание страницей,
    /// предохранитель памяти).
    pub(crate) fn note_playback_position(&self, position: Duration) {
        self.note_playback_position_at(position, Instant::now());
    }

    /// То же с явным «сейчас» (окно недоверия после перемотки — в описании модуля).
    pub(crate) fn note_playback_position_at(&self, position: Duration, now: Instant) {
        let position = MediaTime::from_player_position(position);
        let mut state = self.lock();
        if let Some(gate) = state.stale_position_gate {
            if gate.rejects(
                position,
                self.seek_match_tolerance,
                self.stale_position_window,
                now,
            ) {
                tracing::debug!(
                    position_us = position.as_micros(),
                    "Pushed media buffer ignored a position reported before the last seek"
                );
                return;
            }
            state.stale_position_gate = None;
        }
        state.reported_position = Some(position);
        self.relieve_memory_pressure(&mut state);
        // Подача, стоявшая на полном ящике, догоняет: позицию сообщает поток
        // приложения, а не плеер.
        if let Err(error) = self.catch_up_backlogged_lanes(&mut state) {
            // Ошибку запуска эпизода увидит плеер (первая ошибка потока) при
            // следующей подаче; монитор ошибок не возвращает.
            tracing::warn!(%error, "Pushed media buffer could not resume deferred feeding");
        }
        self.publish_live_timeline(&mut state);
    }

    /// Перемотка: позиции далеко от цели какое-то время не принимаются.
    pub(super) fn arm_stale_position_gate(state: &mut BufferState, target: MediaTime) {
        state.stale_position_gate = Some(StalePositionGate::new(target, Instant::now()));
    }

    /// Первый момент, с которого данные есть на полке у КАЖДОЙ полосы с метками
    /// времени: самое позднее из начал полос.
    ///
    /// `None` — у какой-то полосы с метками полка пуста или таких полос нет (TS без
    /// времени коробок: «где начало» источник не знает).
    pub(crate) fn earliest_common_start(&self) -> Option<MediaTime> {
        let mut state = self.lock();
        let mut common_start: Option<MediaTime> = None;
        for lane in state.timed_lanes_mut() {
            // Одна пустая полоса — общего начала ещё нет: плеер ждал бы её данных.
            let lane_start = lane.shelf.earliest_start()?;
            common_start = Some(common_start.map_or(lane_start, |start| start.max(lane_start)));
        }
        common_start
    }

    /// Запас вперёд и «полон» (правило — в описании модуля и в
    /// [`BufferStatus::buffered_ahead`]).
    pub(crate) fn buffer_status(&self) -> BufferStatus {
        let mut state = self.lock();
        let held_bytes_full = self.ledger.held_bytes() >= self.full_at_held_bytes;
        let rules = self.rules;
        let stream_ahead = state.position().and_then(|position| {
            state
                .timed_lanes_mut()
                .map(|lane| {
                    lane.shelf
                        .covered_run_end(position, &rules)
                        .map_or(Duration::ZERO, |end| ahead_of(position, end))
                })
                .min()
        });
        let buffered_ahead = stream_ahead.unwrap_or(Duration::ZERO);
        let ahead_full = stream_ahead.is_some_and(|ahead| ahead >= rules.ahead_limit());
        let intake = if ahead_full || held_bytes_full {
            BufferIntake::Full
        } else {
            BufferIntake::Accepting
        };
        BufferStatus {
            buffered_ahead,
            intake,
        }
    }
}

/// Сколько времени от позиции до конца куска (ноль, если конец не впереди).
fn ahead_of(position: MediaTime, end: MediaTime) -> Duration {
    let micros = end.as_micros().saturating_sub(position.as_micros());
    Duration::from_micros(u64::try_from(micros).unwrap_or(0))
}
