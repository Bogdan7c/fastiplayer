//! Web-дорожки в владельце субтитров: установка списка, задача загрузки, уведомления о сбоях.
//!
//! # Границы
//! * Список дорожек и их порядок — `web_track` (чистая логика); скачивание — за контрактом
//!   `WebSubtitleDownloader` (`web_download`); сам разбор и сетевой вызов — в фоновой задаче
//!   `web_load_job`. Здесь только склейка с состоянием media.
//! * Автоповторов нет: провал скачивания записывается в дорожку (`Unavailable(DownloadFailed)`)
//!   и один раз ставится в очередь уведомлений. Новая попытка — только когда пользователь
//!   снова выбирает дорожку (`TrackEntry::prepare_retry_after_failed_download`).
//! * Уведомления отдаются наружу как типизированные ошибки (`take_download_failures`): текст
//!   пользователю и показ — задача `AppState`/`subtitle_message`.

use media_source_open::web_subtitle_fetch::WebSubtitleFetchError;
use source_core::CancellationToken;

use super::SubtitleOwner;
use super::media_state::MediaSubtitles;
use super::sidecar_job::SidecarJobTask;
use super::web_track::{WebSubtitleSource, WebTrackDescription};

impl MediaSubtitles {
    /// Подключает web-дорожки к списку media и запоминает загрузчик.
    pub(super) fn install_web_source(
        &mut self,
        source: &WebSubtitleSource,
        descriptions: Vec<WebTrackDescription>,
    ) {
        self.tracks.push_web_tracks(descriptions);
        self.web_downloader = Some(source.downloader());
    }

    /// Задача загрузки web-дорожки; `None`, если у media нет загрузчика (нарушение
    /// инварианта «web-дорожки появляются только вместе с загрузчиком»).
    pub(super) fn web_load_task(
        &self,
        description: &WebTrackDescription,
    ) -> Option<SidecarJobTask> {
        let downloader = self.web_downloader.as_ref()?;
        Some(SidecarJobTask::LoadWebTrack {
            track: description.track_id().clone(),
            downloader: std::sync::Arc::clone(downloader),
            cancellation: CancellationToken::new(),
        })
    }
}

impl SubtitleOwner {
    /// Сбои скачивания с прошлой выдачи, по одному на неудавшуюся попытку (выдаются один раз).
    /// По ним `AppState` показывает уведомление; отменённые и устаревшие попытки сюда не попадают.
    pub(crate) fn take_download_failures(&mut self) -> Vec<WebSubtitleFetchError> {
        std::mem::take(&mut self.download_failures)
    }
}
