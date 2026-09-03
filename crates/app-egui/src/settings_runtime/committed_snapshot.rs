use super::*;

/// Read-only snapshot committed config-а для слоёв, которые не должны владеть `AppConfig`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CommittedConfigSnapshot {
    /// Полный clone нужен как snapshot, но поле закрыто, чтобы не стать вторым owner-ом.
    config: AppConfig,
}

impl CommittedConfigSnapshot {
    /// Создаёт snapshot из authoritative committed config-а.
    #[must_use]
    pub(crate) fn from_config(config: &AppConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }

    /// Возвращает immutable config view для legacy boundaries, которые пока ждут `&AppConfig`.
    #[must_use]
    pub(crate) fn as_config(&self) -> &AppConfig {
        &self.config
    }

    /// Autoplay policy для нового media open-а.
    #[must_use]
    pub(crate) fn autoplay_for_new_media(&self) -> bool {
        !self.config.player.start_paused
    }

    /// Demux settings, которые local open job должен захватить в момент запуска.
    #[must_use]
    pub(crate) fn demux_config_for_open(&self) -> PlayerDemuxConfig {
        self.config.player.demux
    }

    /// Committed intent выбора video backend-а для app-owned pipeline selector-а.
    #[must_use]
    pub(crate) fn video_backend_preference(&self) -> VideoBackendPreference {
        self.config.video.preferred_backend
    }

    /// Process policy для новых фоновых YtDlp playlist metadata jobs.
    #[must_use]
    pub(crate) fn yt_dlp_metadata_config(&self) -> YtDlpConfig {
        self.config.yt_dlp.clone()
    }

    /// Default volume policy для startup/new media и mute-toggle restore.
    #[must_use]
    pub(crate) fn default_volume_for_new_media(&self) -> f32 {
        self.config.audio.volume as f32
    }

    /// Малый relative seek step для следующего hotkey event-а.
    #[must_use]
    pub(crate) fn hotkey_small_seek_step(&self) -> Duration {
        Duration::from_secs(self.config.player.seek.hotkey_small_step_secs)
    }

    /// Большой relative seek step для следующего hotkey event-а.
    #[must_use]
    pub(crate) fn hotkey_large_seek_step(&self) -> Duration {
        Duration::from_secs(self.config.player.seek.hotkey_large_step_secs)
    }

    /// Stable skin id из последнего committed config-а.
    #[must_use]
    pub(crate) fn ui_skin(&self) -> &str {
        &self.config.ui.skin
    }

    /// Visibility flag для telemetry panel из последнего committed config-а.
    #[must_use]
    pub(crate) fn show_telemetry(&self) -> bool {
        self.config.ui.show_telemetry
    }

    /// Включены ли live preview updates во время timeline drag.
    #[must_use]
    pub(crate) fn live_scrub_enabled(&self) -> bool {
        self.config.frame_server.live_scrub_enabled
    }

    /// Decode launch policy для следующего timeline live scrub gesture.
    #[must_use]
    pub(crate) fn live_scrub_decode_mode(&self) -> FrameServerLiveScrubDecodeModeConfig {
        self.config.frame_server.live_scrub_decode_mode
    }

    /// Max decode start rate для `throttled_latest` live scrub gesture.
    #[must_use]
    pub(crate) fn live_scrub_max_hz(&self) -> u16 {
        self.config.frame_server.live_scrub_max_hz
    }

    /// Длительность анимации выезда settings sidebar в секундах; `0` — без анимации.
    #[must_use]
    pub(crate) fn sidebar_slide_duration_seconds(&self) -> f32 {
        if self.config.ui.animations.reduced_motion {
            0.0
        } else {
            f32::from(self.config.ui.animations.sidebar_slide_duration_ms) / 1000.0
        }
    }

    /// Требует ли UI мгновенных layout-переходов и отключённого scale/pulse.
    #[must_use]
    pub(crate) fn reduced_motion(&self) -> bool {
        self.config.ui.animations.reduced_motion
    }

    /// Запоминаемая fully-open ширина общего sidebar host в egui points.
    #[must_use]
    pub(crate) fn sidebar_width_points(&self) -> u16 {
        self.config.ui.sidebar.width_points
    }

    /// Высота кастомного titlebar в egui points; config хранит те же логические UI px.
    #[must_use]
    pub(crate) fn titlebar_height_points(&self) -> f32 {
        f32::from(self.config.ui.window.titlebar_height_px)
    }

    /// Committed радиус контура окна в egui points; draft сюда не попадает до Apply/OK.
    #[must_use]
    pub(crate) fn window_corner_radius_points(&self) -> f32 {
        f32::from(self.config.ui.window.corner_radius_px)
    }
}

/// Renderer settings, которые применяются один раз при создании runtime renderer-а.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InitialRenderSettings {
    /// Color pipeline snapshot из committed config-а.
    pub(crate) color_pipeline: ColorPipelineSettings,

    /// HDR-to-SDR snapshot из committed config-а.
    pub(crate) hdr_to_sdr: HdrToSdrSettings,
}

impl SettingsRuntime {
    /// Возвращает initial render settings без передачи `AppConfig` renderer-у.
    pub(crate) fn initial_render_settings(&self) -> SettingsResult<InitialRenderSettings> {
        Ok(InitialRenderSettings {
            color_pipeline: color_pipeline_settings_from_config(self.committed_config())
                .map_err(|error| settings_core::SettingsError::access_failed(error.to_string()))?,
            hdr_to_sdr: hdr_to_sdr_settings_from_config(self.committed_config()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduced_motion_forces_instant_sidebar_without_losing_saved_duration() {
        let default_snapshot = CommittedConfigSnapshot::from_config(&AppConfig::default());
        assert!(default_snapshot.reduced_motion());
        assert_eq!(default_snapshot.sidebar_slide_duration_seconds(), 0.0);

        let mut animated_config = AppConfig::default();
        animated_config.ui.animations.reduced_motion = false;
        let animated_snapshot = CommittedConfigSnapshot::from_config(&animated_config);
        assert!(!animated_snapshot.reduced_motion());
        assert_eq!(animated_snapshot.sidebar_slide_duration_seconds(), 0.5);
    }
}
