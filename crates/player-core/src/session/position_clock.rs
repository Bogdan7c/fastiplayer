use std::time::{Duration, Instant};

use media_core::{MediaDuration, MediaTime};

use crate::{
    MediaPlaybackWindow, PlaybackState, PlayerError, PlayerErrorKind, PlayerResult, SeekRequest,
};

use super::PlayerSession;

impl PlayerSession {
    /// Обновляет позицию playback из clock/UI без накопления high-frequency событий.
    pub fn update_current_position(&mut self, position: Duration) {
        // Публичный setter остаётся явной lifecycle-операцией: caller меняет
        // позицию и тем самым создаёт новый no-audio anchor в одной точке времени.
        let observed_at = Instant::now();
        let source_position = self.absolute_position_for_relative(position.into());
        self.publish_clock_sample(source_position.as_duration());
        self.reanchor_no_audio_clock(source_position.as_duration(), observed_at);
    }

    /// Публикует измеренную absolute source position как relative public position.
    pub(super) fn publish_clock_sample(&mut self, source_position: Duration) {
        let relative_position =
            self.relative_position_for_source(MediaTime::from_duration(source_position));
        if self.current_source_position == source_position
            && self.snapshot.timeline.current_position == relative_position
        {
            return;
        }

        self.current_source_position = source_position;
        self.snapshot.set_timeline_position(relative_position);
    }

    /// Явно перепривязывает только no-audio clock на lifecycle boundary.
    pub(super) fn reanchor_no_audio_clock(&mut self, position: Duration, observed_at: Instant) {
        if self.snapshot.playback_state == PlaybackState::Playing
            && !self.pipeline.has_audio_clock()
        {
            self.pipeline.start_monotonic_media_clock(
                position,
                observed_at,
                self.snapshot.playback_rate,
            );
        }
    }

    /// Возвращает media clock position на monotonic момент `now`.
    ///
    /// Audio clock остаётся главным источником времени. Если audio clock отсутствует,
    /// Playing/EOF-drain без audio используют внутренний monotonic anchor, а не частоту worker tick-а.
    #[must_use]
    pub(crate) fn presentation_clock_position_at(&self, now: Instant) -> Duration {
        if self.pipeline.has_audio_clock() {
            return self.audio_media_clock_position();
        }

        if let Some(seek_target_position) = self.seek_presentation_clock_override() {
            return seek_target_position;
        }

        if self.monotonic_media_clock_drives_position()
            && let Some(position) = self.pipeline.monotonic_media_position(now)
        {
            return position;
        }

        self.current_source_position
    }

    /// Проецирует ближайшую wall-задержку в media position выбранного clock source.
    #[must_use]
    pub(crate) fn presentation_media_position_after_wall_delay(
        &self,
        now: Instant,
        wall_delay: Duration,
    ) -> Duration {
        if self.pipeline.has_audio_clock() {
            return self
                .pipeline
                .media_position_after_audio_output_delay(wall_delay);
        }

        if self.seek_presentation_clock_override().is_none()
            && self.monotonic_media_clock_drives_position()
            && let Some(position) = self
                .pipeline
                .monotonic_media_position_after_wall_delay(now, wall_delay)
        {
            return position;
        }

        let current_media_position = self.presentation_clock_position_at(now);
        let media_delta = self
            .snapshot
            .playback_rate
            .scale_wall_delta_to_media_delta(wall_delay);
        current_media_position
            .checked_add(media_delta)
            .unwrap_or(Duration::MAX)
    }

    /// Переводит absolute media deadline в wall delay без clock-эвристик scheduler-а.
    #[must_use]
    pub(crate) fn wall_delay_until_media_deadline(
        &self,
        now: Instant,
        media_deadline: Duration,
    ) -> Duration {
        if self.pipeline.has_audio_clock() {
            return self
                .pipeline
                .audio_output_delay_until_media_deadline(media_deadline);
        }

        if self.seek_presentation_clock_override().is_none()
            && self.monotonic_media_clock_drives_position()
            && let Some(wall_delay) = self
                .pipeline
                .monotonic_wall_delay_until_media_deadline(now, media_deadline)
        {
            return wall_delay;
        }

        let current_media_position = self.presentation_clock_position_at(now);
        let media_delta = media_deadline.saturating_sub(current_media_position);
        self.snapshot
            .playback_rate
            .scale_media_delta_to_wall_delay(media_delta)
    }

    /// Проверяет, может ли no-audio monotonic clock сейчас двигать user-visible position.
    pub(super) fn monotonic_media_clock_drives_position(&self) -> bool {
        self.snapshot.playback_state == PlaybackState::Playing || self.eof_drain_needs_progress()
    }

    /// Синхронизирует snapshot position с monotonic fallback clock без изменения playback state.
    pub(super) fn sync_monotonic_media_clock_position(&mut self, now: Instant) {
        if self.pipeline.has_audio_clock() {
            return;
        }

        let position = self.presentation_clock_position_at(now);
        self.publish_clock_sample(position);
    }

    /// Запускает или перезапускает no-audio media clock от текущей snapshot position.
    pub(super) fn anchor_monotonic_media_clock_if_needed(&mut self, now: Instant) {
        if self.pipeline.has_audio_clock() {
            self.pipeline.clear_monotonic_media_clock();
            return;
        }

        self.pipeline.start_monotonic_media_clock(
            self.current_source_position,
            now,
            self.snapshot.playback_rate,
        );
    }

    /// Останавливает no-audio media clock, предварительно сохранив актуальную позицию.
    pub(super) fn clear_monotonic_media_clock_anchor(&mut self, now: Instant) {
        self.sync_monotonic_media_clock_position(now);
        self.pipeline.clear_monotonic_media_clock();
    }

