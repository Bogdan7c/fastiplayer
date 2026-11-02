//! Правила полки по дорожкам: замена и продолжение подачи «как буферы дорожек MSE».
//!
//! Один SourceBuffer страницы может нести и видео, и звук. Браузер держит буфер
//! каждой дорожки отдельно и удаляет пересечения тоже по каждой дорожке. У соседних
//! фрагментов такой полосы границы дорожек расходятся: звук фрагмента кончается на
//! 5–21 мс позже начала видео следующего (кадр AAC — 21 мс). Объединённые отрезки
//! соседей поэтому «накрывали» друг друга больше допуска, и полка (сессия 09a моста):
//! - выбрасывала предыдущий фрагмент как «повтор» — перемотка назад просила страницу
//!   (а MSE-страница то, что у неё лежит, не присылает), а неподанный ещё фрагмент
//!   пропадал вовсе: плеер вставал в Buffering у дыры, которой у страницы нет;
//! - начинала новый эпизод (свежий демуксер) на каждом таком стыке.
//!
//! Здесь оба решения принимаются по общим дорожкам: у каждой — её собственный
//! точно известный отрезок ([`crate::framing::TrackSpans`]). Если хоть у одной
//! стороны время по дорожкам неизвестно (сегмент без кадров), правило — прежнее, по
//! объединённому отрезку. Дорожки, которых нет у другой стороны, друг другу не
//! мешают: фрагмент «только звук» не заменяет фрагмент «только видео» того же
//! времени и не уводит время назад.

use super::{FeedCursor, ShelfEntry, ShelfRules, earlier};
use crate::framing::{MediaTimeSpan, TrackSpans};

impl ShelfRules {
    /// Заменяет ли новый сегмент (`incoming` — его точно известный отрезок,
    /// `incoming_tracks` — по дорожкам) лежащий на полке `lying`.
    pub(super) fn replaces(
        &self,
        lying: &ShelfEntry,
        incoming: MediaTimeSpan,
        incoming_tracks: &TrackSpans,
    ) -> bool {
        if lying.tracks.is_known() && incoming_tracks.is_known() {
            return lying
                .tracks
                .shared_with(incoming_tracks)
                .any(|(lying_track, incoming_track)| self.overlaps(lying_track, incoming_track));
        }
        self.overlaps(lying.certain_extent(), incoming)
    }

    /// Уводит ли сегмент `entry` время назад от поданного до курсора: тогда текущий
    /// эпизод его принять не может.
    ///
    /// По дорожкам — назад, если у какой-то общей дорожки начало раньше её поданного
    /// конца больше чем на допуск. Без сведений о дорожках — начало сегмента раньше
    /// точного конца курсора больше чем на допуск (прежнее правило).
    pub(super) fn goes_back(&self, cursor: &FeedCursor, entry: &ShelfEntry) -> bool {
        if cursor.tracks().is_known() && entry.tracks.is_known() {
            return cursor
                .tracks()
                .shared_with(&entry.tracks)
                .any(|(fed, next)| {
                    next.start() < earlier(fed.end(), self.overlap_tolerance_micros)
                });
        }
        entry.span().start() < earlier(cursor.certain_end(), self.overlap_tolerance_micros)
    }
}
