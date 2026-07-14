use super::present_frame_cache::CachedPresentFrameDiscardReason;
use super::*;

/// Сохраняет прежний state-module import path до миграции callsites в Session 10D.
pub(crate) use crate::media_open::ActiveMediaSource;

impl AppState {
    /// Возвращает cloneable ordered player control stream для process-lifetime owner-а.
    pub(crate) fn player_command_sender(&self) -> player_core::PlayerCommandSender {
        self.player_worker.command_sender()
    }

    /// Загружает локальный файл через playback worker.
    pub fn load_file(&mut self, path: &Path) {
        let autoplay = self.committed_config_snapshot.autoplay_for_new_media();
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::MediaOpenBoundary);
        self.clear_startup_status();
        self.current_local_file = Some(path.to_path_buf());

        match local_media::prepare_local_file(
            path,
            &self.committed_config_snapshot.demux_config_for_open(),
        ) {
            Ok(prepared_media) => {
                if let Err(error) = self
                    .player_worker
                    .load_prepared_media(prepared_media, autoplay)
                {
                    warn!(error = %error, "Не удалось отправить подготовленный файл в worker");
                    return;
                }
                self.remember_active_media_source(ActiveMediaSource::LocalFile(path.to_path_buf()));
            }
            Err(error) => {
                warn!(error = %error, "Не удалось открыть файл");
                let open_request =
                    MediaOpenRequest::new(MediaSource::LocalFile(path.to_path_buf()), autoplay);
                let player_error =
                    PlayerError::new(PlayerErrorKind::DemuxError, format!("Ошибка: {error}"));
                if let Err(send_error) = self
                    .player_worker
                    .fail_media_open(open_request, player_error)
                {
                    warn!(error = %send_error, "Не удалось отправить ошибку открытия файла в worker");
                    return;
                }
            }
        }

