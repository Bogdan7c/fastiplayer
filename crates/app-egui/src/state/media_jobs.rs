use super::present_frame_cache::CachedPresentFrameDiscardReason;
use super::*;

/// Сохраняет прежний state-module import path до миграции callsites в Session 10D.
pub(crate) use crate::media_open::ActiveMediaSource;

/// App cleanup сохраняет race distinction между старым reset и новым Installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExactMediaResetCleanup {
    Cleared,
    SupersededByNewSnapshot,
}

/// Pure correlation не позволяет старому receipt очистить app state нового instance.
pub(super) fn classify_exact_media_reset_cleanup(
    current_media_instance_id: Option<player_core::MediaInstanceId>,
    reset_media_instance_id: player_core::MediaInstanceId,
) -> ExactMediaResetCleanup {
    match current_media_instance_id {
        Some(current) if current != reset_media_instance_id => {
            ExactMediaResetCleanup::SupersededByNewSnapshot
        }
        Some(_) | None => ExactMediaResetCleanup::Cleared,
    }
}

impl AppState {
    /// Возвращает cloneable ordered player control stream для process-lifetime owner-а.
    pub(crate) fn player_command_sender(&self) -> player_core::PlayerCommandSender {
        self.player_worker.command_sender()
    }

    /// Возвращает восстановимый active source intent для controlled media rebuild.
    #[must_use]
    pub(crate) fn active_media_source(&self) -> Option<ActiveMediaSource> {
        self.active_media_source.clone()
    }

    pub(crate) fn remember_active_media_source(&mut self, source: ActiveMediaSource) {
        self.active_media_source = Some(source);
    }

    /// Публикует exact Installed вместе с runtime-only VOD recovery attachment-ом.
    pub(crate) fn record_installed_media(&mut self, installed: &InstalledSingleMediaOpen) {
        self.record_installed_media_observables(
            installed.source.clone(),
            super::subtitles::installed_media_instance_id(installed),
        );
        self.bind_installed_vod_endpoint_recovery(installed);
    }

    /// Suspend/resume публикует source и exact runtime attachment после своего Installed receipt.
    pub(crate) fn record_resumed_installed_media(
        &mut self,
        source: ActiveMediaSource,
        media_instance_id: player_core::MediaInstanceId,
        attachment: Option<
            media_source_open::web_media_vod_recovery::VodEndpointRecoveryAttachment,
        >,
    ) {
        self.record_installed_media_observables(source.clone(), Some(media_instance_id));
        self.bind_resumed_vod_endpoint_recovery(media_instance_id, source, attachment);
    }

