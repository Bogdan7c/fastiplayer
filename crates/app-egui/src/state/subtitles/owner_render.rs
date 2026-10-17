//! Владелец и поток-рисовальщик: опрос результатов, отказ потока, часы показа без кадра.
//!
//! Сам показ кадра — в `owner_placement`; здесь то, что происходит между кадрами.

use super::render_worker::RenderWorkerFailure;
use super::render_worker::clock_tracker::PlaybackObservation;
use super::sidecar_job::SubtitleLoadFailure;
use super::track_list::TrackLoadState;
use super::{CurrentMediaForSubtitles, SubtitleOwner};

impl SubtitleOwner {
    /// Опрос потока-рисовальщика по wake-событию. `true` — нужна перерисовка: пришла картинка
    /// для текущего кадра (на паузе новый кадр сам не придёт) либо поток отказал.
    pub(super) fn poll_render_worker(&mut self) -> bool {
        let picture_for_current_frame = self.presenter.poll_worker();
        match self.presenter.take_failure() {
            Some(failure) => {
                self.mark_display_track_unavailable_after(failure);
                true
            }
            None => picture_for_current_frame,
        }
    }

    /// Поток отказал: показываемая дорожка становится недоступной с понятной причиной (меню
    /// покажет её). Воспроизведение видео это не затрагивает. Повторное добавление файла
    /// снова пробует загрузку, а следующая загрузка пересоздаёт поток.
    pub(super) fn mark_display_track_unavailable_after(&mut self, failure: RenderWorkerFailure) {
        let failure = match failure {
            RenderWorkerFailure::StartFailed { kind } => {
                SubtitleLoadFailure::RenderWorkerStartFailed { kind }
            }
            RenderWorkerFailure::Lost => SubtitleLoadFailure::RenderWorkerLost,
        };
        let Some(media) = self.media.as_mut() else {
            return;
        };
        if let Some(track_id) = media.display_track_id()
            && let Some(entry) = media.tracks.get_mut(track_id)
        {
            entry.load = TrackLoadState::Unavailable(failure);
        }
    }

    /// Принимает наблюдение за плеером (UI вызывает на каждом кадре, в том числе когда видео
    /// кадра нет). Во время seek/scrub старый кадр остаётся на экране без субтитров, а цель
    /// уже известна: сообщаем её потоку сразу, чтобы картинка цели была готова к первому
    /// кадру после seek.
    pub(super) fn observe_playback(
        &mut self,
        current: CurrentMediaForSubtitles,
        observation: PlaybackObservation,
    ) {
        self.presenter.observe_playback(observation);
        if !self.presenter.playback_follows_target() || self.appearance_edit.is_some() {
            return;
        }
        let Some(media) = self.media.as_ref() else {
            return;
        };
        if current.media_instance_id != Some(media.media_instance_id) {
            return;
        }
        let Some(entry) = media
            .display_track_id()
            .and_then(|track_id| media.tracks.get(track_id))
        else {
            return;
        };
        if let TrackLoadState::Ready(track) = &entry.load {
            self.presenter.refresh_target_clock(track);
        }
    }
}
