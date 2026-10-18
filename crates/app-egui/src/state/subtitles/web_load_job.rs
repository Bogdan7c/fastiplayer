//! Тело фоновой задачи «скачать и разобрать web-дорожку».
//!
//! Чистая последовательность шагов без потоков и `AppState` (как `file_loader`): поток,
//! доставку результата и отмену обеспечивает `job_runner`, поэтому тесты запускают её синхронно
//! с фальшивым загрузчиком.
//!
//! Шаги: скачать (блокирующе, с `CancellationToken`) → разобрать как srv3/WebVTT → таймлайн →
//! библиотека шрифтов. Между шагами проверяется отмена. Скачивание НИКОГДА не повторяется
//! здесь: любой отказ возвращается типизированно, а повтор — только новым кликом.
//!
//! В логи не попадают ни текст реплик, ни ссылки (загрузчик их не отдаёт по контракту).

use std::num::NonZeroUsize;

use media_source_open::web_subtitle_fetch::{
    WebSubtitleFetchError, WebSubtitleFormat, WebSubtitleTrackId,
};
use source_core::CancellationToken;
use subtitle_core::SubtitleTimeline;
use subtitle_formats::{LegacyEncodingHint, TextSubtitleFormat};

use super::file_loader::parse_text_timeline;
use super::sidecar_job::{
    FontStep, SidecarJobOutcome, SidecarJobRequest, SubtitleLoadFailure,
    build_fonts_unless_cancelled,
};
use super::web_download::WebSubtitleDownloader;

/// Скачанная и разобранная web-дорожка.
pub(super) struct LoadedWebTrack {
    /// Реплики дорожки.
    pub(super) timeline: SubtitleTimeline,
    /// Формат, в котором пришли байты (только для debug-лога).
    pub(super) format: WebSubtitleFormat,
}

/// Формат разбора для скачанного формата.
const fn text_format_of(format: WebSubtitleFormat) -> TextSubtitleFormat {
    match format {
        WebSubtitleFormat::Srv3 => TextSubtitleFormat::Srv3,
        WebSubtitleFormat::WebVtt => TextSubtitleFormat::WebVtt,
    }
}

/// Выполняет задачу целиком. `is_cancelled` — общий флаг отмены задачи; `cancellation` —
/// токен для блокирующей сетевой части (оба переворачиваются вместе).
pub(super) fn run_web_track_load(
    request: &SidecarJobRequest,
    track: &WebSubtitleTrackId,
    downloader: &dyn WebSubtitleDownloader,
    cancellation: &CancellationToken,
    is_cancelled: &dyn Fn() -> bool,
) -> SidecarJobOutcome {
    if is_cancelled() || cancellation.is_cancelled() {
        return SidecarJobOutcome::Cancelled;
    }
    // Лимит размера — тот же, что у файлов субтитров (`subtitles.max_file_size_mb`).
    let Some(max_bytes) = NonZeroUsize::new(request.limits.file_max_bytes) else {
        return SidecarJobOutcome::Failed(SubtitleLoadFailure::FileTooLarge { limit_bytes: 0 });
    };
    let downloaded = match downloader.download(track, max_bytes, cancellation) {
        Ok(downloaded) => downloaded,
        Err(WebSubtitleFetchError::Cancelled) => return SidecarJobOutcome::Cancelled,
        Err(error) => {
            return SidecarJobOutcome::Failed(SubtitleLoadFailure::DownloadFailed(error));
        }
    };
    if is_cancelled() || cancellation.is_cancelled() {
        return SidecarJobOutcome::Cancelled;
    }
    let timeline = match parse_text_timeline(
        &downloaded.bytes,
        text_format_of(downloaded.format),
        // Скачанные байты — XML/WebVTT в UTF-8: старой кодировки у них не бывает.
        LegacyEncodingHint::Generic,
        &request.limits.parse,
    ) {
        Ok(timeline) => timeline,
        Err(failure) => return SidecarJobOutcome::Failed(failure),
    };
    match build_fonts_unless_cancelled(request, is_cancelled) {
        FontStep::Built(fonts) => SidecarJobOutcome::WebTrackLoaded {
            track: Box::new(LoadedWebTrack {
                timeline,
                format: downloaded.format,
            }),
            fonts,
        },
        FontStep::Cancelled => SidecarJobOutcome::Cancelled,
        FontStep::Failed(failure) => SidecarJobOutcome::Failed(failure),
    }
}
