//! Приём результатов фоновых задач и запуск ленивой загрузки дорожек владельцем.
//!
//! Инварианты:
//! - результат принимается, только если media владельца совпало с тем, для которого задача
//!   запускалась, и задача всё ещё числится актуальной (поиск — `Searching`, загрузка —
//!   `load_job` той же дорожки). Иначе результат выбрасывается как устаревший;
//! - одновременно идёт не больше одной загрузки дорожки: смена показываемой дорожки
//!   отменяет предыдущую загрузку (дорожка возвращается в `NotLoaded`);
//! - поздний результат поиска НЕ переопределяет выбор, который пользователь уже сделал.

use player_core::MediaInstanceId;
use subtitle_core::SubtitleTrackId;
use subtitle_raster::SubtitleFontLibrary;

use super::active_track::ActiveSubtitleTrack;
use super::file_loader::LoadedSidecarTrack;
use super::job_runner::SidecarJob;
use super::media_state::{DiscoveryState, MediaSubtitles, TrackLoadJob};
use super::sidecar_job::{
    DiscoveryOutcome, PreloadedTrack, SidecarJobOutcome, SidecarJobRequest, SidecarJobTask,
    SubtitleLoadFailure,
};
use super::track_list::TrackLoadState;
use super::{JobOrigin, JobResultDisposition, SubtitleOwner};

impl SubtitleOwner {
    /// Применяет результат задачи, запущенной для `media_instance_id`.
    ///
    /// Поздний результат для другого media (или уже замененной задачи) выбрасывается:
    /// проверка идёт по текущему состоянию, а не по порядку доставки.
    pub(super) fn accept_job_outcome(
        &mut self,
        media_instance_id: MediaInstanceId,
        origin: JobOrigin,
        outcome: SidecarJobOutcome,
    ) -> JobResultDisposition {
        if !self.job_origin_is_current(media_instance_id, origin) {
            tracing::debug!("Dropped a subtitle result for media that is no longer current");
            return JobResultDisposition::DroppedStale;
        }
        let disposition = match origin {
            JobOrigin::Discovery => self.accept_discovery_outcome(outcome),
            JobOrigin::TrackLoad(track_id) => self.accept_track_load_outcome(track_id, outcome),
        };
        if disposition == JobResultDisposition::Applied {
            self.presenter.forget_shown_overlay();
            // Теперь, когда список дорожек известен, показываемая дорожка может требовать чтения.
            self.ensure_display_track_loading();
        }
        disposition
    }

    /// Задача `origin` ещё актуальна для media `media_instance_id`.
    fn job_origin_is_current(&self, media_instance_id: MediaInstanceId, origin: JobOrigin) -> bool {
        let Some(media) = &self.media else {
            return false;
        };
        if media.media_instance_id != media_instance_id {
            return false;
        }
        match origin {
            JobOrigin::Discovery => matches!(media.discovery, DiscoveryState::Searching(_)),
            JobOrigin::TrackLoad(track_id) => media
                .load_job
                .as_ref()
                .is_some_and(|load| load.track_id == track_id),
        }
    }

    fn accept_discovery_outcome(&mut self, outcome: SidecarJobOutcome) -> JobResultDisposition {
        let Some(media) = self.media.as_mut() else {
            return JobResultDisposition::DroppedStale;
        };
        match outcome {
            SidecarJobOutcome::Cancelled => JobResultDisposition::DroppedStale,
            SidecarJobOutcome::Failed(failure) => {
                tracing::warn!(%failure, "Subtitle search failed; playback continues");
                media.discovery = DiscoveryState::Failed(failure);
                JobResultDisposition::Applied
            }
            SidecarJobOutcome::Discovered(discovery) => {
                self.apply_discovery(*discovery);
                JobResultDisposition::Applied
            }
            SidecarJobOutcome::FileLoaded { .. } | SidecarJobOutcome::FontsBuilt(_) => {
                tracing::error!("A search task returned a foreign result; ignoring it");
                media.discovery = DiscoveryState::Failed(SubtitleLoadFailure::WorkerLost);
                JobResultDisposition::Applied
            }
        }
    }

    /// Добавляет найденные дорожки, применяет автовыбор и прочитанную дорожку.
    fn apply_discovery(&mut self, discovery: DiscoveryOutcome) {
        let DiscoveryOutcome {
            candidates,
            preloaded,
            fonts,
            // Автовыбор задачи нужен только ей самой (чтобы решить, что прочитать); итоговый
            // выбор считается заново над полным списком.
            selection: _,
        } = discovery;
        self.install_fonts_if_needed(fonts);
        let Some(media) = self.media.as_mut() else {
            return;
        };
        // Завершённая задача больше не нужна: освобождаем поток.
        media.discovery = DiscoveryState::Finished;

        let track_ids = media.tracks.insert_discovered(&candidates);
        let id_of = |index: usize| track_ids.get(index).copied();
        // Автовыбор по всему списку (файлы + встроенные, которые могли появиться раньше поиска).
        // Считается ДО применения прочитанной дорожки: нечитаемый файл остаётся выбранным и
        // показывается как недоступный, а не подменяется молча другой дорожкой.
        self.reselect_automatically();
        let Some(media) = self.media.as_mut() else {
            return;
        };
        tracing::debug!(
            tracks = track_ids.len(),
            auto_selected = media.selected.is_some(),
            "Subtitle search finished"
        );

        if let Some(PreloadedTrack {
            candidate_index,
            result,
        }) = preloaded
            && let Some(track_id) = id_of(candidate_index)
        {
            let sync_offset = media.sync_offset;
            let source_id = self.source_ids.allocate();
            if let Some(entry) = media.tracks.get_mut(track_id) {
                entry.load = loaded_state(result, source_id, sync_offset);
            }
        }
    }