    /// Публикует уже frozen presentation position перед обычной Pause.
    pub(super) fn freeze_current_position_for_pause(&mut self, current_position: Duration) {
        self.publish_clock_sample(current_position);
        self.pipeline.clear_monotonic_media_clock();
    }

    /// Возвращает абсолютную media position по audio clock.
    fn audio_media_clock_position(&self) -> Duration {
        self.pipeline.media_position_from_audio_clock()
    }

    /// Добавляет delta к текущей позиции без panic при переполнении.
    pub fn advance_position(&mut self, delta: Duration) {
        let next_position = self
            .snapshot
            .current_position
            .checked_add(delta)
            .unwrap_or(Duration::MAX);
        self.update_current_position(next_position);
    }

    /// Разрешает seek target в абсолютную media-позицию без изменения runtime seek policy.
    pub(super) fn resolve_seek_target(&self, request: SeekRequest) -> PlayerResult<MediaTime> {
        let relative_target = request
            .target
            .resolve(self.snapshot.timeline.current_position);

        if self.snapshot.timeline.mode == media_core::TimelineMode::Live {
            let range = self.snapshot.timeline.seekable_range.ok_or_else(|| {
                PlayerError::new(
                    PlayerErrorKind::SeekUnavailable,
                    "Live source does not currently expose a DVR window",
                )
            })?;
            if !range.contains(relative_target) {
                return Err(PlayerError::new(
                    PlayerErrorKind::SeekTargetExpired,
                    format!(
                        "Live seek target {} ms is outside latest DVR range {:?}",
                        relative_target.as_duration().as_millis(),
                        range
                    ),
                ));
            }
            return Ok(relative_target);
        }

        let clamped_relative = self
            .snapshot
            .timeline
            .seekable_range
            .map(|range| relative_target.clamp_to(range))
            .unwrap_or(relative_target);
        Ok(self.absolute_position_for_relative(clamped_relative))
    }

    /// Синхронно обновляет physical source duration и public relative duration.
    pub(super) fn set_snapshot_duration(&mut self, source_duration: Option<Duration>) {
        self.source_duration = source_duration;
        let public_duration = self.playback_window.map_or_else(
            || source_duration.map(MediaDuration::from_duration),
            |window| window.relative_duration(source_duration),
        );
        self.snapshot.set_timeline_duration(public_duration);
    }

    /// Переводит absolute source position в bounded public relative position.
    pub(super) fn relative_position_for_source(&self, source_position: MediaTime) -> MediaTime {
        self.playback_window.map_or(source_position, |window| {
            window.relative_position(source_position, self.source_duration)
        })
    }

    /// Переводит public relative position в absolute demux/source position.
    pub(super) fn absolute_position_for_relative(&self, relative_position: MediaTime) -> MediaTime {
        self.playback_window.map_or(relative_position, |window| {
            window.absolute_position(relative_position, self.source_duration)
        })
    }

    /// Публикует absolute seek/scrub target в relative timeline snapshot.
    pub(super) fn set_timeline_target_from_source(&mut self, source_target: MediaTime) {
        self.snapshot.timeline.target_position =
            Some(self.relative_position_for_source(source_target));
    }

    /// Возвращает absolute exclusive end активного bounded window.
    pub(super) fn playback_window_end(&self) -> Option<MediaTime> {
        self.playback_window
            .and_then(MediaPlaybackWindow::end_exclusive)
    }

    /// Отбрасывает packet на/после bounded end и отмечает selected-track progress.
    pub(super) fn packet_is_outside_playback_window(
        &mut self,
        packet: &media_core::Packet,
    ) -> bool {
        let Some(playback_window) = self.playback_window else {
            return false;
        };
        if playback_window.admits_packet_at(packet.pts) {
            return false;
        }

        let belongs_to_selected_track = match packet.kind {
            media_core::TrackKind::Audio => {
                self.pipeline.selected_audio_track_id() == Some(packet.track_id)
            }
            media_core::TrackKind::Video => {
                self.pipeline.selected_video_track_id() == Some(packet.track_id)
            }
        };
        if belongs_to_selected_track {
            self.playback_window_end_state
                .note_selected_track_end(packet.kind);
        }
        true
    }

    /// Проверяет audio packet, который целиком лежит до absolute window start.
    ///
    /// Пересекающий start packet сохраняется: audio runtime уже обрезает PCM по
    /// установленному absolute media clock base, как при Accurate seek.
    pub(super) fn audio_packet_is_before_playback_window(
        &self,
        packet: &media_core::Packet,
    ) -> bool {
        if packet.kind != media_core::TrackKind::Audio {
            return false;
        }
        let Some(playback_window) = self.playback_window else {
            return false;
        };
        let Some(packet_duration) = packet.duration else {
            return false;
        };
        packet.pts.saturating_add(packet_duration) <= playback_window.start().as_duration()
    }

    /// Проверяет готовность synthetic EOF после пересечения end выбранными tracks.
    pub(super) fn playback_window_end_observed(&self) -> bool {
        self.playback_window_end_state.all_selected_tracks_ended(
            self.pipeline.selected_audio_track_id().is_some(),
            self.pipeline.selected_video_track_id().is_some(),
        )
    }

    /// Сбрасывает end progress после install/seek discontinuity.
    pub(super) fn reset_playback_window_end_observation(&mut self) {
        self.playback_window_end_state.reset();
    }

    /// Проверяет decoded frame против absolute start/end активного window.
    pub(super) fn playback_window_admits_frame(&self, absolute_pts: Duration) -> bool {
        self.playback_window
            .is_none_or(|window| window.admits_frame_at(absolute_pts))
    }
}
