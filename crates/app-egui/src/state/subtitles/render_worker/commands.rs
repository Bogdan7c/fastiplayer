//! Язык общения UI и потока-рисовальщика: команды (UI -> поток) и события (поток -> UI).
//!
//! Все команды идут одним каналом, поэтому порядок сохраняется: «заменить дорожку» всегда
//! обработается раньше «рисуй для этой дорожки», отправленного позже. События идут обратным
//! каналом тоже по порядку; на этом держится честность отмены: картинка несёт `mirror_serial`
//! зеркала дорожки, и UI отбрасывает картинку, нарисованную до поздней дописки реплик.

use std::time::Duration;

use subtitle_core::{SubtitleEvent, SubtitleLanguage, SubtitleTimeline};
use subtitle_raster::{
    AssRenderOptions, AttachedFontLimits, AttachedFontsClearOutcome, AttachedFontsInstallReport,
    CanvasSize, CueSourceId, PreparedAttachedFonts, SubtitleAppearance, TextLanguagePreference,
};

use super::prepared_queue::{PreparedOverlay, RenderInputsRevision};
use super::schedule::PresentationClock;

/// Для какой дорожки и на какой холст рисует поток.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct RenderTarget {
    /// Дорожка (источник реплик), чьи реплики рисуем.
    pub(in crate::state::subtitles) source: CueSourceId,
    /// Холст: размер картинки видео на экране.
    pub(in crate::state::subtitles) canvas: CanvasSize,
}

/// Входы рисования, не зависящие от кадра: стиль, настройки ASS и их ревизия.
#[derive(Debug, Clone)]
pub(in crate::state::subtitles) struct LookInputs {
    /// Ревизия: картинки, нарисованные с этими входами, получают её в своей идентичности.
    pub(in crate::state::subtitles) revision: RenderInputsRevision,
    /// Внешний вид простых субтитров.
    pub(in crate::state::subtitles) appearance: SubtitleAppearance,
    /// Настройки ASS (масштаб, источник стиля, размер картинки видео).
    pub(in crate::state::subtitles) ass: AssRenderOptions,
}

/// Изменение зеркала дорожки на стороне потока.
pub(in crate::state::subtitles) enum TrackCommand {
    /// Дорожка целиком (первая отправка или полная пересинхронизация).
    Replace {
        /// Порядковый номер изменения (общий счётчик UI): картинки поток помечает номером
        /// последнего применённого изменения дорожки.
        serial: u64,
        source: CueSourceId,
        language: Option<SubtitleLanguage>,
        timeline: SubtitleTimeline,
    },
    /// Дописанные реплики встроенной дорожки, в порядке их id (так id в зеркале совпадут).
    Append {
        /// Порядковый номер изменения (см. `Replace`).
        serial: u64,
        source: CueSourceId,
        events: Vec<SubtitleEvent>,
    },
    /// Дорожка больше не нужна, зеркало можно выбросить.
    Forget { source: CueSourceId },
}

/// Команда потоку-рисовальщику.
pub(in crate::state::subtitles) enum WorkerCommand {
    /// Изменение зеркала дорожки.
    Track(TrackCommand),
    /// «Часы показа»: для чего и когда рисовать. Последние часы побеждают.
    ///
    /// `look` (если изменился) едет В ТОЙ ЖЕ команде: иначе поток мог бы успеть обработать
    /// новые входы раньше новой цели и впустую перерисовать прежнюю цель.
    Present {
        target: RenderTarget,
        clock: PresentationClock,
        look: Option<LookInputs>,
    },
    /// Установить подготовленные вложенные шрифты media.
    ///
    /// `look` с новой ревизией едет в той же команде: иначе поток успел бы впустую
    /// нарисовать кадр со старой ревизией входов.
    InstallAttachedFonts {
        prepared: PreparedAttachedFonts,
        limits: AttachedFontLimits,
        look: Option<LookInputs>,
    },
    /// Снять вложенные шрифты прежнего media (`look` — как у установки).
    ClearAttachedFonts { look: Option<LookInputs> },
    /// Остановиться.
    Shutdown,
    /// Только тесты: ответить `Idle`, когда вся накопленная работа сделана.
    #[cfg(test)]
    Flush { token: u64 },
    /// Только тесты: искусственная задержка каждой растеризации (модель занятого потока).
    #[cfg(test)]
    SetRenderDelay(Duration),
    /// Только тесты: поток завершается без прощания (модель гибели потока).
    #[cfg(test)]
    SimulateCrash,
    /// Только тесты: прислать текущие счётчики растеризатора.
    #[cfg(test)]
    QueryDiagnostics,
}

/// Сведения о выполненной растеризации (диагностика и счётчики).
#[derive(Debug, Clone, Copy)]
pub(in crate::state::subtitles) struct RenderReport {
    /// Сколько длилась растеризация.
    pub(in crate::state::subtitles) elapsed: Duration,
    /// Сколько реплик ушло в запрос растеризатору.
    pub(in crate::state::subtitles) cue_count: usize,
    /// Сколько реплик пропущено: их тип содержимого ещё не поддерживается.
    pub(in crate::state::subtitles) skipped_unsupported_cues: u64,
    /// Язык текста, с которым рисовали.
    pub(in crate::state::subtitles) text_language: TextLanguagePreference,
}

/// Диагностика потока (только тесты).
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(in crate::state::subtitles) struct WorkerDiagnostics {
    /// Счётчики растеризатора.
    pub(in crate::state::subtitles) raster: subtitle_raster::RasterStatistics,
    /// Сколько раз защита от зацикливания отказалась рисовать то же самое второй раз подряд.
    pub(in crate::state::subtitles) repeated_jobs_suppressed: u64,
}

/// Событие потока-рисовальщика.
pub(in crate::state::subtitles) enum WorkerEvent {
    /// Готовая картинка со сроком годности.
    Prepared {
        picture: PreparedOverlay,
        report: RenderReport,
        /// Номер последнего изменения дорожки, учтённого при рисовании: если UI с тех пор
        /// дописал реплики, задевающие срок картинки, картинку надо выбросить.
        mirror_serial: u64,
    },
    /// Итог установки вложенных шрифтов.
    AttachedFontsInstalled(AttachedFontsInstallReport),
    /// Итог снятия вложенных шрифтов.
    AttachedFontsCleared(AttachedFontsClearOutcome),
    /// Только тесты: ответ на `Flush`.
    #[cfg(test)]
    Idle { token: u64 },
    /// Только тесты: счётчики растеризатора.
    #[cfg(test)]
    Diagnostics(WorkerDiagnostics),
}
