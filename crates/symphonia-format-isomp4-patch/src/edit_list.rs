//! Fastiplayer patch: отображение media-времени в presentation-время по списку правок `elst`.
//!
//! ISO/IEC 14496-12 §8.6.6 (EditListBox) задаёт, какая часть media трека и в какой момент
//! презентации показывается. Мы поддерживаем самую частую форму, которую пишет ffmpeg:
//!
//! * ноль или более **ведущих пустых правок** (`media_time == -1`) — «dwell», время, пока трек
//!   ещё ничего не показывает (аудио, стартующее на 0.2257 с позже видео);
//! * затем **одна обычная правка** с `media_time == M >= 0` и `media_rate == 1` — media-время `M`
//!   (после composition offset) соответствует началу презентации (видео с B-frame: `M` = задержка
//!   первого CTS; AAC priming: `M` = 1024).
//!
//! Presentation-время sample = `media_pts - M + dwell`, все величины в media timescale трека
//! (`mdhd`); `segment_duration` пустых правок живёт в movie timescale (`mvhd`) и пересчитывается.
//!
//! Всё остальное (несколько обычных правок, `rate != 1`, отрицательный `media_time` кроме `-1`,
//! правки в середине) — known limitation: список игнорируется целиком, трек играет по своему
//! media timeline, как раньше. Длительность `segment_duration` обычной правки не используется
//! (хвост не обрезается).
//!
//! Результат может быть отрицательным (sample с media-временем меньше `M`, например AAC priming):
//! Symphonia `Timestamp` знаковый, а нейтральная demux-граница сама нормализует отрицательный PTS
//! в ноль и сохраняет сырые знаковые единицы.

use std::num::NonZero;

use log::warn;

use crate::atoms::elst::{ElstAtom, ElstEntry};

/// Значение `media_time`, означающее пустую правку.
const EMPTY_EDIT_MEDIA_TIME: i64 = -1;

/// Почему список правок не поддерживается и игнорируется целиком.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedEditList {
    /// После ведущих пустых правок идёт не «ноль или одна простая правка со скоростью 1».
    UnsupportedShape,
    /// Сумма или пересчёт не помещается в `u64`/`i64`-таймстемп.
    ArithmeticOverflow,
}

/// Сдвиг presentation-времени трека относительно его media-времени.
///
/// Хранит величины вместе с timescale (`mdhd` трека), чтобы нельзя было сложить тики разных шкал.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationShift {
    /// Длительность ведущих пустых правок (dwell) в тиках `timescale`.
    dwell_ticks: u64,
    /// Media-время `M`, с которого начинается презентация, в тиках `timescale`.
    media_start_ticks: u64,
    /// Media timescale трека (`mdhd`).
    timescale: NonZero<u32>,
}

impl PresentationShift {
    /// Нет сдвига: presentation-время совпадает с media-временем.
    pub const fn none(timescale: NonZero<u32>) -> Self {
        Self {
            dwell_ticks: 0,
            media_start_ticks: 0,
            timescale,
        }
    }

    /// Разбирает `elst` трека; неподдерживаемые формы логируются и дают «нет сдвига».
    ///
    /// Отказ не фатален: файл с экзотическим списком правок играет как раньше.
    pub fn from_track_edit_list(
        edit_list: Option<&ElstAtom>,
        movie_timescale: NonZero<u32>,
        media_timescale: NonZero<u32>,
    ) -> Self {
        let Some(edit_list) = edit_list else {
            return Self::none(media_timescale);
        };
        match Self::try_from_entries(&edit_list.entries, movie_timescale, media_timescale) {
            Ok(shift) => shift,
            Err(reason) => {
                warn!("isomp4: edit list ignored ({reason:?}), track keeps media timeline");
                Self::none(media_timescale)
            }
        }
    }

