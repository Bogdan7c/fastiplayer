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
                    LocalFileOpenResult::PrepareFailed { error, .. }
                    | LocalFileOpenResult::JobFailed { error } => {
                        self.handle_preparation_failure(error, app_state, playlist_runtime);
                    }
                    LocalFileOpenResult::Cancelled => {
                        self.handle_preparation_failure(
                            "Startup local preparation отменена".to_owned(),
                            app_state,
                            playlist_runtime,
                        );
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
}