    /// Общие UI projections не знают устройство source lifecycle attachment-а.
    ///
    /// Здесь же стартует загрузка субтитров: это общая точка всех путей установки media
    /// (Open, drag&drop, playlist next, пересборка настроек, resume).
    fn record_installed_media_observables(
        &mut self,
        source: ActiveMediaSource,
        media_instance_id: Option<player_core::MediaInstanceId>,
    ) {
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::MediaOpenBoundary);
        self.clear_startup_status();
        self.url_sidebar_controller.record_installed_source();
        self.current_local_file = match source.physical_source() {
            ActiveMediaSource::LocalFile(path) => Some(path.clone()),
            ActiveMediaSource::Web(_) | ActiveMediaSource::BrowserStream => None,
            ActiveMediaSource::PlaybackWindow { .. } => {
                unreachable!("physical_source removes playback-window wrappers")
            }
        };
        self.start_subtitles_for_installed_media(media_instance_id, &source);
        self.remember_active_media_source(source);
        self.mark_pending_worker_redraw();
    }

    /// Exact release удаляет app-side source projection уже отсутствующего player media.
    pub(crate) fn clear_released_installed_media_source(
        &mut self,
        released_media_instance_id: player_core::MediaInstanceId,
    ) {
        if classify_exact_media_reset_cleanup(
            self.last_player_snapshot.media_instance_id,
            released_media_instance_id,
        ) == ExactMediaResetCleanup::SupersededByNewSnapshot
        {
            return;
        }
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::MediaOpenBoundary);
        self.current_local_file = None;
        self.active_media_source = None;
        self.clear_subtitles_for_released_media();
        self.clear_installed_vod_endpoint_recovery();
        // Редкий путь: если снимок ещё делит `Arc` с worker handle или контекстом кадра,
        // `make_mut` делает копию, и общий снимок worker-а остаётся нетронутым.
        let cached_snapshot = Arc::make_mut(&mut self.last_player_snapshot);
        cached_snapshot.media_instance_id = None;
        cached_snapshot.playback_state = PlaybackState::Stopped;
        cached_snapshot.source_label = None;
        cached_snapshot.media_title = None;
        cached_snapshot.current_video_frame = None;
        cached_snapshot.clear_timeline();
        self.mark_pending_worker_redraw();
    }

    /// Очищает app-owned media только если snapshot ещё не принадлежит новому instance.
    pub(super) fn record_cleared_media_after_exact_reset(
        &mut self,
        reset_media_instance_id: player_core::MediaInstanceId,
    ) -> ExactMediaResetCleanup {
        if classify_exact_media_reset_cleanup(
            self.last_player_snapshot.media_instance_id,
            reset_media_instance_id,
        ) == ExactMediaResetCleanup::SupersededByNewSnapshot
        {
            return ExactMediaResetCleanup::SupersededByNewSnapshot;
        }
        self.clear_cached_present_frame(CachedPresentFrameDiscardReason::PlaylistClearReset);
        self.current_local_file = None;
        self.active_media_source = None;
        self.clear_subtitles_for_released_media();
        // Редкий путь: если снимок ещё делит `Arc` с worker handle или контекстом кадра,
        // `make_mut` делает копию, и общий снимок worker-а остаётся нетронутым.
        let cached_snapshot = Arc::make_mut(&mut self.last_player_snapshot);
        cached_snapshot.media_instance_id = None;
        cached_snapshot.playback_state = PlaybackState::Stopped;
        cached_snapshot.source_label = None;
        cached_snapshot.media_title = None;
        cached_snapshot.current_video_frame = None;
        cached_snapshot.clear_timeline();
        self.mark_pending_worker_redraw();
        ExactMediaResetCleanup::Cleared
    }

    /// Восстанавливает controls только через exact request/instance boundaries после Installed.
    ///
    /// Блокирующий вариант (откат настроек); неблокирующий путь пересборки использует
    /// `dispatch_playback_restore` + опрос receipt-а (`state/settings_media_rebuild`).
    pub(crate) fn restore_playback_after_media_reconfigure(
        &mut self,
        snapshot: &PlayerSnapshot,
        installed: &InstalledSingleMediaOpen,
    ) -> Result<(), String> {
        self.dispatch_playback_restore(snapshot, installed)?.wait()
    }

    /// Возвращает `true`, пока shell ждёт file dialog, подготовку или установку локального media.
    ///
    /// Установка входит сюда, чтобы повторный Open получил «Файл ещё открывается» сразу,
    /// а не после выбора файла и подтверждения замены очереди.
    #[must_use]
    pub fn has_pending_local_file_open(&self) -> bool {
        self.local_file_open_job.is_some() || self.is_local_open_installing()
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

    /// Неблокирующе продвигает открытие локального файла: установку из Open и async job.
    pub fn poll_local_file_open_job(
        &mut self,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &mut dyn render_shell_api::RenderShell,
    ) -> bool {
        // Установка опрашивается первой: её terminal освобождает общий слот для того,
        // что могло ждать за ней (например, Next, отменивший открытие).
        let install_changed =
            self.poll_local_open_install(playlist_runtime, renderer.video_frames());
        let job_changed = self.drain_local_file_open_job(playlist_runtime, renderer);
        // После опроса: если открытие закончилось без установки, файлы субтитров из того же
        // броска не должны дожидаться чужого видео.
        self.settle_dropped_subtitles(playlist_runtime);
        install_changed || job_changed
    }

    /// Забирает события async job-а (диалог выбора и подготовка файла).
    fn drain_local_file_open_job(
        &mut self,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &mut dyn render_shell_api::RenderShell,
    ) -> bool {
        let Some(job) = self.local_file_open_job.as_mut() else {
            self.local_file_open_wake_port
                .acknowledge_abandoned_mailbox();
            return false;
        };
        let drain = job.drain();
        let had_visible_mutation = drain.has_payload();

        if let Some(path) = drain.preparing_path {
            self.set_startup_pending(local_open_preparing_message(&path));
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
        renderer: &mut dyn render_shell_api::RenderShell,
    ) {
        match result {
            LocalFileOpenResult::Cancelled => {
                self.startup_pending = None;
                self.mark_pending_worker_redraw();
            }
            LocalFileOpenResult::Selected { path } => {
                self.open_selected_local_file(path, playlist_runtime, renderer);
            }
            LocalFileOpenResult::Prepared { prepared } => {
                // Только начинает установку: итог заберёт `poll_local_open_install`.
                self.begin_local_open_install(*prepared, playlist_runtime, renderer);
            }
            LocalFileOpenResult::PrepareFailed { path, reason } => {
                // Техническая цепочка уже записана в лог worker-ом подготовки.
                self.report_local_open_failure(&path, reason);
            }
            LocalFileOpenResult::JobFailed { error } => {
                warn!(error = %error, "Local file open job ended up with an error");
                self.set_startup_error(format!("Opening the media file error: {error}"));
            }
        }
    }

    /// Открывает выбранный локальный файл: общий путь кнопки Open и drag & drop одного файла.
    ///
    /// Сначала проверяется, не лежит ли файл уже в очереди (тогда играем эту строку без
    /// замены очереди), иначе идёт admission замены очереди с подтверждением.
    pub(crate) fn open_selected_local_file(
        &mut self,
        path: std::path::PathBuf,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &mut dyn render_shell_api::RenderShell,
    ) {
        if let crate::playlist_runtime::LocalFileSelectionDisposition::PlayCommittedItem {
            item_id,
        } = playlist_runtime.classify_in_app_local_file_selection(&path)
        {
            let outcome = playlist_runtime.play_playlist_row(item_id);
            if !crate::transport_runtime::apply_playlist_row_play(
                self,
                playlist_runtime,
                renderer,
                outcome,
            ) {
                self.set_startup_error(
                    "Cannot open the selected file from the current queue".to_string(),
                );
            }
            return;
        }
        let intent = crate::playlist_runtime::InAppQueueReplacementIntent::local_file(path);
        self.apply_in_app_queue_replacement_admission(
            playlist_runtime.admit_in_app_queue_replacement(intent),
            playlist_runtime,
            renderer,
        );
    }

    /// Применяет итог admission замены очереди: старт сразу, ожидание Confirm или ошибка.
    pub(crate) fn apply_in_app_queue_replacement_admission(
        &mut self,
        admission: Result<
            crate::playlist_runtime::InAppQueueReplacementAdmission,
            crate::playlist_runtime::QueueReplacementAdmissionError,
        >,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &mut dyn render_shell_api::RenderShell,
    ) {
        match admission {
            Ok(crate::playlist_runtime::InAppQueueReplacementAdmission::StartNow(admitted)) => {
                self.start_admitted_queue_replacement(admitted, playlist_runtime, renderer);
            }
            Ok(crate::playlist_runtime::InAppQueueReplacementAdmission::AwaitingConfirmation) => {
                self.startup_pending = None;
                self.mark_pending_worker_redraw();
            }
            Err(error) => {
                warn!(error = %error, "Local open admission inclined preparation");
                self.set_startup_error(format!("Failed to open media: {error}"));
            }
        }
    }

    /// Запускает нижний local preparation owner только после typed admission.
    pub(crate) fn start_admitted_queue_replacement(
        &mut self,
        admitted: crate::playlist_runtime::AdmittedQueueReplacementIntent,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &mut dyn render_shell_api::RenderShell,
    ) {
        let local_open = match admitted {
            crate::playlist_runtime::AdmittedQueueReplacementIntent::LocalFile(local_open) => {
                local_open
            }
            crate::playlist_runtime::AdmittedQueueReplacementIntent::LocalFiles(local_files) => {
                let truncation = local_files.truncation();
                self.replace_queue_with_admitted_local_files(
                    local_files.into_paths(),
                    truncation,
                    playlist_runtime,
                    renderer,
                );
                return;
            }
            crate::playlist_runtime::AdmittedQueueReplacementIntent::ServiceUrl(url_open) => {
                // Ссылка, брошенная на видео: новая очередь из одной строки + Row Play.
                self.replace_queue_with_admitted_service_url(
                    &url_open.into_locator(),
                    playlist_runtime,
                    renderer,
                );
                return;
            }
            crate::playlist_runtime::AdmittedQueueReplacementIntent::ResolvedUrlCollection(
                collection,
            ) => {
                // Коллекция по ссылке, брошенной на видео: новая очередь из её записей + Row Play.
                self.replace_queue_with_admitted_resolved_url_collection(
                    collection,
                    playlist_runtime,
                    renderer,
                );
                return;
            }
        };
        let path = local_open.into_path();
        let safe_label = crate::playlist_runtime::safe_local_open_label(&path);
        let preparing_message = local_open_preparing_message(&path);
        match LocalFileOpenJob::spawn_preparation(
            path,
            self.committed_config_snapshot.demux_config_for_open(),
            self.committed_config_snapshot.attachment_limits_for_open(),
            self.local_file_open_wake_port.clone(),
        ) {
            Ok(job) => {
                self.local_file_open_job = Some(job);
                self.set_startup_pending(preparing_message);
            }
            Err(error) => {
                warn!(error = %error, "Could not start local preparation");
                self.set_startup_error(format!(
                    "Opening error of media file {safe_label}: {error}"
                ));
            }
        }
    }

    /// Применяет typed Confirm/Cancel после egui closure, не сохраняя intent в `AppState`.
    pub(crate) fn apply_playlist_confirmation_action(
        &mut self,
        action: crate::playlist_runtime::PlaylistConfirmationAction,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &mut dyn render_shell_api::RenderShell,
    ) {
        let outcome = playlist_runtime.respond_to_playlist_confirmation(action);
        playlist_runtime.finish_url_draft_after_confirmation(&outcome);
        match outcome {
            crate::playlist_runtime::PlaylistConfirmationApplyOutcome::QueueReplacementConfirmed(intent) => {
                self.start_admitted_queue_replacement(intent, playlist_runtime, renderer);
            }
            crate::playlist_runtime::PlaylistConfirmationApplyOutcome::Cancelled
            | crate::playlist_runtime::PlaylistConfirmationApplyOutcome::Import(_)
            | crate::playlist_runtime::PlaylistConfirmationApplyOutcome::ExportWriterStarted
            | crate::playlist_runtime::PlaylistConfirmationApplyOutcome::UrlAppended
            | crate::playlist_runtime::PlaylistConfirmationApplyOutcome::UrlNoCapacity
            | crate::playlist_runtime::PlaylistConfirmationApplyOutcome::DeferredUntilStartupInstallResolution
            | crate::playlist_runtime::PlaylistConfirmationApplyOutcome::CommitRejected => {
                self.mark_pending_worker_redraw();
            }
            crate::playlist_runtime::PlaylistConfirmationApplyOutcome::Stale => {
                debug!("Stale playlist confirmation response ignored");
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