    /// Строгий разбор: возвращает причину, почему форма не поддержана.
    pub fn try_from_entries(
        entries: &[ElstEntry],
        movie_timescale: NonZero<u32>,
        media_timescale: NonZero<u32>,
    ) -> Result<Self, UnsupportedEditList> {
        let leading_empty_count = entries
            .iter()
            .take_while(|entry| entry.media_time == EMPTY_EDIT_MEDIA_TIME)
            .count();
        let (leading_empty, remainder) = entries.split_at(leading_empty_count);

        // Остаток: ничего либо ровно одна обычная правка нормальной скорости.
        let media_start_ticks = match remainder {
            [] => 0,
            [only] if only.media_rate_int == 1 && only.media_rate_frac == 0 => {
                u64::try_from(only.media_time).map_err(|_| UnsupportedEditList::UnsupportedShape)?
            }
            _ => return Err(UnsupportedEditList::UnsupportedShape),
        };

        // Dwell: сумма длительностей пустых правок (movie timescale) -> media timescale.
        let movie_ticks = leading_empty
            .iter()
            .try_fold(0u64, |sum, entry| sum.checked_add(entry.segment_duration))
            .ok_or(UnsupportedEditList::ArithmeticOverflow)?;
        let dwell_ticks = rescale_ticks_rounded(movie_ticks, movie_timescale, media_timescale)
            .ok_or(UnsupportedEditList::ArithmeticOverflow)?;

        // Оба слагаемых должны помещаться в знаковый i64-таймстемп Symphonia.
        if i64::try_from(dwell_ticks).is_err() || i64::try_from(media_start_ticks).is_err() {
            return Err(UnsupportedEditList::ArithmeticOverflow);
        }
        Ok(Self {
            dwell_ticks,
            media_start_ticks,
            timescale: media_timescale,
        })
    }

    /// Dwell в тиках media timescale трека (используется тестами).
    #[cfg(test)]
    pub const fn dwell_ticks(&self) -> u64 {
        self.dwell_ticks
    }

    /// Media-время начала презентации `M` в тиках media timescale трека (используется тестами).
    #[cfg(test)]
    pub const fn media_start_ticks(&self) -> u64 {
        self.media_start_ticks
    }

    /// Нет ли никакого сдвига.
    pub const fn is_identity(&self) -> bool {
        self.dwell_ticks == 0 && self.media_start_ticks == 0
    }

    /// Media-время sample -> presentation-время (`media - M + dwell`), знаковое.
    ///
    /// `None` только при невозможном переполнении `i64`.
    pub fn presentation_ticks(&self, media_ticks: u64) -> Option<i64> {
        let shifted = i128::from(media_ticks) + i128::from(self.dwell_ticks)
            - i128::from(self.media_start_ticks);
        i64::try_from(shifted).ok()
    }

    /// Presentation-время запроса seek -> media-время для поиска sample (`pres + M - dwell`).
    ///
    /// Цель внутри dwell (до первого sample) насыщается в 0: ближайший sample — первый.
    pub fn media_ticks_for_presentation(&self, presentation_ticks: u64) -> u64 {
        let media = i128::from(presentation_ticks) + i128::from(self.media_start_ticks)
            - i128::from(self.dwell_ticks);
        u64::try_from(media.max(0)).unwrap_or(u64::MAX)
    }

    /// Чистый сдвиг `dwell - M`, пересчитанный в другой timescale (например, `sidx`).
    pub fn net_shift_in(&self, target: NonZero<u32>) -> Option<i64> {
        let dwell = rescale_ticks_rounded(self.dwell_ticks, self.timescale, target)?;
        let start = rescale_ticks_rounded(self.media_start_ticks, self.timescale, target)?;
        i64::try_from(i128::from(dwell) - i128::from(start)).ok()
    }

    /// Media-длительность (в `target` timescale) -> длительность до конца презентации.
    ///
    /// `dwell + max(media_duration - M, 0)`; `None` при переполнении.
    pub fn presentation_duration_ticks(
        &self,
        media_duration_ticks: u64,
        target: NonZero<u32>,
    ) -> Option<u64> {
        let dwell = rescale_ticks_rounded(self.dwell_ticks, self.timescale, target)?;
        let start = rescale_ticks_rounded(self.media_start_ticks, self.timescale, target)?;
        media_duration_ticks
            .saturating_sub(start)
            .checked_add(dwell)
    }
}

/// Пересчёт тиков между шкалами: `round_half_up(ticks * to / from)` в 128-битной арифметике.
///
/// `None`, если результат не помещается в `u64`.
pub fn rescale_ticks_rounded(ticks: u64, from: NonZero<u32>, to: NonZero<u32>) -> Option<u64> {
    let from = u128::from(from.get());
    let scaled = u128::from(ticks).checked_mul(u128::from(to.get()))?;
    // Округляем половину вверх: прибавляем from/2 до целочисленного деления.
    let rounded = scaled.checked_add(from / 2)? / from;
    u64::try_from(rounded).ok()
}

#[cfg(test)]
mod reader_tests;
#[cfg(test)]
mod test_mp4;
#[cfg(test)]
mod tests;
