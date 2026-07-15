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
    pub fn load_file(
        &mut self,
        admitted_open: crate::playlist_runtime::AdmittedLocalFileOpen,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) {
        let path = admitted_open.into_path();
        match local_media::prepare_local_file(
            &path,
            &self.committed_config_snapshot.demux_config_for_open(),
        ) {
            Ok(prepared_media) => {
                self.load_prepared_local_file(
                    path.clone(),
                    prepared_media,
                    playlist_runtime,
                    renderer,
                );
            }
            Err(error) => {
                let safe_label = crate::playlist_runtime::safe_local_open_label(&path);
                warn!(error = %error, source = %safe_label, "Не удалось открыть файл");
                self.set_startup_error(format!(
                    "Ошибка открытия media-файла {safe_label}: {error}"
                ));
            }
        }
        self.mark_pending_worker_redraw();
    }

    /// Доставляет уже подготовленный локальный media в worker после async UI opening-а.
    pub(crate) fn load_prepared_local_file(
        &mut self,
        path: PathBuf,
        prepared_media: PreparedMedia,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) -> bool {
        let autoplay = self.committed_config_snapshot.autoplay_for_new_media();
        let source = ActiveMediaSource::LocalFile(path.clone());
        let prepared_input = PreparedSingleMediaOpen::new(
            prepared_media,
            source.clone(),
            crate::playlist_runtime::safe_local_open_label(&path),
        );
        if let Err(error) = self.install_prepared_media_strong(
            playlist_runtime,
            renderer,
            prepared_input,
            player_core::PlaybackIntent::from_autoplay(autoplay),
        ) {
            let safe_label = crate::playlist_runtime::safe_local_open_label(&path);
            warn!(error = %error, source = %safe_label, "Не удалось отправить подготовленный файл в worker");
            self.set_startup_error(format!(
                "Ошибка открытия media-файла {safe_label}: worker недоступен: {error}"
            ));
            return false;
        }

        self.record_installed_media_source(source);
        true
    }

    /// Загружает YouTube demuxer без долговременного database/cache слоя.
    pub fn load_youtube_demuxer(
        &mut self,
        source_locator: service_youtube::YoutubeMediaLocator,
        label: String,
        demuxer: Box<dyn symphonia_demux::Demuxer + Send>,
        selected_stream_identity: service_youtube::YoutubeSelectedStreamIdentity,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) -> bool {
        let autoplay = self.committed_config_snapshot.autoplay_for_new_media();
        let prepared_media = PreparedMedia::from_external_label(label, demuxer);
        let source = ActiveMediaSource::YouTubeUrl {
            source_locator: source_locator.clone(),
            selected_stream_identity,
        };
        let prepared_input = PreparedSingleMediaOpen::new(
            prepared_media,
            source.clone(),
            crate::media_open::SafeMediaLabel::from_service_safe_label(source_locator.safe_label()),
        );
        if let Err(error) = self.install_prepared_media_strong(
            playlist_runtime,
            renderer,
            prepared_input,
            player_core::PlaybackIntent::from_autoplay(autoplay),
        ) {
            warn!(error = %error, "Не удалось отправить YouTube demuxer в worker");
            self.set_startup_error(format!(
                "WorkerUnavailable: YouTube worker недоступен для {source_locator}: {error}"
            ));
            return false;
        }

        self.record_installed_media_source(source);
        true
    }

    /// Загружает подготовленный direct media URL и запоминает восстановимый source intent.
    pub fn load_prepared_direct_media(
        &mut self,
        source_locator: service_direct_media::DirectMediaUrl,
        label: String,
        prepared_media: PreparedMedia,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) -> bool {
        let autoplay = self.committed_config_snapshot.autoplay_for_new_media();
        let source = ActiveMediaSource::DirectMediaUrl(source_locator.clone());
        let prepared_input = PreparedSingleMediaOpen::new(
            prepared_media,
            source.clone(),
            crate::media_open::SafeMediaLabel::from_service_safe_label(source_locator.safe_label()),
        );
        if let Err(error) = self.install_prepared_media_strong(
            playlist_runtime,
            renderer,
            prepared_input,
            player_core::PlaybackIntent::from_autoplay(autoplay),
        ) {
            warn!(error = %error, label = %label, "Не удалось отправить внешний media source в worker");
            self.set_startup_error(format!(
                "WorkerUnavailable: direct media worker недоступен для {label}: {error}"
            ));
            return false;
        }

        self.record_installed_media_source(source);
        true
    }

    /// Возвращает восстановимый active source intent для controlled media rebuild.
    #[must_use]
    pub(crate) fn active_media_source(&self) -> Option<ActiveMediaSource> {
        self.active_media_source.clone()
    }

    pub(crate) fn remember_active_media_source(&mut self, source: ActiveMediaSource) {
        self.active_media_source = Some(source);
    }

    /// Публикует app observable state только после exact player Installed.
    pub(crate) fn record_installed_media_source(&mut self, source: ActiveMediaSource) {
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::MediaOpenBoundary);
        self.clear_startup_status();
        self.current_local_file = match &source {
            ActiveMediaSource::LocalFile(path) => Some(path.clone()),
            ActiveMediaSource::DirectMediaUrl(_) | ActiveMediaSource::YouTubeUrl { .. } => None,
        };
        self.remember_active_media_source(source);
        self.mark_pending_worker_redraw();
    }

    /// Восстанавливает controls только через exact request/instance boundaries после Installed.
    pub(crate) fn restore_playback_after_media_reconfigure(
        &mut self,
        snapshot: &PlayerSnapshot,
        installed: &InstalledSingleMediaOpen,
    ) -> Result<(), String> {
        let player_core::MediaInstallCompletion::Installed {
            media_instance_id,
            applied_intent,
            ..
        } = installed.completion
        else {
            return Err("media reconfigure completion was not Installed".to_string());
        };
        let desired_intent = playback_intent_from_snapshot(snapshot);
        if applied_intent != desired_intent {
            return Err(format!(
                "Installed applied unexpected playback intent: {applied_intent:?}"
            ));
        }

        self.player_worker
            .try_send_command(PlayerCommand::SetVolume(snapshot.volume))
            .map_err(|error| format!("volume restore dispatch failed: {error}"))?;
        if let Some(selected_quality) = snapshot
            .available_qualities
            .iter()
            .find(|quality| quality.selected)
        {
            self.player_worker
                .try_send_command(PlayerCommand::SelectQuality(QualitySelection::Specific(
                    selected_quality.id.clone(),
                )))
                .map_err(|error| format!("quality restore dispatch failed: {error}"))?;
        }

        let restore = player_core::InstalledMediaStateRestore {
            request_id: installed.player_request_id,
            media_instance_id,
            video_track: snapshot.selected_tracks.video_track.map_or(
                player_core::InstalledTrackRestore::KeepDefault,
                player_core::InstalledTrackRestore::Select,
            ),
            audio_track: snapshot.selected_tracks.audio_track.map_or(
                player_core::InstalledTrackRestore::KeepDefault,
                player_core::InstalledTrackRestore::Select,
            ),
            subtitle_track: snapshot.selected_tracks.subtitle_track.map_or(
                player_core::InstalledSubtitleRestore::KeepDefault,
                player_core::InstalledSubtitleRestore::Select,
            ),
            position: if snapshot.current_position > Duration::ZERO {
                player_core::InstalledPositionRestore::SeekTo(snapshot.current_position)
            } else {
                player_core::InstalledPositionRestore::KeepStart
            },
        };
        let restore_receipt = self
            .player_worker
            .restore_installed_media_state(restore)
            .map_err(|error| format!("exact position/track restore dispatch failed: {error}"))?;
        match restore_receipt
            .wait_for_outcome()
            .map_err(|error| format!("exact position/track restore outcome missing: {error}"))?
        {
            player_core::InstalledMediaStateRestoreOutcome::Applied {
                media_instance_id: applied_instance,
            } if applied_instance == media_instance_id => {}
            outcome => {
                return Err(format!(
                    "exact position/track restore was rejected: {outcome:?}"
                ));
            }
        }

        Ok(())
    }

    /// Возвращает `true`, пока shell ждёт file dialog или подготовку локального media.
    #[must_use]
    pub fn has_pending_local_file_open(&self) -> bool {
        self.local_file_open_job.is_some()
    }

    /// Передаёт renderer-bound local job process owner-у на время suspend.
    ///
    /// Suspend не является process shutdown: handle должен пережить уничтожение
    /// `AppState`, а результат будет применён уже к следующей renderer generation.
    pub(crate) fn take_local_file_open_job_for_suspend(&mut self) -> Option<LocalFileOpenJob> {
        self.local_file_open_job.take()
    }

    /// Возвращает сохранённый process owner-ом local job после resume.
    ///
    /// При нарушении single-job invariant ownership возвращается вызывающему коду,
    /// чтобы тот мог выполнить terminal shutdown без скрытого detach.
    pub(crate) fn restore_local_file_open_job_after_resume(
        &mut self,
        transferred_job: LocalFileOpenJob,
    ) -> LocalFileOpenRestoreOutcome {
        if self.local_file_open_job.is_some() {
            return LocalFileOpenRestoreOutcome::ExistingJob(Box::new(transferred_job));
        }
        self.local_file_open_job = Some(transferred_job);
        LocalFileOpenRestoreOutcome::Restored
    }

    /// Неблокирующе забирает события async открытия локального файла.
    pub fn poll_local_file_open_job(
        &mut self,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) -> bool {
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
            self.apply_local_file_open_result(result, playlist_runtime, renderer);
        }

        had_visible_mutation
    }

    /// Применяет финальный результат local open job-а к shell и worker boundary.
    pub(super) fn apply_local_file_open_result(
        &mut self,
        result: LocalFileOpenResult,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) {
        match result {
            LocalFileOpenResult::Cancelled => {
                self.startup_pending = None;
                self.mark_pending_worker_redraw();
            }
            LocalFileOpenResult::Selected { path } => {
                let intent = crate::playlist_runtime::InAppQueueReplacementIntent::local_file(path);
                match playlist_runtime.admit_in_app_queue_replacement(intent) {
                    Ok(crate::playlist_runtime::InAppQueueReplacementAdmission::StartNow(
                        admitted,
                    )) => self.start_admitted_queue_replacement(admitted),
                    Ok(crate::playlist_runtime::InAppQueueReplacementAdmission::AwaitingConfirmation) => {
                        self.startup_pending = None;
                        self.mark_pending_worker_redraw();
                    }
                    Err(error) => {
                        warn!(error = %error, "Local open admission отклонён до preparation");
                        self.set_startup_error(format!(
                            "Не удалось начать открытие media: {error}"
                        ));
                    }
                }
            }
            LocalFileOpenResult::Prepared {
                path,
                prepared_media,
            } => {
                self.load_prepared_local_file(path, prepared_media, playlist_runtime, renderer);
            }
            LocalFileOpenResult::PrepareFailed { path, error } => {
                let safe_label = crate::playlist_runtime::safe_local_open_label(&path);
                warn!(source = %safe_label, error = %error, "Не удалось подготовить локальный файл");
                self.set_startup_error(local_file_prepare_error_message(&path, &error));
            }
            LocalFileOpenResult::JobFailed { error } => {
                warn!(error = %error, "Local file open job завершился ошибкой");
                self.set_startup_error(format!("Ошибка открытия media-файла: {error}"));
            }
        }
    }

    /// Запускает нижний local preparation owner только после typed admission.
    fn start_admitted_queue_replacement(
        &mut self,
        admitted: crate::playlist_runtime::AdmittedQueueReplacementIntent,
    ) {
        let crate::playlist_runtime::AdmittedQueueReplacementIntent::LocalFile(local_open) =
            admitted
        else {
            // Production URL editor отсутствует: такой intent не может появиться из текущего UI.
            self.set_startup_error(
                "Внутренняя ошибка: URL open route ещё не подключён к in-app UI".to_string(),
            );
            return;
        };
        let path = local_open.into_path();
        let safe_label = crate::playlist_runtime::safe_local_open_label(&path);
        match LocalFileOpenJob::spawn_preparation(
            path,
            self.committed_config_snapshot.demux_config_for_open(),
            self.local_file_open_wake_port.clone(),
        ) {
            Ok(job) => {
                self.local_file_open_job = Some(job);
                self.set_startup_pending(format!("Подготовка media-файла: {safe_label}"));
            }
            Err(error) => {
                warn!(error = %error, source = %safe_label, "Не удалось запустить local preparation");
                self.set_startup_error(format!(
                    "Ошибка открытия media-файла {safe_label}: {error}"
                ));
            }
        }
    }

    /// Применяет typed Confirm/Cancel после egui closure, не сохраняя intent в `AppState`.
    pub(crate) fn apply_queue_replacement_confirmation_action(
        &mut self,
        action: crate::playlist_runtime::QueueReplacementConfirmationAction,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
    ) {
        match playlist_runtime.respond_to_queue_replacement_confirmation(action) {
            crate::playlist_runtime::QueueReplacementConfirmationOutcome::Confirmed(intent) => {
                self.start_admitted_queue_replacement(intent);
            }
            crate::playlist_runtime::QueueReplacementConfirmationOutcome::Cancelled => {
                self.mark_pending_worker_redraw();
            }
            crate::playlist_runtime::QueueReplacementConfirmationOutcome::Stale => {
                debug!("Stale queue replacement confirmation response проигнорирован");
            }
        }
    }
}

/// Переводит snapshot state в stable D52 intent без positional bool.
pub(crate) fn playback_intent_from_snapshot(
    snapshot: &PlayerSnapshot,
) -> player_core::PlaybackIntent {
    match snapshot.playback_state {
        PlaybackState::Playing
        | PlaybackState::Buffering
        | PlaybackState::Seeking
        | PlaybackState::Draining => player_core::PlaybackIntent::StartPlaying,
        PlaybackState::Scrubbing | PlaybackState::Paused => {
            player_core::PlaybackIntent::StartPaused
        }
        PlaybackState::Idle
        | PlaybackState::Opening
        | PlaybackState::Stopped
        | PlaybackState::Ended
        | PlaybackState::Failed => player_core::PlaybackIntent::StartPaused,
    }
}
