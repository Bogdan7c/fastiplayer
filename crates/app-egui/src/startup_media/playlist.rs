//! Startup/desktop association routing для локальных playlist документов.
//!
//! Extension выбирает только trusted import route. Authoritative content validation,
//! capacity policy, preview и ID allocation остаются у существующих playlist owners.

use super::StartupMediaController;
use super::orchestration::{StartupMediaPhase, StartupMediaTarget, StartupPendingInstall};
use crate::playlist_runtime::{PlaylistImportIntent, StartupPlaylistImportTerminal};

impl StartupMediaController {
    /// Продвигает trusted playlist flow и возвращает `Some`, пока он владеет startup turn.
    pub(super) fn drive_startup_playlist_import(
        &mut self,
        app_state: &mut crate::state::AppState,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
        structurally_superseded: bool,
    ) -> Option<bool> {
        if !self.startup_playlist_pending {
            return None;
        }
        if structurally_superseded {
            playlist_runtime.cancel_startup_playlist_import();
            self.startup_playlist_pending = false;
            self.orchestration.target = None;
            self.orchestration.phase = StartupMediaPhase::Idle;
            app_state.clear_startup_status();
            return Some(true);
        }

        let Some(terminal) = playlist_runtime.take_startup_playlist_import_terminal() else {
            if playlist_runtime
                .pending_playlist_import_preview()
                .is_some_and(|preview| preview.intent() == PlaylistImportIntent::StartupReplace)
            {
                // Preview/Continue — единственное partial decision; startup overlay не скрывает его.
                app_state.clear_startup_status();
                self.orchestration.phase = StartupMediaPhase::PreparedAwaitingAllocator;
            }
            return Some(false);
        };
        self.startup_playlist_pending = false;

        match terminal {
            StartupPlaylistImportTerminal::Committed(receipt) => {
                if !matches!(
                    self.orchestration.target.take(),
                    Some(StartupMediaTarget::CliReplacement)
                ) {
                    self.handle_install_failure(
                        "Startup playlist commit потерял CLI winner identity".to_owned(),
                        true,
                        app_state,
                    );
                    return Some(true);
                }
                let autoplay = self
                    .startup_config
                    .as_ref()
                    .is_some_and(|config| !config.player.start_paused);
                let install =
                    match playlist_runtime.plan_startup_playlist_first_install(receipt, autoplay) {
                        Ok(install) => install,
                        Err(error) => {
                            self.handle_install_failure(error.to_string(), true, app_state);
                            return Some(true);
                        }
                    };
                app_state.set_startup_pending(
                    "Открытие первого элемента startup playlist...".to_owned(),
                );
                match app_state.begin_startup_playlist_install(playlist_runtime, renderer, install)
                {
                    Ok(()) => {
                        self.startup_error = None;
                        playlist_runtime.begin_startup_action_retention();
                        self.orchestration.pending_install = Some(StartupPendingInstall {
                            is_cli: true,
                            local_discovery: None,
                            superseded: false,
                        });
                        self.orchestration.phase = StartupMediaPhase::Applying;
                    }
                    Err(error) => {
                        self.handle_install_failure(error.to_string(), true, app_state);
                    }
                }
            }
            StartupPlaylistImportTerminal::Empty => self.handle_preparation_failure(
                "Startup playlist не содержит accepted media entries".to_owned(),
                app_state,
                playlist_runtime,
            ),
            StartupPlaylistImportTerminal::Failed(error) => {
                self.handle_preparation_failure(error, app_state, playlist_runtime);
            }
            StartupPlaylistImportTerminal::Cancelled => self.handle_preparation_failure(
                "Startup playlist import отменён до queue commit".to_owned(),
                app_state,
                playlist_runtime,
            ),
        }
        Some(true)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::startup_media::is_recognized_startup_playlist_path;

    #[test]
    fn recognizes_all_startup_playlist_extensions_case_insensitively() {
        for path in ["list.m3u", "list.M3U8", "list.xSpF", "album.CUE"] {
            assert!(
                is_recognized_startup_playlist_path(Path::new(path)),
                "{path}"
            );
        }
    }

    #[test]
    fn ordinary_local_media_extension_stays_outside_playlist_route() {
        assert!(!is_recognized_startup_playlist_path(Path::new("movie.mkv")));
    }
}