        self.mark_pending_worker_redraw();
    }

    /// Доставляет уже подготовленный локальный media в worker после async UI opening-а.
    pub(crate) fn load_prepared_local_file(
        &mut self,
        path: PathBuf,
        prepared_media: PreparedMedia,
    ) -> bool {
        let autoplay = self.committed_config_snapshot.autoplay_for_new_media();

        if let Err(error) = self
            .player_worker
            .load_prepared_media(prepared_media, autoplay)
        {
            warn!(error = %error, path = %path.display(), "Не удалось отправить подготовленный файл в worker");
            self.set_startup_error(format!(
                "Ошибка открытия media-файла {}: worker недоступен: {error}",
                path.display()
            ));
            return false;
        }

        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::MediaOpenBoundary);
        self.clear_startup_status();
        self.current_local_file = Some(path.clone());
        self.remember_active_media_source(ActiveMediaSource::LocalFile(path));
        self.mark_pending_worker_redraw();
        true
    }

    /// Загружает YouTube demuxer без долговременного database/cache слоя.
    pub fn load_youtube_demuxer(
        &mut self,
        source_locator: service_youtube::YoutubeMediaLocator,
        label: String,
        demuxer: Box<dyn symphonia_demux::Demuxer + Send>,
        selected_stream_identity: service_youtube::YoutubeSelectedStreamIdentity,
    ) -> bool {
        let autoplay = self.committed_config_snapshot.autoplay_for_new_media();
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::MediaOpenBoundary);
        self.clear_startup_status();
        self.current_local_file = None;
        let prepared_media = PreparedMedia::from_external_label(label, demuxer);
        if let Err(error) = self
            .player_worker
            .load_prepared_media(prepared_media, autoplay)
        {
            warn!(error = %error, "Не удалось отправить YouTube demuxer в worker");
            self.set_startup_error(format!(
                "WorkerUnavailable: YouTube worker недоступен для {source_locator}: {error}"
            ));
            return false;
        }

        self.remember_active_media_source(ActiveMediaSource::YouTubeUrl {
            source_locator,
            selected_stream_identity,
        });
        self.mark_pending_worker_redraw();
        true
    }

    /// Загружает уже подготовленный внешний media source через PreparedMedia boundary.
    pub fn load_prepared_external_media(
        &mut self,
        label: String,
        prepared_media: PreparedMedia,
    ) -> bool {
        let autoplay = self.committed_config_snapshot.autoplay_for_new_media();
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::MediaOpenBoundary);
        self.clear_startup_status();
        self.current_local_file = None;

        if let Err(error) = self
            .player_worker
            .load_prepared_media(prepared_media, autoplay)
        {
            warn!(error = %error, label = %label, "Не удалось отправить внешний media source в worker");
            self.set_startup_error(format!(
                "WorkerUnavailable: direct media worker недоступен для {label}: {error}"
            ));
            return false;
        }

        self.mark_pending_worker_redraw();
        true
    }

    /// Загружает подготовленный direct media URL и запоминает восстановимый source intent.
    pub fn load_prepared_direct_media(
        &mut self,
        source_locator: service_direct_media::DirectMediaUrl,
        label: String,
        prepared_media: PreparedMedia,
    ) -> bool {
        if self.load_prepared_external_media(label, prepared_media) {
            self.remember_active_media_source(ActiveMediaSource::DirectMediaUrl(source_locator));
            true
        } else {
            false
        }
    }

    /// Возвращает последний локальный файл, открытый shell-ом.
    #[must_use]
    pub fn current_local_file(&self) -> Option<&Path> {
        self.current_local_file.as_deref()
    }

    /// Возвращает восстановимый active source intent для controlled media rebuild.
    #[must_use]
    pub(crate) fn active_media_source(&self) -> Option<ActiveMediaSource> {
        self.active_media_source.clone()
    }

    fn remember_active_media_source(&mut self, source: ActiveMediaSource) {
        self.active_media_source = Some(source);
    }

    /// Восстанавливает runtime playback controls после controlled media reopen.
    pub(crate) fn restore_playback_after_media_reconfigure(&mut self, snapshot: &PlayerSnapshot) {
        self.send_restore_command(PlayerCommand::SetVolume(snapshot.volume));

        if let Some(track_id) = snapshot.selected_tracks.video_track {
            self.send_restore_command(PlayerCommand::SelectVideoTrack(track_id));
        }
        if let Some(track_id) = snapshot.selected_tracks.audio_track {
            self.send_restore_command(PlayerCommand::SelectAudioTrack(track_id));
        }
        if let Some(track_id) = snapshot.selected_tracks.subtitle_track {
            self.send_restore_command(PlayerCommand::SelectSubtitleTrack(Some(track_id)));
        }
        if let Some(selected_quality) = snapshot
            .available_qualities
            .iter()
            .find(|quality| quality.selected)
        {
            self.send_restore_command(PlayerCommand::SelectQuality(QualitySelection::Specific(
                selected_quality.id.clone(),
            )));
        }

        if snapshot.current_position > Duration::ZERO {
            self.send_restore_command(PlayerCommand::Seek(SeekRequest::absolute(
                snapshot.current_position.into(),
            )));
        }

        match snapshot.playback_state {
            PlaybackState::Playing
            | PlaybackState::Buffering
            | PlaybackState::Seeking
            | PlaybackState::Draining => self.send_restore_command(PlayerCommand::Play),
            PlaybackState::Scrubbing | PlaybackState::Paused => {
                self.send_restore_command(PlayerCommand::Pause);
            }
            PlaybackState::Idle
            | PlaybackState::Opening
            | PlaybackState::Stopped
            | PlaybackState::Ended
            | PlaybackState::Failed => {}
        }
    }

    /// Отправляет restore command, сохраняя ошибки доставки видимыми в логах.
    pub(super) fn send_restore_command(&mut self, command: PlayerCommand) {
        if let Err(error) = self.player_worker.try_send_command(command) {
            warn!(error = %error, "Не удалось восстановить playback state после media reconfigure");
        }
    }

    /// Возвращает `true`, пока shell ждёт file dialog или подготовку локального media.
    #[must_use]
    pub fn has_pending_local_file_open(&self) -> bool {
        self.local_file_open_job.is_some()
    }

    /// Неблокирующе забирает события async открытия локального файла.
    pub fn poll_local_file_open_job(&mut self) -> bool {
        let Some(job) = self.local_file_open_job.as_mut() else {
            self.local_file_open_wake_port
                .acknowledge_abandoned_mailbox();
            return false;
        };
        let drain = job.drain();
        let had_visible_mutation = drain.has_payload();

        if let Some(path) = drain.preparing_path {
            self.set_startup_pending(preparing_local_file_message(&path));
        }

        if let Some(result) = drain.completion {
            self.local_file_open_job = None;
            self.apply_local_file_open_result(result);
        }

        had_visible_mutation
    }

    /// Применяет финальный результат local open job-а к shell и worker boundary.
    pub(super) fn apply_local_file_open_result(&mut self, result: LocalFileOpenResult) {
        match result {
            LocalFileOpenResult::Cancelled => {
                self.startup_pending = None;
                self.mark_pending_worker_redraw();
            }
            LocalFileOpenResult::Prepared {
                path,
                prepared_media,
            } => {
                self.load_prepared_local_file(path, prepared_media);
            }
            LocalFileOpenResult::PrepareFailed { path, error } => {
                warn!(path = %path.display(), error = %error, "Не удалось подготовить локальный файл");
                self.set_startup_error(local_file_prepare_error_message(&path, &error));
            }
            LocalFileOpenResult::JobFailed { error } => {
                warn!(error = %error, "Local file open job завершился ошибкой");
                self.set_startup_error(format!("Ошибка открытия media-файла: {error}"));
            }
        }
    }
}
