//! Укорочение конца уже вставленного события.
//!
//! Нужно открытым событиям: PGS-картинка в MKV не имеет длительности, её показывают
//! «до следующего набора». Событие вставляют с очень далёким концом и укорачивают, когда
//! приходит преемник. Разрешено только укорочение: так «хвост» событий остаётся
//! append-only, а потребители (зеркала, кэши картинок) получают узкую дельту
//! «отрезок `[new_end, old_end)` перестал быть покрытым этим событием».
//!
//! Взаимодействие с дедупликацией: [`SubtitleTimeline::insert`] сравнивает события целиком,
//! включая `end`. После укорочения событие с прежним (длинным) концом уже НЕ считается
//! дубликатом. Поэтому повторное чтение тех же пакетов после seek назад обязан отсекать
//! владелец события (приложение хранит, что композиция уже вставлена), а не таймлайн.

use std::collections::VecDeque;

use super::{SubtitleEventId, SubtitleTimeline};
use crate::time::SubtitleTime;

/// Сколько последних укорочений помнит журнал. Потребитель, отставший больше чем на столько
/// изменений, получает [`ShorteningHistoryLost`] и обязан пересинхронизироваться целиком.
pub(super) const MAX_REMEMBERED_SHORTENINGS: usize = 1024;

/// Одно выполненное укорочение (запись журнала).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndShortening {
    pub id: SubtitleEventId,
    /// Конец до укорочения.
    pub previous_end: SubtitleTime,
    /// Конец после укорочения.
    pub new_end: SubtitleTime,
}

/// Журнал укорочений ограниченной длины; записи помечены `revision` после укорочения.
#[derive(Debug, Clone, Default)]
pub(super) struct ShortenLog {
    records: VecDeque<(u64, EndShortening)>,
    /// `revision` самой новой забытой записи (0 — ничего не забыто).
    forgotten_through: u64,
}

impl ShortenLog {
    fn push(&mut self, revision: u64, shortening: EndShortening) {
        if self.records.len() >= MAX_REMEMBERED_SHORTENINGS
            && let Some((forgotten_revision, _)) = self.records.pop_front()
        {
            self.forgotten_through = forgotten_revision;
        }
        self.records.push_back((revision, shortening));
    }
}

/// Журнал уже не помнит всех укорочений после запрошенной ревизии.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the shortening history no longer reaches back to revision {requested_revision}")]
pub struct ShorteningHistoryLost {
    pub requested_revision: u64,
}

/// Что произошло при укорочении.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortenOutcome {
    /// Конец перенесён раньше; `revision` вырос. Отрезок `[new_end, previous_end)` больше
    /// не покрыт этим событием.
    Shortened {
        previous_end: SubtitleTime,
        new_end: SubtitleTime,
    },
    /// Конец уже равен запрошенному; ничего не менялось (`revision` прежний).
    AlreadyEndsThere,
}

/// Почему укоротить нельзя; таймлайн при ошибке не меняется.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ShortenError {
    /// Такого события нет.
    #[error("subtitle event {0:?} is not in the timeline")]
    UnknownEvent(SubtitleEventId),
    /// Новый конец позже текущего: удлинять нельзя (это нарушило бы append-only дельты).
    #[error("new end {new_end:?} is later than the current end {current_end:?}")]
    WouldExtend {
        current_end: SubtitleTime,
        new_end: SubtitleTime,
    },
    /// Новый конец не позже начала: событие стало бы пустым.
    #[error("new end {new_end:?} is not after the event start {start:?}")]
    NotAfterStart {
        start: SubtitleTime,
        new_end: SubtitleTime,
    },
}

impl SubtitleTimeline {
    /// Переносит конец события `id` на `new_end` (только раньше текущего, строго после
    /// начала). Id событий остаются прежними; индексы поиска обновляются за O(log n).
    pub fn shorten_event_end(
        &mut self,
        id: SubtitleEventId,
        new_end: SubtitleTime,
    ) -> Result<ShortenOutcome, ShortenError> {
        let event = self.event(id).ok_or(ShortenError::UnknownEvent(id))?;
        let (start, current_end) = (event.start(), event.end());
        if new_end == current_end {
            return Ok(ShortenOutcome::AlreadyEndsThere);
        }
        if new_end > current_end {
            return Err(ShortenError::WouldExtend {
                current_end,
                new_end,
            });
        }
        if new_end <= start {
            return Err(ShortenError::NotAfterStart { start, new_end });
        }
        // Позиция события в порядке (start, id): id найден выше, поэтому поиск не промахнётся.
        let position = self
            .order_by_start
            .partition_point(|&other| (self.start_of(other), other) < (start, id));
        if !self.max_end_tree.set_end(position, new_end) {
            return Err(ShortenError::UnknownEvent(id));
        }
        if let Some(event) = self.events.get_mut(id.index()) {
            event.set_shortened_end(new_end);
        }
        self.bump_revision();
        self.shorten_log.push(
            self.revision,
            EndShortening {
                id,
                previous_end: current_end,
                new_end,
            },
        );
        Ok(ShortenOutcome::Shortened {
            previous_end: current_end,
            new_end,
        })
    }

    /// Укорочения, выполненные ПОСЛЕ ревизии `revision`, в порядке выполнения.
    ///
    /// Зеркало, которое в последний раз видело таймлайн на ревизии `revision`, получает ровно
    /// то, чего ему не хватает. Журнал ограничен: если нужные записи уже вытеснены, возвращается
    /// [`ShorteningHistoryLost`] — зеркало обязано пересобраться целиком. В записях могут быть
    /// события, добавленные после `revision`: зеркалу они приходят сразу с актуальным концом.
    pub fn shortenings_since(
        &self,
        revision: u64,
    ) -> Result<impl Iterator<Item = EndShortening> + '_, ShorteningHistoryLost> {
        if revision < self.shorten_log.forgotten_through {
            return Err(ShorteningHistoryLost {
                requested_revision: revision,
            });
        }
        Ok(self
            .shorten_log
            .records
            .iter()
            .filter(move |(recorded_at, _)| *recorded_at > revision)
            .map(|(_, shortening)| *shortening))
    }
}
