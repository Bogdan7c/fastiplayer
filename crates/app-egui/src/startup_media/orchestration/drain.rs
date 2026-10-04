//! Сбор завершившихся startup jobs и передача единственного winner-а orchestration owner-у.

use crate::local_file_open::LocalFileOpenResult;

use super::*;

impl StartupMediaController {
    pub(super) fn drain_preparation_jobs(
        &mut self,
        app_state: &mut crate::state::AppState,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) -> bool {
        let mut changed = false;
        if let Some(job) = self.local_startup_job.as_mut() {
            let drain = job.drain();
            changed |= drain.has_payload();
            if let Some(completion) = drain.completion {
                self.local_startup_job = None;
                match completion {
                    LocalFileOpenResult::Prepared { prepared } => {
                        self.hold_prepared(PreparedStartupMedia::Local(prepared), playlist_runtime);
                    }
                    LocalFileOpenResult::PrepareFailed { path, reason } => {
                        self.handle_local_preparation_failure(
                            &path,
                            reason,
                            app_state,
                            playlist_runtime,
                        );
                    }
                    LocalFileOpenResult::JobFailed { error } => {
                        self.handle_preparation_failure(error, app_state, playlist_runtime);
                    }
                    LocalFileOpenResult::Cancelled => {
                        self.finish_cancelled_local_preparation();
                    }
                    LocalFileOpenResult::Selected { .. } => {
                        self.handle_preparation_failure(
                            "Startup local owner получил неожиданный picker result".to_owned(),
                            app_state,
                            playlist_runtime,
                        );
                    }
                }
            }
        }

        if let Some(job) = self.yt_dlp_startup_job.as_mut()
            && let Some(result) = job.try_take_result()
        {
            let source_locator = job.source_locator.clone();
            self.yt_dlp_startup_job = None;
            changed = true;
            match result {
                Ok(prepared) => self.hold_prepared(
                    PreparedStartupMedia::Extractor {
                        source_locator,
                        prepared: Box::new(prepared),
                    },
                    playlist_runtime,
                ),
                Err(error) => {
                    self.handle_preparation_failure(error, app_state, playlist_runtime);
                }
            }
        }

        if let Some(job) = self.direct_media_startup_job.as_mut()
            && let Some(result) = job.try_take_result()
        {
            let source_locator = job.source_locator.clone();
            self.direct_media_startup_job = None;
            changed = true;
            match result {
                Ok(opened_media) => self.hold_prepared(
                    web_preparation::compose_direct_startup_media(source_locator, opened_media),
                    playlist_runtime,
                ),
                Err(error) => {
                    self.handle_preparation_failure(error, app_state, playlist_runtime);
                }
            }
        }

        if let Some(job) = self.native_hls_startup_job.as_mut()
            && let Some(result) = job.try_take_result()
        {
            self.native_hls_startup_job = None;
            changed = true;
            match result {
                Ok(prepared) => self.hold_prepared(prepared, playlist_runtime),
                Err(error) => {
                    self.handle_preparation_failure(error, app_state, playlist_runtime);
                }
            }
        }

        if let Some(job) = self.native_dash_startup_job.as_mut()
            && let Some(result) = job.try_take_result()
        {
            self.native_dash_startup_job = None;
            changed = true;
            match result {
                Ok(prepared) => self.hold_prepared(prepared, playlist_runtime),
                Err(error) => {
                    self.handle_preparation_failure(error, app_state, playlist_runtime);
                }
            }
        }

        if let Some(job) = self.native_hds_startup_job.as_mut()
            && let Some(result) = job.try_take_result()
        {
            self.native_hds_startup_job = None;
            changed = true;
            match result {
                Ok(prepared) => self.hold_prepared(prepared, playlist_runtime),
                Err(error) => {
                    self.handle_preparation_failure(error, app_state, playlist_runtime);
                }
            }
        }

        if let Some(job) = self.native_smooth_startup_job.as_mut()
            && let Some(result) = job.try_take_result()
        {
            self.native_smooth_startup_job = None;
            changed = true;
            match result {
                Ok(prepared) => self.hold_prepared(prepared, playlist_runtime),
                Err(error) => {
                    self.handle_preparation_failure(error, app_state, playlist_runtime);
                }
            }
        }

        if playlist_runtime.allocator_load_gate_is_open() && self.orchestration.prepared.is_some() {
            changed |= self.begin_prepared_winner(app_state, playlist_runtime, renderer);
        }
        changed
    }

    /// Стартовая ошибка локального файла: тот же текст, что и у кнопки Open.
    ///
    /// В лог идёт только типизированная причина: текст для пользователя содержит имя
    /// файла, а имя в лог не пишем. Строка restored-элемента получает короткую причину
    /// («Файл не найден») — имя файла строка очереди и так показывает.
    fn handle_local_preparation_failure(
        &mut self,
        path: &std::path::Path,
        reason: crate::media_open::LocalOpenFailureReason,
        app_state: &mut crate::state::AppState,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
    ) {
        tracing::warn!(reason = ?reason, "Startup local media preparation failed");
        let user_message = crate::local_open_message::local_open_failure_message(path, reason);
        let row_summary = Arc::<str>::from(
            crate::local_open_message::local_open_failure_row_summary(reason),
        );
        self.publish_preparation_failure(user_message, row_summary, app_state, playlist_runtime);
    }

    /// Отмена стартовой подготовки локального файла — не ошибка.
    ///
    /// Раньше отмена шла через `handle_preparation_failure`: пользователь видел текст
    /// «Startup local preparation отменена», а отменённый элемент сохранённой очереди
    /// ошибочно помечался «недоступным» и запускалась попытка следующего. Теперь цель
    /// просто завершается: без сообщения, без бейджа и без перехода к следующему элементу.
    /// Порядок «забрать target → preparation_failed()» тот же, что и у ошибки, поэтому
    /// фазы orchestration и признак CLI-сбоя ведут себя одинаково.
    fn finish_cancelled_local_preparation(&mut self) {
        let cancelled_target = self.orchestration.target.take();
        self.orchestration.preparation_failed();
        tracing::debug!(
            restored_current = matches!(
                cancelled_target,
                Some(StartupMediaTarget::RestoredCurrent(_))
            ),
            "Стартовая подготовка локального файла отменена"
        );
    }
}
