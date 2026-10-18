//! Старт шкалы контейнера для внешних файлов субтитров.
//!
//! # Зачем
//! Плеер показывает и отдаёт кадрам СЫРОЕ время контейнера: у записи MPEG-TS первый кадр
//! может лежать на 1,4 с. Файлы SRT/ASS рядом с видео (и брошенные пользователем) считают
//! время от нуля, как это делают ffmpeg и mpv, поэтому без поправки реплики опаздывают
//! ровно на этот старт.
//!
//! # Границы (решение владельца, сессия 14)
//! Поправка касается ТОЛЬКО файловых дорожек (`TrackDescription::Sidecar`) локального
//! файла. Встроенные дорожки лежат на той же шкале PES, что и видео; потоковые (HLS/DASH)
//! уже нормализованы транспортом; web-дорожки (YouTube) считают от начала ролика.
//! Старт - отдельное поле, а не часть `SubtitleSyncOffset`: пользовательский сдвиг (Z/X)
//! сохраняется в память дорожек и показывается в уведомлениях, поправка контейнера - нет.

use std::time::Duration;

use subtitle_core::SubtitleTime;

/// Точка шкалы кадров, которую файл субтитров считает своим нулём.
///
/// `ZERO` - файл и кадры считают от одного нуля (поведение до сессии 14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TimelineOrigin(SubtitleTime);

impl TimelineOrigin {
    /// Нулевое начало: поправки нет.
    pub(super) const ZERO: Self = Self(SubtitleTime::from_micros(0));

    /// Начало шкалы файла на шкале кадров.
    ///
    /// Хранится в микросекундах модели субтитров; сверхбольшой старт насыщается.
    pub(super) fn at(start: Duration) -> Self {
        Self(SubtitleTime::from_micros(micros_of(start)))
    }

    /// Переводит позицию кадра на шкалу файла субтитров (может быть отрицательной у кадров
    /// до старта: реплик там нет, а Z/X поверх по-прежнему арифметика со знаком).
    pub(super) fn file_time_for(self, frame_pts: Duration) -> SubtitleTime {
        SubtitleTime::from_micros(micros_of(frame_pts).saturating_sub(self.0.as_micros()))
    }
}

/// Микросекунды `Duration` в модели субтитров; сверхбольшое значение насыщается.
fn micros_of(duration: Duration) -> i64 {
    i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}

/// Результат сообщения владельцу о старте контейнера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContainerStartReport {
    /// Начало шкалы файлов изменилось: дорожки-файлы надо обновить.
    Changed(TimelineOrigin),
    /// Значение то же, что уже учтено.
    Unchanged,
    /// Источник не локальный файл: поправка к нему не применяется.
    NotApplicable,
}

/// Учтённый старт контейнера одного media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ContainerStart {
    applicability: Applicability,
    origin: TimelineOrigin,
}

/// К каким источникам применима поправка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Applicability {
    /// Локальный файл: его шкалу публикует демуксер контейнера.
    LocalFile,
    /// Web, поток и прочее: шкала нормализована не здесь, поправка запрещена.
    Never,
}

impl ContainerStart {
    /// Локальный файл: старт придёт позже из снимка плеера (до тех пор `ZERO`).
    pub(super) const fn for_local_file() -> Self {
        Self {
            applicability: Applicability::LocalFile,
            origin: TimelineOrigin::ZERO,
        }
    }

    /// Любой нелокальный источник: поправки не будет никогда.
    pub(super) const fn not_applicable() -> Self {
        Self {
            applicability: Applicability::Never,
            origin: TimelineOrigin::ZERO,
        }
    }

    /// Начало шкалы, с которым надо создавать новые файловые дорожки.
    pub(super) const fn origin(self) -> TimelineOrigin {
        self.origin
    }

    /// Запоминает старт, опубликованный демуксером.
    pub(super) fn report(&mut self, start: Duration) -> ContainerStartReport {
        if self.applicability == Applicability::Never {
            return ContainerStartReport::NotApplicable;
        }
        let origin = TimelineOrigin::at(start);
        if origin == self.origin {
            return ContainerStartReport::Unchanged;
        }
        self.origin = origin;
        ContainerStartReport::Changed(origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_time_is_frame_pts_minus_origin_and_may_be_negative() {
        let origin = TimelineOrigin::at(Duration::from_millis(1_400));
        assert_eq!(
            origin.file_time_for(Duration::from_millis(2_000)),
            SubtitleTime::from_millis(600)
        );
        assert_eq!(
            origin.file_time_for(Duration::from_millis(600)),
            SubtitleTime::from_millis(-800)
        );
        assert_eq!(
            TimelineOrigin::ZERO.file_time_for(Duration::from_millis(600)),
            SubtitleTime::from_millis(600)
        );
    }

    #[test]
    fn local_file_accepts_start_once_and_foreign_sources_never() {
        let mut local = ContainerStart::for_local_file();
        let start = Duration::from_millis(1_400);
        assert_eq!(
            local.report(start),
            ContainerStartReport::Changed(TimelineOrigin::at(start))
        );
        assert_eq!(local.report(start), ContainerStartReport::Unchanged);
        assert_eq!(local.origin(), TimelineOrigin::at(start));

        let mut web = ContainerStart::not_applicable();
        assert_eq!(web.report(start), ContainerStartReport::NotApplicable);
        assert_eq!(web.origin(), TimelineOrigin::ZERO);
    }
}
