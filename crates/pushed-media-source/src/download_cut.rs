//! Обрыв скачивания: вход потока кончился, потому что ушла страница (решение 133).
//!
//! Честный конец (`endOfStream` страницы) значит «больше данных нет, медиа
//! кончилось» — плеер доигрывает всё полученное, как файл. Обрыв значит другое:
//! страница ушла посреди скачивания, и дорожки скачаны на разную глубину (YouTube
//! качает звук дальше картинки: видео до 0:48, звук до 1:00). Если доиграть всё,
//! картинка встанет на 0:48, а звук и ползунок пойдут ещё 12 с.
//!
//! Поэтому при обрыве источник ставит **отсечку** — конец того, что можно сыграть
//! всеми дорожками вместе: самый ранний из концов непрерывных кусков полок,
//! покрывающих позицию плеера (модуль `stream_buffer::playable_end`). Пакеты с
//! временем от отсечки и дальше плееру не уходят ни в одной полосе, полоса на
//! отсечке кончается, и картинка со звуком встают вместе. Перемотка во время
//! досмотра считает отсечку заново от новой позиции. Плееру поток отвечает
//! [`media_core::DemuxDataEnd::CutShort`] с отсечкой: звук, который плеер успел
//! прочитать впрок до обрыва, он сам не играет дальше отсечки, а позиция на `Ended` —
//! отсечка, а не длительность фильма.
//!
//! Полосы без меток времени (MPEG-TS) отсечки не получают: где кончаются их данные
//! во времени, источник не знает, — они доигрываются как при честном конце.

use std::time::Duration;

use media_core::DemuxDataEnd;

use crate::framing::MediaTime;

/// Конец того, что можно сыграть всеми дорожками вместе (шкала плеера).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayableEnd {
    /// Всё доступное кончается здесь: у каждой полосы с метками времени данные
    /// непрерывно идут от позиции плеера как минимум до этого момента.
    At(Duration),
    /// Время конца неизвестно: есть полоса без меток времени (MPEG-TS) или
    /// позиции ещё нет (ничего не подано). Отсечки нет — полосы доигрывают всё.
    Unknown,
}

impl PlayableEnd {
    /// Время отсечки (`None` — неизвестно).
    pub(crate) const fn time(self) -> Option<Duration> {
        match self {
            Self::At(end) => Some(end),
            Self::Unknown => None,
        }
    }

    /// Конец по времени полки (время до нуля — ноль: шкала плеера неотрицательна).
    pub(crate) fn at_shelf_time(end: MediaTime) -> Self {
        Self::At(end.to_player_time().as_duration())
    }
}

/// Итог обрыва скачивания ([`crate::PushedStreamWriter::end_download_cut_short`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadCutOutcome {
    /// Вход оборван: плеер доиграет полученное до отсечки и узнает, что конец
    /// данных обрезан.
    CutShort(DownloadCutReport),
    /// Вход уже был закончен честно (`end_of_stream` страницы, и байтов после него не
    /// было): скачанное — всё медиа, обрывать нечего, плеер доиграет его целиком.
    AlreadyComplete,
}

/// Подробности обрыва.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadCutReport {
    /// Отсечка: дальше неё плеер не получит пакетов.
    pub playable_end: PlayableEnd,
    /// Байты недособранных сегментов, выброшенные при обрыве.
    pub dropped_incomplete_bytes: usize,
}

/// Оборван ли вход потока — общее состояние писателя и стороны плеера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DownloadCut {
    /// Вход открыт или кончился честно: отсечки нет, конец данных настоящий.
    #[default]
    NotCut,
    /// Вход оборван уходом страницы: пакеты от отсечки не выдаются.
    CutShort(PlayableEnd),
}

impl DownloadCut {
    /// Что сказать плееру о конце данных.
    pub(crate) const fn data_end(self) -> DemuxDataEnd {
        match self {
            Self::NotCut => DemuxDataEnd::Complete,
            Self::CutShort(playable_end) => DemuxDataEnd::CutShort {
                playable_end: playable_end.time(),
            },
        }
    }

    /// Решение полосы о пакете со временем показа `pts`.
    pub(crate) fn admit_packet(self, pts: Duration) -> CutAdmission {
        match self {
            Self::CutShort(PlayableEnd::At(end)) if pts >= end => CutAdmission::EndLaneAtCut,
            Self::NotCut | Self::CutShort(_) => CutAdmission::Deliver,
        }
    }
}

/// Решение полосы о пакете при обрыве.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CutAdmission {
    /// Пакет раньше отсечки (или отсечки нет): выдать плееру.
    Deliver,
    /// Пакет на отсечке или за ней: полоса здесь кончается (до перемотки).
    EndLaneAtCut,
}

/// Дошла ли полоса до отсечки (состояние демуксера полосы).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LaneCutProgress {
    /// Полоса выдаёт пакеты как обычно.
    #[default]
    Delivering,
    /// Полоса встретила пакет за отсечкой и отвечает концом потока, пока перемотка
    /// не поставит её на новое место.
    EndedAtCut,
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUT: Duration = Duration::from_millis(1_200);

    /// Отсечка пропускает только время строго раньше себя; без обрыва и без
    /// известного конца пропускается всё.
    #[test]
    fn packets_from_the_cut_on_end_the_lane() {
        let cut = DownloadCut::CutShort(PlayableEnd::At(CUT));
        assert_eq!(
            cut.admit_packet(CUT - Duration::from_millis(1)),
            CutAdmission::Deliver
        );
        assert_eq!(cut.admit_packet(CUT), CutAdmission::EndLaneAtCut);
        assert_eq!(
            cut.admit_packet(CUT + Duration::from_secs(10)),
            CutAdmission::EndLaneAtCut
        );
        let unknown = DownloadCut::CutShort(PlayableEnd::Unknown);
        assert_eq!(
            unknown.admit_packet(Duration::from_secs(3_600)),
            CutAdmission::Deliver
        );
        assert_eq!(
            DownloadCut::NotCut.admit_packet(Duration::from_secs(3_600)),
            CutAdmission::Deliver
        );
    }

    /// Плееру «обрезано» говорит любой обрыв — даже без известной отсечки (TS):
    /// данные всё равно кончились раньше медиа; известная отсечка уходит плееру.
    #[test]
    fn any_cut_reports_cut_short_data_to_the_player() {
        assert_eq!(DownloadCut::NotCut.data_end(), DemuxDataEnd::Complete);
        assert_eq!(
            DownloadCut::CutShort(PlayableEnd::At(CUT)).data_end(),
            DemuxDataEnd::CutShort {
                playable_end: Some(CUT)
            }
        );
        assert_eq!(
            DownloadCut::CutShort(PlayableEnd::Unknown).data_end(),
            DemuxDataEnd::CutShort { playable_end: None }
        );
    }

    /// Время полки до нуля — отсечка в нуле, а не переполнение.
    #[test]
    fn negative_shelf_time_becomes_a_cut_at_zero() {
        assert_eq!(
            PlayableEnd::at_shelf_time(MediaTime::from_micros(-5_000)),
            PlayableEnd::At(Duration::ZERO)
        );
        assert_eq!(
            PlayableEnd::at_shelf_time(MediaTime::from_micros(1_200_000)),
            PlayableEnd::At(CUT)
        );
    }
}

#[cfg(test)]
#[path = "download_cut_tests.rs"]
mod stream_tests;
