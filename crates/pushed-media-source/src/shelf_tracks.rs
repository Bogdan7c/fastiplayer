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
//!
//! Замена судит ещё и по времени ДЕКОДИРОВАНИЯ (сессия 16b2 моста, ролики TikTok):
//! отрезок показа сегмента — лишь оболочка времён его кадров, и у видео с B-кадрами
//! оболочки соседей накладываются, когда страница режет фрагменты посреди группы кадров.
//! Полка принимала такого соседа за повтор и выбрасывала предыдущий фрагмент — с
//! единственным ключевым кадром или из середины ролика; перемотка (первая же — при
//! передаче вкладки) начинала подачу уже без него. Правило — в [`ShelfRules::replaces`].

use super::{FeedCursor, ShelfEntry, ShelfRules, certain_extent_of, earlier};
use crate::feed::EpisodeKey;
use crate::framing::{DecodeTiming, MediaTimeSpan, TimedSpan, TrackSpans};

/// Новый сегмент, каким его видит правило замены ([`ShelfRules::replaces`]): всё, по чему
/// полка решает, повторяет ли он уже лежащее.
#[derive(Debug, Clone, Copy)]
pub(crate) struct IncomingSegment<'segment> {
    /// Время показа после смещения полосы и точность его конца.
    pub(crate) timing: TimedSpan,
    /// Точно известное время показа каждой дорожки (пусто — по дорожкам неизвестно).
    pub(crate) tracks: &'segment TrackSpans,
    /// Время декодирования (у контейнера без него — то же время показа).
    pub(crate) decode: &'segment DecodeTiming,
    /// С каким init, контейнером и смещением его играть.
    pub(crate) key: &'segment EpisodeKey,
}

/// Время сегмента на одной оси (показа или декодирования): точно известный отрезок
/// целиком и по дорожкам.
#[derive(Debug, Clone, Copy)]
struct TimeOnAxis<'segment> {
    /// Точно известный отрезок всех дорожек.
    certain_extent: MediaTimeSpan,
    /// Отрезок каждой дорожки (пусто — по дорожкам неизвестно).
    tracks: &'segment TrackSpans,
}

impl ShelfRules {
    /// Заменяет ли новый сегмент `incoming` лежащий на полке `lying`.
    ///
    /// Повтор — это общее время ПОКАЗА (как «coded frame removal» в MSE). Но MSE судит
    /// по каждому кадру, а полка знает только отрезок сегмента от самого раннего показа
    /// до самого позднего. У видео с B-кадрами такие отрезки соседей накладываются без
    /// единого общего кадра: фрагмент, разрезанный посреди группы кадров, начинается с
    /// B-кадров, которые показываются РАНЬШЕ кадра P из предыдущего фрагмента (TikTok,
    /// сессия 16b2: наложение 83 мс при декодировании строго встык). Поэтому у сегментов
    /// одного кодирования наложение показа — ещё не повтор: нужны общие кадры, то есть
    /// общее время декодирования ([`Self::carries_other_frames`]).
    pub(super) fn replaces(&self, lying: &ShelfEntry, incoming: IncomingSegment<'_>) -> bool {
        let lying_presentation = TimeOnAxis {
            certain_extent: lying.certain_extent(),
            tracks: &lying.tracks,
        };
        let incoming_presentation = TimeOnAxis {
            certain_extent: certain_extent_of(incoming.timing),
            tracks: incoming.tracks,
        };
        self.share_time(lying_presentation, incoming_presentation)
            && !self.carries_other_frames(lying, incoming)
    }

    /// Несут ли сегменты РАЗНЫЕ кадры одного и того же кодирования: тот же эпизод (init,
    /// контейнер, смещение) и ни у одной общей дорожки нет общего времени декодирования.
    ///
    /// В одном кодировании каждый кадр декодируется в свой момент, так что сегменты без
    /// общего времени декодирования не могут повторять кадры друг друга — как бы ни
    /// накладывались их отрезки показа. У сегментов РАЗНЫХ эпизодов (смена качества, новый
    /// init) оси декодирования несравнимы — задержка показа у кодирований своя, — и для
    /// них действует только правило показа. Контейнер без времени декодирования (WebM,
    /// MPEG-TS) отдаёт вместо него время показа: ответ здесь всегда «нет», правило прежнее.
    fn carries_other_frames(&self, lying: &ShelfEntry, incoming: IncomingSegment<'_>) -> bool {
        if !lying.key.same_episode(incoming.key) {
            return false;
        }
        let lying_decode = TimeOnAxis {
            certain_extent: certain_extent_of(lying.decode.timing()),
            tracks: lying.decode.tracks(),
        };
        let incoming_decode = TimeOnAxis {
            certain_extent: certain_extent_of(incoming.decode.timing()),
            tracks: incoming.decode.tracks(),
        };
        !self.share_time(lying_decode, incoming_decode)
    }

    /// Есть ли у двух сегментов общее время на одной оси (больше допуска, см.
    /// [`Self::overlaps`]): по общим дорожкам, а если хоть у одной стороны время по
    /// дорожкам неизвестно — по отрезкам целиком.
    fn share_time(&self, first: TimeOnAxis<'_>, second: TimeOnAxis<'_>) -> bool {
        if first.tracks.is_known() && second.tracks.is_known() {
            return first
                .tracks
                .shared_with(second.tracks)
                .any(|(first_track, second_track)| self.overlaps(first_track, second_track));
        }
        self.overlaps(first.certain_extent, second.certain_extent)
    }

    /// Уводит ли сегмент `entry` время назад от поданного до курсора: тогда текущий
    /// эпизод его принять не может.
    ///
    /// Время — ДЕКОДИРОВАНИЯ (ось курсора подачи, см. описание `shelf`): B-кадр,
    /// показываемый раньше уже поданного P, но декодируемый после него, — продолжение.
    /// По дорожкам — назад, если у какой-то общей дорожки начало раньше её поданного
    /// конца больше чем на допуск. Без сведений о дорожках — начало сегмента раньше
    /// точного конца курсора больше чем на допуск (прежнее правило).
    pub(super) fn goes_back(&self, cursor: &FeedCursor, entry: &ShelfEntry) -> bool {
        let decode_tracks = entry.decode.tracks();
        if cursor.tracks().is_known() && decode_tracks.is_known() {
            return cursor
                .tracks()
                .shared_with(decode_tracks)
                .any(|(fed, next)| {
                    next.start() < earlier(fed.end(), self.overlap_tolerance_micros)
                });
        }
        entry.decode_start() < earlier(cursor.certain_end(), self.overlap_tolerance_micros)
    }
}