    fn accept_track_load_outcome(
        &mut self,
        track_id: SubtitleTrackId,
        outcome: SidecarJobOutcome,
    ) -> JobResultDisposition {
        let (result, fonts) = match outcome {
            SidecarJobOutcome::Cancelled => return JobResultDisposition::DroppedStale,
            SidecarJobOutcome::FileLoaded { track, fonts } => (Ok(*track), fonts),
            SidecarJobOutcome::Failed(failure) => (Err(failure), None),
            SidecarJobOutcome::Discovered(_) | SidecarJobOutcome::FontsBuilt(_) => {
                tracing::error!("A file task returned a foreign result; ignoring it");
                (Err(SubtitleLoadFailure::WorkerLost), None)
            }
        };
        self.install_fonts_if_needed(fonts);
        let Some(media) = self.media.as_mut() else {
            return JobResultDisposition::DroppedStale;
        };
        // Завершённая задача больше не нужна: освобождаем поток.
        media.load_job = None;
        let sync_offset = media.sync_offset;
        let source_id = self.source_ids.allocate();
        if let Some(entry) = media.tracks.get_mut(track_id) {
            entry.load = loaded_state(result, source_id, sync_offset);
        }
        JobResultDisposition::Applied
    }

    /// Создаёт растеризатор из принесённой библиотеки, если его ещё нет (две параллельные
    /// задачи могли построить библиотеки: достаётся первая, вторая выбрасывается).
    pub(super) fn install_fonts_if_needed(&mut self, fonts: Option<SubtitleFontLibrary>) {
        if let Some(fonts) = fonts
            && !self.presenter.has_rasterizer()
        {
            self.presenter.install_font_library(fonts);
        }
        // Вложенные шрифты могли ждать именно растеризатора.
        self.install_ready_attached_fonts();
    }

    /// Гарантирует, что показываемая дорожка читается: если она ещё не загружена, запускает
    /// фоновую загрузку (предыдущая загрузка другой дорожки отменяется).
    pub(super) fn ensure_display_track_loading(&mut self) {
        let fonts = self.font_plan_for_next_job();
        let limits = self.settings.load_limits;
        let Some(media) = self.media.as_mut() else {
            return;
        };
        let Some(target) = media.display_track_id() else {
            return;
        };
        if media
            .load_job
            .as_ref()
            .is_some_and(|load| load.track_id == target)
        {
            return;
        }
        let Some(entry) = media.tracks.get(target) else {
            return;
        };
        if !matches!(entry.load, TrackLoadState::NotLoaded) {
            return;
        }
        // Встроенные дорожки не читаются из файла: они готовы с самого начала.
        let Some(candidate) = entry.description.sidecar_candidate().cloned() else {
            return;
        };
        let request = SidecarJobRequest {
            task: SidecarJobTask::LoadFile { candidate },
            fonts,
            limits,
        };

        // Другая загрузка больше не нужна: отменяем и возвращаем её дорожку в «не загружена».
        if let Some(previous) = media.load_job.take() {
            previous.job.cancel();
            if let Some(previous_entry) = media.tracks.get_mut(previous.track_id) {
                previous_entry.load = TrackLoadState::NotLoaded;
            }
            self.retiring_jobs.push(previous.job);
        }
        spawn_track_load(media, target, request, self.wake_port.clone());
    }
}

/// Запускает загрузку и записывает её состояние в дорожку.
fn spawn_track_load(
    media: &mut MediaSubtitles,
    track_id: SubtitleTrackId,
    request: SidecarJobRequest,
    wake_port: crate::app_wake::AppWakePort,
) {
    let new_state = match SidecarJob::spawn(request, wake_port) {
        Ok(job) => {
            media.load_job = Some(TrackLoadJob { track_id, job });
            TrackLoadState::Loading
        }
        Err(failure) => {
            tracing::warn!(%failure, "Subtitle track is unavailable; playback continues");
            TrackLoadState::Unavailable(failure)
        }
    };
    if let Some(entry) = media.tracks.get_mut(track_id) {
        entry.load = new_state;
    }
}

/// Итоговое состояние дорожки по результату чтения.
fn loaded_state(
    result: Result<LoadedSidecarTrack, SubtitleLoadFailure>,
    source_id: subtitle_raster::CueSourceId,
    sync_offset: subtitle_core::SubtitleSyncOffset,
) -> TrackLoadState {
    match result {
        Ok(loaded) => {
            tracing::debug!(
                format = ?loaded.format,
                file_name = ?loaded.file_name,
                events = loaded.timeline.len(),
                "Subtitle track loaded"
            );
            TrackLoadState::Ready(ActiveSubtitleTrack::new(
                source_id,
                loaded.timeline,
                sync_offset,
                loaded.language,
            ))
        }
        Err(failure) => {
            tracing::warn!(%failure, "Subtitle file is unavailable; playback continues");
            TrackLoadState::Unavailable(failure)
        }
    }
}
