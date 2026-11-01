//! Позиция плеера и состояние буфера для страницы («буферизовано», «полон»).
//!
//! **Устаревшие позиции после перемотки.** Приложение сообщает позицию со своего
//! потока, а перемотка идёт на ходе плеера: позиция, снятая ДО перемотки, может
//! прийти ПОСЛЕ неё (например, 100 с после перемотки на 10 с). Поверив ей, буфер
//! отпустил бы с полки как «сыгранное» (стирание страницей, предохранитель
//! памяти) то, что плееру сейчас нужно, и считал бы запас от чужого места. Поэтому
//! после перемотки позиция, далёкая от цели (дальше допуска совпадения перемоток),
//! не принимается, пока плеер не сообщит позицию у цели: после своего коммита
//! перемотки плеер стоит ровно на цели, и первая такая позиция снимает недоверие.
//!
//! **Пока перемотка ждёт данных у цели, недоверие не кончается по времени.**
//! Плеер в это время ещё показывает старое место и сообщает его позицию.
//! Поверив ей, буфер считал бы запас вперёд от старого места — «есть 11 с» при
//! дыре у цели (живой показ 04b, решение 134), — а «сыгранное» для стирания
//! страницей тоже мерил бы чужим местом. Пока хоть одна полоса ждёт цель,
//! позицией остаётся цель (её ставит сама перемотка).
//!
//! **После посадки — окно от МОМЕНТА ПОСАДКИ, а не от перемотки** (пакет F 04d).
//! Данные цели пришли, но плеер ещё декодирует до цели и набирает звук и до своего
//! коммита сообщает старое место. Раньше окно считалось от самой перемотки: если
//! страница несла данные дольше окна, сразу после посадки старая позиция снова
//! принималась, и «буферизовано» считалось от старого места (стенд 04e: 6,27 с =
//! 9023,13 − 9016,86 при цели 4000 с). Теперь после посадки недоверие длится до
//! позиции у цели, но не дольше окна успокоения от момента посадки
//! ([`crate::PushedSourcePolicy`], 10 с = срок перемотки плеера на этой стадии): за
//! него плеер либо садится и сообщает цель, либо сдаётся и остаётся на старом месте —
//! тогда старая позиция снова настоящая, и буфер не замерзает навсегда.
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
    /// Когда все полосы сели (`None` — ещё не сели или посадку ещё не видели).
    landed_at: Option<Instant>,
}

impl StalePositionGate {
    /// Недоверие с момента перемотки: полосы ещё не сели.
    pub(super) const fn new(target: MediaTime) -> Self {
        Self {
            target,
            landed_at: None,
        }
    }

    /// Сверяет гейт с состоянием перемотки: пока хоть одна полоса ждёт цель, посадки
    /// нет (даже если раньше все полосы на миг считались севшими — например, посреди
    /// самой перемотки, когда часть полос ещё не переставлена); когда все сели,
    /// запоминается ПЕРВЫЙ момент нынешней посадки.
    pub(super) fn note_seek_wait(&mut self, seek_wait: SeekWait, now: Instant) {
        match seek_wait {
            SeekWait::AwaitingTarget => self.landed_at = None,
            SeekWait::Landed => {
                self.landed_at.get_or_insert(now);
            }
        }
    }

    /// Устаревшая ли позиция: далеко от цели, а перемотка ещё ждёт данных или окно
    /// успокоения после посадки ещё не прошло.
    ///
    /// Посадку, которую никто не отметил (полоса пропала, пока ждала цель), гейт
    /// отмечает сам при первом взгляде — окно тогда считается с этого момента.
    /// Все `SeekWait` здесь — нынешнее состояние полос.
    fn rejects(
        &mut self,
        position: MediaTime,
        tolerance: Duration,
        settle_window: Duration,
        seek_wait: SeekWait,
        now: Instant,
    ) -> bool {
        let distance = position.as_micros().abs_diff(self.target.as_micros());
        let near_target = u128::from(distance) <= tolerance.as_micros();
        self.note_seek_wait(seek_wait, now);
        let distrusted = match self.landed_at {
            None => true,
            Some(landed_at) => now.saturating_duration_since(landed_at) < settle_window,
        };
        !near_target && distrusted
    }
}

/// Ждёт ли перемотка данных у цели.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SeekWait {
    /// Хоть одна полоса ещё ждёт сегмент с целью: плеер стоит на старом месте.
    AwaitingTarget,
    /// Все полосы сели (или перемотки не было).
    Landed,
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
        let seek_wait = state.seek_wait();
        let rejected = state.stale_position_gate.as_mut().is_some_and(|gate| {
            gate.rejects(
                position,
                self.seek_match_tolerance,
                self.seek_settle_window,
                seek_wait,
                now,
            )
        });
        if rejected {
            tracing::debug!(
                position_us = position.as_micros(),
                "Pushed media buffer ignored a position reported before the last seek"
            );
        }
        let believed = !rejected;
        if believed {
            state.stale_position_gate = None;
            state.reported_position = Some(position);
        }
        // Предохранитель и догоняющая подача — и на отвергнутой позиции: отчёты
        // приложения — единственный частый повод пройти их без байтов страницы. После
        // перемотки старое место может держать труба старого эпизода (предохранитель в
        // самой перемотке его не трогает), а отпускает она его позже, когда поток эпизода
        // выйдет. Без этого прохода поток оставался бы «полон» всё ожидание цели, честная
        // страница молчала бы, и перемотка сдалась бы (находка ревью пакета F). Места
        // плеера отвергнутая позиция не меняет: считается от цели.
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
        state.stale_position_gate = Some(StalePositionGate::new(target));
    }

    /// Сверяет гейт с состоянием перемотки на момент `now`: если все полосы сели,
    /// отсюда считается окно успокоения (описание модуля). Зовёт перемотка (в конце,
    /// когда переставлены все полосы) и подача (полоса дождалась цели).
    pub(super) fn note_seek_landing_if_landed(state: &mut BufferState, now: Instant) {
        let seek_wait = state.seek_wait();
        if let Some(gate) = state.stale_position_gate.as_mut() {
            gate.note_seek_wait(seek_wait, now);
        }
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
