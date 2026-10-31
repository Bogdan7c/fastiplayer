//! «Сядет ли перемотка к T с полки?» — вопрос без перемотки (живой скраб окна моста).
//!
//! Окно браузерного режима показывает кадры на лету, пока зритель тянет таймлайн, —
//! но только там, где перемотка плеера не попросит страницу перемотаться (каждая
//! промежуточная цель вне полки стала бы `seek` странице). Здесь — ответ на этот
//! вопрос по ТЕМ ЖЕ правилам, что у настоящей перемотки ([`StreamBuffer::seek`],
//! описание модуля `seek`): все полосы с временем держат цель на полке (правила 1–4)
//! или вступают позже по другим полосам (правило 5). Правило одно — функция
//! `seek::can_land_from_shelf`, поэтому вопрос и перемотка не могут разойтись.
//!
//! Ничего не меняет: ни полок, ни курсоров, ни журнала разговора со страницей, ни
//! «недоверия» к позициям после перемотки. Замок буфера берётся коротко, как у
//! монитора.

use std::time::Duration;

use super::seek::can_land_from_shelf;
use super::{LaneTimeline, StreamBuffer};
use crate::framing::MediaTime;
use crate::monitor::ShelfSeekOutlook;

impl StreamBuffer {
    /// Что сделала бы перемотка к `target` сейчас (см. описание модуля).
    pub(crate) fn seek_outlook(&self, target: Duration) -> ShelfSeekOutlook {
        let target = MediaTime::from_player_position(target);
        let state = self.lock();
        // Полос ещё нет — показывать нечего; полоса без меток времени (MPEG-TS) —
        // перемотка по времени невозможна вовсе (`StreamSeekError::UntimedLane`).
        let seekable_by_time = !state.lanes.is_empty()
            && state
                .lanes
                .values()
                .all(|lane| lane.timeline == LaneTimeline::Timed);
        if !seekable_by_time {
            return ShelfSeekOutlook::Unavailable;
        }
        let lands_from_shelf = state
            .lanes
            .keys()
            .all(|lane| can_land_from_shelf(&state, *lane, target, &self.rules));
        if lands_from_shelf {
            ShelfSeekOutlook::FromShelf
        } else {
            ShelfSeekOutlook::AsksPage
        }
    }
}
