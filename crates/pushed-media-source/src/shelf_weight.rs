//! Вес полки полосы около позиции: байты сегментов и покрытое ими время
//! (`media_weight` буфера потока считает по этому битрейт видео, решение 277).
//!
//! **Только то качество, что играет у позиции.** Смена качества страницей (ABR YouTube,
//! ручной выбор) приходит новым init-сегментом в тот же `SourceBuffer`: у сегментов нового
//! качества другой [`EpisodeKey`]. Считаются только сегменты с тем же ключом, что у сегмента
//! под позицией (если позиция в дыре — у ближайшего позади, иначе у первого впереди). Иначе
//! окно в 10 с ещё ~10 с после смены смешивало бы старое качество с новым, и кнопка «720p»
//! показывала бы битрейт 360p (так и вышло на стенде `quality` до этого правила).
//!
//! Окно длиной `window` берётся ПЕРЕД позицией: сегменты, чей отрезок пересекает
//! `[позиция − окно, позиция]` (в их числе тот, что играет под позицией). Сегмент
//! берётся целиком — байты не режутся по краю окна; время — объединение отрезков
//! (соседние сегменты могут касаться и накладываться). Если позади ничего нет
//! (сразу после перемотки, у начала потока, за стёртым), берутся сегменты впереди,
//! `[позиция, позиция + окно]`. Стёртое страницей, но ещё лежащее на полке — тоже
//! в счёт: именно это сейчас играется. Сегмент без кадров (пустой отрезок) не
//! считается: времени он не даёт, а байты исказили бы число.

use super::{LaneShelf, MediaTime, ShelfEntry};
use crate::feed::EpisodeKey;
use crate::media_bitrate::VideoWeight;

impl LaneShelf {
    /// Вес сегментов полосы около `anchor` (правило — в описании модуля).
    pub(crate) fn weight_near(&self, anchor: MediaTime, window_micros: i64) -> VideoWeight {
        let Some(playing) = self.playing_key_near(anchor) else {
            return VideoWeight::default();
        };
        let window_start = anchor.as_micros().saturating_sub(window_micros);
        let behind = self.weigh_where(window_start, anchor.as_micros(), |entry| {
            let span = entry.span();
            entry.key.same_episode(playing)
                && span.start() <= anchor
                && span.end().as_micros() > window_start
        });
        if behind.covered_micros > 0 {
            return behind;
        }
        let window_end = anchor.as_micros().saturating_add(window_micros);
        self.weigh_where(anchor.as_micros(), window_end, |entry| {
            let span = entry.span();
            entry.key.same_episode(playing)
                && span.start().as_micros() < window_end
                && span.end() > anchor
        })
    }

    /// Ключ (init, контейнер, смещение) качества, которое играет у `anchor`: сегмента,
    /// покрывающего позицию; если позиция в дыре — ближайшего, начавшегося раньше; если
    /// позади ничего нет — первого впереди. Пустые сегменты (без кадров) не в счёт.
    fn playing_key_near(&self, anchor: MediaTime) -> Option<&EpisodeKey> {
        // Сегменты по возрастанию начала: всё до `split` начинается не позже позиции.
        let split = self
            .entries
            .partition_point(|entry| entry.span().start() <= anchor);
        let (started, ahead) = self.entries.split_at(split);
        let has_frames = |entry: &&ShelfEntry| !entry.span().is_empty();
        // Среди начавшихся — тот, что ещё идёт у позиции (длинный сегмент может начаться
        // раньше соседей), иначе самый поздний из начавшихся, иначе первый впереди.
        let covering = started
            .iter()
            .rev()
            .filter(has_frames)
            .find(|entry| entry.span().end() > anchor);
        let latest_started = || started.iter().rev().find(has_frames);
        let first_ahead = || ahead.iter().find(has_frames);
        covering
            .or_else(latest_started)
            .or_else(first_ahead)
            .map(|entry| &entry.key)
    }

    /// Суммирует сегменты, чьи начала лежат в `[earliest_reach, latest_start]` (с запасом на
    /// длину сегмента, чтобы бинарным поиском не терять длинные), и которые подходят под
    /// `counts`. Полка идёт по возрастанию начала, поэтому объединение отрезков считается
    /// одним проходом.
    fn weigh_where(
        &self,
        earliest_reach_micros: i64,
        latest_start_micros: i64,
        counts: impl Fn(&ShelfEntry) -> bool,
    ) -> VideoWeight {
        // Сегмент, достающий до `earliest_reach`, начинается не раньше, чем за самую
        // длинную протяжённость сегментов полки до него.
        let first_start = earliest_reach_micros.saturating_sub(self.shape.longest_extent_micros());
        let from = self
            .entries
            .partition_point(|entry| entry.span().start().as_micros() < first_start);
        let to = self
            .entries
            .partition_point(|entry| entry.span().start().as_micros() <= latest_start_micros);
        let mut weight = VideoWeight::default();
        // Правый край уже посчитанного объединения: новый отрезок добавляет только
        // то, что за ним.
        let mut union_end: Option<MediaTime> = None;
        for entry in self.entries.get(from..to).unwrap_or_default() {
            let span = entry.span();
            if span.is_empty() || !counts(entry) {
                continue;
            }
            weight.bytes = weight
                .bytes
                .saturating_add(u64::try_from(entry.media_len()).unwrap_or(u64::MAX));
            let new_from = union_end.map_or(span.start(), |end| end.max(span.start()));
            if span.end() > new_from {
                let added = span.end().as_micros().saturating_sub(new_from.as_micros());
                weight.covered_micros = weight
                    .covered_micros
                    .saturating_add(u64::try_from(added).unwrap_or(0));
                union_end = Some(span.end());
            }
        }
        weight
    }
}
