//! Стабильная шкала плейлиста субтитров: `start` сегмента не «ползёт» при скользящем окне live.
//!
//! Playlist-relative начало сегмента (`сумма EXTINF предыдущих`) в живом плейлисте меняется на
//! каждом перечитывании: самый старый сегмент уходит, и всё, что осталось, «сдвигается к нулю».
//! Потребитель, выучивший смещение «плейлист -> плеер» по одному снимку, получил бы устаревшее
//! смещение уже на следующем. Поэтому транспорт держит одну сквозную шкалу на rendition и
//! каждый новый снимок закрепляет на ней по перекрытию: сегмент с тем же
//! `(media_sequence, discontinuity_sequence)`, что был в прошлом снимке, сохраняет прежнее
//! начало (так же, как `HlsLiveComponentSnapshot::refreshed` делает для видео).
//!
//! Если перекрытия нет (плейлист ушёл дальше окна перечитывания), шкала продолжается от
//! конца прошлого снимка и увеличивает [`PlaylistTimelineEpoch`]: смещение, выученное
//! потребителем, после этого недействительно, и он обязан это узнать явно.

use std::collections::HashMap;
use std::time::Duration;

use super::playlist::HlsSubtitleSegment;

/// Номер «поколения» шкалы: растёт каждый раз, когда шкалу пришлось закрепить заново.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlaylistTimelineEpoch(u64);

impl PlaylistTimelineEpoch {
    /// Числовое значение (для сравнения «шкала сменилась»).
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Идентичность сегмента внутри одного rendition-а.
type SegmentIdentity = (u64, u64);

/// Шкала одного rendition-а.
#[derive(Debug, Default)]
pub(super) struct PlaylistTimeline {
    /// Стабильные начала сегментов прошлого снимка.
    retained_starts: HashMap<SegmentIdentity, Duration>,
    /// Конец прошлого снимка на стабильной шкале (на нём продолжается шкала при разрыве).
    previous_end: Duration,
    epoch: PlaylistTimelineEpoch,
    /// Был ли хоть один снимок (первый закрепляется в ноль без смены поколения).
    anchored: bool,
}

impl PlaylistTimeline {
    /// Переносит `segments` (с локальными началами от нуля) на стабильную шкалу.
    ///
    /// Возвращает поколение шкалы после закрепления.
    pub(super) fn anchor(&mut self, segments: &mut [HlsSubtitleSegment]) -> PlaylistTimelineEpoch {
        let Some(first) = segments.first() else {
            return self.epoch;
        };
        let shift = if self.anchored {
            match self.overlap_shift(segments) {
                Some(shift) => shift,
                None => {
                    // Разрыв: окно перечитывания пропущено целиком. Шкала продолжается от конца
                    // прошлого снимка, а выученное потребителем смещение объявляется устаревшим.
                    self.epoch = PlaylistTimelineEpoch(self.epoch.0.saturating_add(1));
                    Shift::Forward(self.previous_end.saturating_sub(first.start))
                }
            }
        } else {
            Shift::Forward(Duration::ZERO)
        };
        for segment in segments.iter_mut() {
            // `overlap_shift` уже проверила, что сдвиг не уводит начала за ноль; при невозможном
            // выходе за границу сегмент остаётся на своём локальном начале, а не теряется.
            segment.start = shift.apply(segment.start).unwrap_or(segment.start);
        }
        self.remember(segments);
        self.epoch
    }

    /// Сдвиг по первому сегменту, пережившему перечитывание; `None` - перекрытия нет.
    fn overlap_shift(&self, segments: &[HlsSubtitleSegment]) -> Option<Shift> {
        let (retained, previous_start) = segments.iter().find_map(|segment| {
            let identity = (segment.media_sequence, segment.discontinuity_sequence);
            self.retained_starts
                .get(&identity)
                .map(|stable_start| (segment, *stable_start))
        })?;
        let shift = if previous_start >= retained.start {
            Shift::Forward(previous_start - retained.start)
        } else {
            Shift::Backward(retained.start - previous_start)
        };
        // Сдвиг назад не должен загонять начала в минус: такое окно - не продолжение прошлого.
        let first_start = segments.first()?.start;
        shift.apply(first_start)?;
        Some(shift)
    }

    fn remember(&mut self, segments: &[HlsSubtitleSegment]) {
        self.retained_starts = segments
            .iter()
            .map(|segment| {
                (
                    (segment.media_sequence, segment.discontinuity_sequence),
                    segment.start,
                )
            })
            .collect();
        self.previous_end = segments.last().map_or(Duration::ZERO, |last| {
            last.start.saturating_add(last.duration)
        });
        self.anchored = true;
    }
}

/// Знаковый сдвиг длительности.
#[derive(Clone, Copy, Debug)]
enum Shift {
    Forward(Duration),
    Backward(Duration),
}

impl Shift {
    /// Применяет сдвиг; `None` при выходе за границы `Duration`.
    fn apply(self, start: Duration) -> Option<Duration> {
        match self {
            Self::Forward(amount) => Some(start.saturating_add(amount)),
            Self::Backward(amount) => start.checked_sub(amount),
        }
    }
}

#[cfg(test)]
mod tests;
