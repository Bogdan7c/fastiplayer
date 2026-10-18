//! Небольшой собственный читатель текстовых дорожек в фрагментированном ISOBMFF (fMP4).
//!
//! Нужен только для субтитров DASH: `stpp` (TTML в сэмплах) и `wvtt` (WebVTT в сэмплах),
//! ISO/IEC 14496-30. Видео/аудио по-прежнему читает Symphonia; сюда ничего из A/V не
//! попадает. Читатель:
//! - достаёт из `moov` таймскейл, тип записи `stsd` (`stpp`/`wvtt`) и умолчания `trex`;
//! - из `moof`/`traf` (`tfhd`, `tfdt`, `trun`) + `mdat` собирает сэмплы: время декодирования
//!   и длительность в единицах таймскейла дорожки и байты сэмпла;
//! - сэмплы `wvtt` превращает в текст WebVTT (`vttc`: `iden`, `sttg`, `payl`; `vtte` пуст).
//!
//! Все границы проверяются, паник нет: любое нарушение - типизированный [`TextFmp4Error`].
//! Неизвестные боксы пропускаются. Не поддерживается нефрагментированный MP4 (таблицы
//! `stbl` без `moof`), потому что DASH-субтитры всегда фрагментированы.
//!
//! Тайминг: время сэмпла - это время на шкале media timeline дорожки (`tfdt` + накопленные
//! длительности). Перевод в шкалу плеера (минус PTO, плюс начало Period) делает владелец
//! транспорта, этот модуль о Period ничего не знает.

mod boxes;
mod fragment;
mod init;
#[cfg(test)]
mod tests;
mod wvtt;

pub(crate) use fragment::parse_text_mp4;
pub(crate) use init::{TextSampleFormat, TextTrackInit};
pub(crate) use wvtt::webvtt_document_from_samples;

/// Предел числа сэмплов в одном сегменте: защита от враждебного `trun` (структурная граница).
pub(crate) const MAX_SAMPLES_PER_SEGMENT: usize = 100_000;

/// Один сэмпл текстовой дорожки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextSample {
    /// Время декодирования в единицах таймскейла дорожки (media timeline, без правки PTO).
    pub decode_time: u64,
    /// Длительность в единицах таймскейла дорожки.
    pub duration: u64,
    /// Байты сэмпла: для `stpp` документ TTML, для `wvtt` последовательность боксов.
    pub data: Vec<u8>,
}

/// Почему сегмент текстовой дорожки не удалось прочитать. Без адресов и данных.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum TextFmp4Error {
    /// Бокс или поле обрезаны: данных меньше, чем заявлено.
    #[error("MP4 data is truncated")]
    Truncated,
    /// Заголовок бокса некорректен (размер меньше заголовка и т. п.).
    #[error("MP4 box header is invalid")]
    InvalidBox,
    /// В `moov` нет дорожки `stpp`/`wvtt`.
    #[error("MP4 init segment has no stpp/wvtt track")]
    NoTextTrack,
    /// Фрагмент встретился раньше инициализации (`moov`) и инициализации нет.
    #[error("MP4 media segment arrived without an init segment")]
    MissingInit,
    /// Нулевой таймскейл дорожки.
    #[error("MP4 track timescale is zero")]
    ZeroTimescale,
    /// Длительность сэмпла нигде не задана (ни в `trun`, ни в `tfhd`, ни в `trex`).
    #[error("MP4 sample duration is not defined")]
    MissingSampleDuration,
    /// `trun` без `data_offset`: раскладку данных нельзя определить однозначно.
    #[error("MP4 track run has no data offset")]
    MissingDataOffset,
    /// Сэмпл указывает за пределы буфера.
    #[error("MP4 sample points outside the segment")]
    SampleOutOfBounds,
    /// Размер сэмпла нигде не задан (ни в `trun`, ни в `tfhd`, ни в `trex`).
    #[error("MP4 sample size is not defined")]
    MissingSampleSize,
    /// Диапазоны сэмплов пересекаются или идут не по возрастанию (враждебный `trun`).
    #[error("MP4 sample byte ranges overlap or are not monotonic")]
    OverlappingSamples,
    /// Сумма размеров сэмплов превышает длину самого сегмента.
    #[error("MP4 samples exceed the segment length")]
    SamplesExceedSegment,
    /// Сэмплов больше структурного предела.
    #[error("MP4 segment has too many samples")]
    TooManySamples,
    /// Арифметическое переполнение во времени или смещениях.
    #[error("MP4 time or offset arithmetic overflowed")]
    Overflow,
    /// Сегмент нефрагментирован (нет `moof`) либо раскладка не поддержана.
    #[error("MP4 layout is not fragmented")]
    UnsupportedLayout,
}
