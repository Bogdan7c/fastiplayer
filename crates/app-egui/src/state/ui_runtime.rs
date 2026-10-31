use super::telemetry_panel::TelemetryPanelState;
use super::*;
use crate::fullscreen_chrome::ChromeHoldObservation;
use crate::state::subtitles::BottomControlsObstruction;
use crate::ui::subtitle_appearance_edit::SubtitleAppearanceEditInput;
use crate::ui::video_area_input::{self, VideoAreaMode};
use crate::ui::{control_action_coalescing, fullscreen_chrome_panels, global_hotkeys};
use tracing::warn;

/// Возвращает player command для playback-rate UI intent-а или `None` для silent skip/no-op.
pub(super) fn playback_rate_command_from_action(
    player_snapshot: &PlayerSnapshot,
    action: &ControlAction,
) -> Option<PlayerCommand> {
    if !playback_rate_ui_accepts_state(player_snapshot.playback_state) {
        return None;
    }

    let target_rate = match action {
        ControlAction::AdjustPlaybackRateSteps(step_count) => {
            playback_rate_from_step_count(player_snapshot.playback_rate, *step_count)?
        }
        ControlAction::ResetPlaybackRate => {
            if player_snapshot.playback_rate == PlaybackRate::NORMAL {
                return None;
            }

            PlaybackRate::NORMAL
        }
        _ => return None,
    };

    if target_rate == player_snapshot.playback_rate {
        return None;
    }

    Some(PlayerCommand::SetPlaybackRate(target_rate))
}

/// S38 разрешает только стабильные public states, а не широкий active-playback predicate.
const fn playback_rate_ui_accepts_state(playback_state: PlaybackState) -> bool {
    matches!(
        playback_state,
        PlaybackState::Playing | PlaybackState::Paused
    )
}

/// Преобразует UI-шаги в typed `PlaybackRate`, сохраняя 0.10x сетку и public clamp.
fn playback_rate_from_step_count(
    current_rate: PlaybackRate,
    step_count: i32,
) -> Option<PlaybackRate> {
    if step_count == 0 {
        return None;
    }

    let requested_rate =
        current_rate.as_f32() + step_count as f32 * player_controls::PLAYBACK_RATE_STEP_X;
    let rounded_rate = (requested_rate * 100.0).round() / 100.0;
    let incremental_rate = skip_normal_incremental_rate(current_rate, rounded_rate, step_count);
    let clamped_rate =
        incremental_rate.clamp(PlaybackRate::MIN.as_f32(), PlaybackRate::MAX.as_f32());

    match PlaybackRate::new(clamped_rate) {
        Ok(playback_rate) => Some(playback_rate),
        Err(error) => {
            warn!(error = %error, clamped_rate, "UI formed an incorrect playback speed");
            None
        }
    }
}

/// Исключает `1x` из wheel/`+`/`-` сетки, сохраняя его только для explicit reset intent.
fn skip_normal_incremental_rate(
    current_rate: PlaybackRate,
    rounded_rate: f32,
    step_count: i32,
) -> f32 {
    // Из исходного 1x обычный первый шаг должен по-прежнему дать 0.9x или 1.1x.
    if current_rate == PlaybackRate::NORMAL {
        return rounded_rate;
    }
    // После округления NORMAL сравнивается точно: 1.0 представляется без float-погрешности.
    if rounded_rate != PlaybackRate::NORMAL.as_f32() {
        return rounded_rate;
    }
    // Landing ровно на 1x переносится ещё на один неизменный 0.10x шаг по направлению жеста.
    rounded_rate + step_count.signum() as f32 * player_controls::PLAYBACK_RATE_STEP_X
}

impl AppState {
    /// Рендерит egui UI поверх видео.
    ///
    /// UI читает только `PlayerSnapshot`, а действия после egui closure отправляет worker-у.
    // TRACE: span на каждом полном проходе UI не нужен обычному логу (cpu-parity сессия 05).
    #[instrument(level = "trace", skip_all)]
    pub fn render_ui(
        &mut self,
        window: &Window,
        egui_input: egui::RawInput,
        frame_context: &AppFrameContext,
        settings_ui_model: &SettingsUiModel,
        playlist_models: PlaylistUiFrameModels<'_>,
    ) -> RenderedAppUi {
        let render_ui_started_at = Instant::now();
        let pre_ui_setup_started_at = Instant::now();
        let player_snapshot = frame_context.player_snapshot();
        let is_playing = player_snapshot.playback_state == PlaybackState::Playing;
        let timeline_inline_status = self.timeline_inline_status_message(Instant::now());

        if is_playing {
            self.next_frame();
        }

        let telemetry = Arc::clone(&self.telemetry);
        let start_time = self.start_time;
        let frame_duration_estimate_ms =
            player_snapshot.video_frame_duration_estimate.as_secs_f64() * 1000.0;
        // Уведомления: фатальная ошибка, прогресс и toast-ы приходят от единого владельца.
        let notifications_frame = self.notifications_frame(player_snapshot, Instant::now());
        let mut notification_output = crate::ui::notifications::NotificationUiOutput::default();
        let subtitle_menu = self.subtitle_menu_view();
        let audio_menu = self.audio_track_menu_view(player_snapshot);
        let render_diagnostics = frame_context.render_diagnostics();
        let selected_skin = skin::skin_from_config(self.committed_config_snapshot.ui_skin())
            .unwrap_or_else(|| {
                warn!(
                    skin = %self.committed_config_snapshot.ui_skin(),
                    "Config validation should reject unknown UI skin; use minimal"
                );
                skin::MinimalSkin
            });
        let controls_style = selected_skin.controls_style();
        // Один typed grid contract обслуживает titlebar и движущийся sidebar header.
        let window_chrome_edge_alignment =
            window_chrome::WindowChromeEdgeAlignment::from_controls_style(controls_style);
        let animation_state = AnimationState::from_timeline(&player_snapshot.timeline);
        // Затемнение stale video frame является underlay над видео, а не overlay
        // над chrome. Цвет вычисляется до egui closure, чтобы внутри pass-а первым
        // paint primitive-ом положить его под sidebar, transport и telemetry.
        let stale_video_dim_color = selected_skin.stale_frame_dim_color(animation_state);
        // Позиция анимации продвинута раньше в prepare_ui_frame; здесь только чтение.
        let sidebar_slide_progress = self.sidebar_controller.open_progress();
        let chrome_presentation = self.fullscreen_chrome.presentation();
        let show_telemetry = self.committed_config_snapshot.show_telemetry();
        let titlebar_height_points = self.committed_config_snapshot.titlebar_height_points();
        let window_is_maximized = window.is_maximized();
        let window_is_fullscreen = window.fullscreen().is_some();
        let mut control_actions = Vec::new();
        let mut settings_actions = Vec::new();
        // Режим правки субтитров на видео: снимок сессии, токены skin-а и собранные намерения.
        let subtitle_edit_view = self.subtitle_appearance_edit_view();
        let subtitle_edit_style = selected_skin.subtitle_appearance_edit_style();
        let settings_host_facts = self.settings_host_facts();
        let mut subtitle_edit_actions = Vec::new();
        let mut sidebar_width_change = None;
        let mut sidebar_close_requested = false;
        let mut window_chrome_actions = Vec::new();
        let mut playlist_confirmation_action = None;
        let playlist_view_model = self.playlist_view_model();
        // Sidebar получает один согласованный active/catalog snapshot текущего frame-а.
        // Visual widget не пересчитывает catalog и не отделяет fallback notice
        // от source projection, с которым он был опубликован.
        let url_sidebar_model =
            self.url_sidebar_model(player_snapshot, playlist_view_model.as_ref());
        let playlist_runtime_binding = self.playlist_runtime_binding();
        let mut playlist_ui_state = std::mem::take(&mut self.playlist_ui_state);
        let mut playlist_ui_output = crate::ui::playlist::PlaylistUiOutput::default();
        let mut url_sidebar_action = None;
        let mut timeline_ui_state = std::mem::take(&mut self.timeline_ui_state);
        let pre_ui_setup_elapsed = pre_ui_setup_started_at.elapsed();
        let mut telemetry_panel_cache_elapsed = Duration::ZERO;
        let telemetry_panel_rows = if show_telemetry {
            let telemetry_panel_cache_started_at = Instant::now();
            let panel_rows = self.telemetry_panel_cache.rows_for_frame(
                Instant::now(),
                TelemetryPanelState {
                    player_snapshot,
                    frame_counters: frame_context.frame_counters(),
                    telemetry: &telemetry,
                    render_diagnostics,
                    timeline_ui_state: &timeline_ui_state,
                    renderer_report: self.renderer_report.as_ref(),
                    start_time,
                    frame_duration_estimate_ms,
                },
            );
            telemetry_panel_cache_elapsed = telemetry_panel_cache_started_at.elapsed();
            Some(panel_rows)
        } else {
            None
        };

        let mut top_bar_elapsed = Duration::ZERO;
        let mut bottom_controls_elapsed = Duration::ZERO;
        let mut telemetry_panel_elapsed = Duration::ZERO;
        let mut center_overlay_elapsed = Duration::ZERO;
        let mut video_viewport_rect =
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(0.0, 0.0));
        // Exclusion-механизм остаётся в боундари рендера, но sidebar им больше
        // не пользуется: он сжимает video viewport, а не вырезает видео под собой.
        let video_exclusion_rects = Vec::new();

        let egui_run_started_at = Instant::now();
        let view_profile = self.view_profile();
        let live_scrub_enabled = self.live_scrub_gesture_enabled();
        let full_output = self.egui_ctx.run_ui(egui_input, |ui| {
            video_viewport_rect = ui.max_rect();

            // Video dim обязан быть первым egui primitive-ом: chrome, sidebar и
            // transport рисуются поверх него и сохраняют собственные цвета.
            if let Some(dim_color) = stale_video_dim_color {
                ui_artwork_egui::ArtworkPainter::new(ui.painter())
                    .video_dim_overlay(ui.max_rect(), dim_color);
            }

            let stage_started_at = Instant::now();
            // Заголовок и нижняя панель уезжают за край экрана по решению автоскрытия.
            let titlebar = fullscreen_chrome_panels::show_titlebar(
                ui,
                chrome_presentation.hidden_fraction,
                WindowChromeInput {
                    title: "Fastiplayer",
                    height_points: titlebar_height_points,
                    is_maximized: window_is_maximized,
                    is_fullscreen: window_is_fullscreen,
                    style: WindowChromeStyle::from_controls_style(controls_style),
                    edge_alignment: window_chrome_edge_alignment,
                    active_sidebar_section: self.sidebar_controller.target(),
                    sidebar_sections: view_profile.sidebar_sections(),
                },
            );
            let window_chrome_output = titlebar.inner;
            window_chrome_actions = window_chrome_output.window_actions;
            for titlebar_action in window_chrome_output.titlebar_icon_actions {
                let TitlebarIconAreaAction::SelectSidebarSection(section) = titlebar_action;
                let outcome = self.sidebar_controller.select(section);
                if matches!(
                    outcome,
                    super::sidebar_controller::SidebarSelectionOutcome::Opened(
                        crate::state::SidebarSection::Settings
                    )
                ) {
                    settings_actions.push(SettingsUiAction::Open);
                }
            }
            top_bar_elapsed = stage_started_at.elapsed();

            let stage_started_at = Instant::now();
            let bottom_controls = fullscreen_chrome_panels::show_bottom_controls(
                ui,
                chrome_presentation.hidden_fraction,
                player_controls::BottomControlsInput {
                    player_snapshot,
                    timeline_state: &mut timeline_ui_state,
                    timeline_inline_status,
                    skin: &selected_skin,
                    is_window_fullscreen: window_is_fullscreen,
                    live_scrub_enabled,
                    view_profile,
                    reduced_motion: self.committed_config_snapshot.reduced_motion(),
                    playlist_transport: playlist_models.transport,
                    subtitle_menu: &subtitle_menu,
                    audio_menu: &audio_menu,
                },
            );
            control_actions = bottom_controls.inner;
            // Панель закрывает низ видео: владелец субтитров поднимает слой, только если мешает.
            self.subtitles.observe_bottom_controls_obstruction(
                BottomControlsObstruction::from_visible_rect(
                    bottom_controls.visible_rect,
                    ui.ctx().pixels_per_point(),
                ),
            );
            bottom_controls_elapsed = stage_started_at.elapsed();

            let sidebar_rect = sidebar::show(
                ui,
                &mut self.sidebar_host_state,
                self.sidebar_controller.displayed(),
                sidebar_slide_progress,
                self.sidebar_controller.content_transition(),
                SidebarRenderContext {
                    model: settings_ui_model,
                    settings_host: settings_host_facts,
                    snapshot: player_snapshot,
                    url_model: &url_sidebar_model,
                    playlist_model: playlist_view_model.as_ref(),
                    playlist_interaction: playlist_models.interaction,
                    playlist_undo: playlist_models.undo,
                    playlist_row_style: selected_skin.playlist_row_style(),
                    playlist_toolbar_style: selected_skin.playlist_toolbar_style(),
                    playlist_header_undo_style: selected_skin.playlist_header_undo_style(),
                    ui_motion: crate::ui::animation::UiMotion::from_reduced_motion(
                        self.committed_config_snapshot.reduced_motion(),
                    ),
                    window_chrome_edge_alignment,
                    playlist_state: &mut playlist_ui_state,
                    playlist_output: &mut playlist_ui_output,
                    url_action: &mut url_sidebar_action,
                    settings_actions: &mut settings_actions,
                    close_requested: &mut sidebar_close_requested,
                },
            );
            self.external_open.record_playlist_panel_rect(
                super::external_open::playlist_panel_rect(
                    sidebar_rect.as_ref(),
                    self.sidebar_controller.displayed(),
                ),
            );
            if let Some(sidebar_output) = sidebar_rect {
                sidebar_width_change = sidebar_output.width_change;
                debug_assert!(
                    (sidebar_output.open_width_points
                        - self.sidebar_host_state.open_width_points())
                    .abs()
                        < f32::EPSILON
                );
                // Sidebar вытесняет видео: content viewport начинается от правого
                // края панели, letterbox/aspect ratio рендер пересчитает сам.
                video_viewport_rect.min.x = sidebar_output
                    .rect
                    .right()
                    .clamp(video_viewport_rect.min.x, video_viewport_rect.max.x);
            }

            if let Some(telemetry_panel_rows) = telemetry_panel_rows.as_ref() {
                let stage_started_at = Instant::now();
                Self::render_telemetry_panel(ui, telemetry_panel_rows);
                telemetry_panel_elapsed =
                    telemetry_panel_cache_elapsed + stage_started_at.elapsed();
            }

            // До оверлеев: их виджеты регистрируются позже и перехватывают клики первыми.
            // Ввод по видео принадлежит либо обычному просмотру, либо правке субтитров.
            let video_rect = ui.available_rect_before_wrap();
            let video_area = video_area_input::show(
                ui,
                video_rect,
                match subtitle_edit_view.as_ref() {
                    Some(view) => {
                        VideoAreaMode::SubtitleAppearanceEdit(SubtitleAppearanceEditInput {
                            view,
                            video_rect,
                            style: &subtitle_edit_style,
                        })
                    }
                    None => VideoAreaMode::Playback,
                },
            );
            control_actions.extend(video_area.control_actions);
            subtitle_edit_actions = video_area.subtitle_edit_actions;
            // Глобальные клавиши (скорость, громкость, Ctrl+O) — после панелей кадра:
            // сфокусированные виджеты уже забрали свои клавиши.
            control_actions.extend(global_hotkeys::collect_actions(ui.ctx()));

            let stage_started_at = Instant::now();
            playlist_confirmation_action = Self::render_center_overlay(
                ui,
                player_snapshot.playback_state,
                view_profile.idle_center_hint(),
                &notifications_frame,
                &mut notification_output,
                playlist_models.import_preview,
                playlist_models.confirmation,
                &mut playlist_ui_output,
            );
            center_overlay_elapsed = stage_started_at.elapsed();
            crate::ui::external_drop_overlay::show_external_drop_overlay(
                ui.ctx(),
                self.external_open
                    .drop_overlay(video_viewport_rect, ui.ctx().pixels_per_point()),
            );
            let visible_chrome_rects = [titlebar.visible_rect, bottom_controls.visible_rect]
                .map(|visible_rect| visible_rect.unwrap_or(egui::Rect::NOTHING));
            self.fullscreen_chrome.finish_frame(
                ui.ctx(),
                ChromeHoldObservation {
                    visible_chrome_rects: &visible_chrome_rects,
                    sidebar_open: self.sidebar_controller.displayed().is_some(),
                    subtitle_appearance_editing: subtitle_edit_view.is_some(),
                    playback_state: player_snapshot.playback_state,
                },
            );
        });
        if sidebar_close_requested {
            self.sidebar_controller.hide();
        }
        self.apply_subtitle_appearance_edit_actions(subtitle_edit_actions);
        self.route_subtitle_appearance_edit_settings_actions(&mut settings_actions);
        self.apply_notification_actions(notification_output);
        let egui_run_elapsed = egui_run_started_at.elapsed();

        if self.sidebar_controller.is_animating() || self.fullscreen_chrome.is_animating() {
            // Пока анимация sidebar или панелей фуллскрина активна, просим следующий кадр явно:
            // без playback нет другого источника непрерывных redraw-ов.
            self.egui_ctx.request_repaint();
        }

        let post_ui_actions_started_at = Instant::now();
        self.playlist_ui_state = playlist_ui_state;
        self.timeline_ui_state = timeline_ui_state;
        let transport_actions =
            self.handle_control_actions(window, player_snapshot, control_actions);
        let post_ui_actions_elapsed = post_ui_actions_started_at.elapsed();

        RenderedAppUi {
            full_output,
            settings_actions,
            sidebar_width_change,
            transport_actions,
            window_chrome_actions,
            playlist_confirmation_action,
            playlist_actions: playlist_ui_output.take_actions(),
            url_sidebar_action,
            playlist_visible_items_hint: playlist_runtime_binding
                .and_then(|binding| playlist_ui_output.into_visible_hint(binding)),
            video_viewport_rect,
            video_exclusion_rects,
            timings: AppUiRenderTimings {
                total: render_ui_started_at.elapsed(),
                pre_ui_setup: pre_ui_setup_elapsed,
                egui_run: egui_run_elapsed,
                top_bar: top_bar_elapsed,
                bottom_controls: bottom_controls_elapsed,
                telemetry_panel: telemetry_panel_elapsed,
                center_overlay: center_overlay_elapsed,
                post_ui_actions: post_ui_actions_elapsed,
                telemetry_panel_visible: show_telemetry,
            },
        }
    }

    /// Применяет действия controls после завершения egui pass.
    pub(super) fn handle_control_actions(
        &mut self,
        window: &Window,
        player_snapshot: &PlayerSnapshot,
        actions: Vec<ControlAction>,
    ) -> Vec<crate::ui::player_controls::TransportControlAction> {
        let mut transport_actions = Vec::new();
        for action in control_action_coalescing::coalesce_consecutive_volume(actions) {
            match action {
                ControlAction::Transport(action) => transport_actions.push(action),
                // Открытие своего медиа есть не в каждом виде окна (браузерное — без него).
                ControlAction::OpenFile if !self.view_profile().offers_media_opening() => {}
                ControlAction::OpenFile => self.open_file(window),
                ControlAction::SetVolume(requested_volume) => {
                    if let Err(error) = self
                        .player_worker
                        .try_send_command(PlayerCommand::SetVolume(requested_volume))
                    {
                        warn!(error = %error, "Incorrect Volume Value from UI");
                    } else {
                        self.mark_pending_worker_redraw();
                    }
                }
                ControlAction::AdjustVolumeSteps(step_count) => {
                    self.apply_volume_steps(player_snapshot, step_count);
                }
                ControlAction::ToggleMute => {
                    if let Err(error) =
                        self.player_worker
                            .try_send_command(PlayerCommand::ToggleMute {
                                fallback_volume: self
                                    .committed_config_snapshot
                                    .remembered_audible_volume(),
                            })
                    {
                        warn!(error = %error, "Could not switch mute from UI");
                    } else {
                        self.mark_pending_worker_redraw();
                    }
                }
                ControlAction::AdjustPlaybackRateSteps(_) | ControlAction::ResetPlaybackRate => {
                    let Some(command) = playback_rate_command_from_action(player_snapshot, &action)
                    else {
                        continue;
                    };

                    if let Err(error) = self.send_transport_command(command) {
                        warn!(error = %error, "Unable to change playback speed from UI");
                    } else {
                        self.mark_pending_worker_redraw();
                    }
                }
                ControlAction::ToggleFullscreen => Self::toggle_fullscreen(window),
                ControlAction::Timeline(timeline_action) => {
                    self.send_timeline_action(timeline_action);
                }
                ControlAction::Subtitles(subtitle_action) => {
                    self.apply_subtitle_control_action(subtitle_action, window);
                }
                ControlAction::AudioTracks(audio_action) => {
                    self.apply_audio_track_control_action(audio_action, player_snapshot);
                }
            }
        }
        transport_actions
    }

    /// Снимает live-scrub completion-gate по worker landing-сигналу.
    ///
    /// `PlayerEvent::SeekTargetFramePresented` означает, что seek/scrub landing
    /// frame уже стал presented; для active live drag это разблокирует следующий
    /// newest decode target (frame-by-frame перемотка вместо supersede-голодания).
    pub(crate) fn note_live_scrub_landing_for_dispatch(&mut self, target_position: Duration) {
        self.timeline_ui_state.note_live_scrub_landing_presented(
            media_core::MediaTime::from_duration(target_position),
        );
    }

    /// Захватывает S19 live scrub settings snapshot для нового pointer gesture-а.
    pub(super) fn live_scrub_settings_snapshot(&self) -> TimelineLiveScrubSettingsSnapshot {
        let decode_mode = match self.committed_config_snapshot.live_scrub_decode_mode() {
            FrameServerLiveScrubDecodeModeConfig::ThrottledLatest => {
                TimelineLiveScrubDecodeMode::ThrottledLatest
            }
            FrameServerLiveScrubDecodeModeConfig::EveryDragEvent => {
                TimelineLiveScrubDecodeMode::EveryDragEvent
            }
        };

        TimelineLiveScrubSettingsSnapshot {
            decode_mode,
            max_hz: self.committed_config_snapshot.live_scrub_max_hz(),
        }
    }

    /// Выполняет configured hotkey seek без дублирования timeline business logic.
    fn seek_by_hotkey(&mut self, step: Duration, forward: bool) {
        let snapshot = self.refresh_player_snapshot();
        let target = if forward {
            snapshot.current_position.checked_add(step).map_or(
                snapshot.current_position,
                |position| {
                    snapshot
                        .duration
                        .filter(|duration| !duration.is_zero())
                        .map_or(position, |duration| position.min(duration))
                },
            )
        } else {
            snapshot.current_position.saturating_sub(step)
        };

        if let Err(error) =
            self.send_transport_command(PlayerCommand::Seek(SeekRequest::absolute(target.into())))
        {
            warn!(error = %error, "Could not send the configured hotkey seek");
        } else {
            self.mark_pending_worker_redraw();
        }
    }

    /// Обрабатывает legacy-горячие клавиши shell (F, M, J/L, стрелки, PageUp/PageDown)
    /// и отправляет команды в playback worker. Space здесь намеренно нет: он идёт
    /// через `ShellHotkeyAction::Transport(TogglePlayback)` в `app_shell/hotkeys.rs`.
    pub fn handle_hotkeys(
        &mut self,
        window: &Window,
        key: winit::keyboard::KeyCode,
        egui_wants_input: bool,
    ) -> bool {
        if egui_wants_input {
            return false;
        }

        match key {
            winit::keyboard::KeyCode::KeyF => {
                Self::toggle_fullscreen(window);
                true
            }
            winit::keyboard::KeyCode::KeyM => {
                if let Err(error) = self
                    .player_worker
                    .try_send_command(PlayerCommand::ToggleMute {
                        fallback_volume: self.committed_config_snapshot.remembered_audible_volume(),
                    })
                {
                    warn!(error = %error, "Failed to switch mute");
                } else {
                    self.mark_pending_worker_redraw();
                }
                true
            }
            winit::keyboard::KeyCode::ArrowLeft | winit::keyboard::KeyCode::KeyJ => {
                self.seek_by_hotkey(
                    self.committed_config_snapshot.hotkey_small_seek_step(),
                    false,
                );
                true
            }
            winit::keyboard::KeyCode::ArrowRight | winit::keyboard::KeyCode::KeyL => {
                self.seek_by_hotkey(
                    self.committed_config_snapshot.hotkey_small_seek_step(),
                    true,
                );
                true
            }
            winit::keyboard::KeyCode::PageUp => {
                self.seek_by_hotkey(
                    self.committed_config_snapshot.hotkey_large_seek_step(),
                    false,
                );
                true
            }
            winit::keyboard::KeyCode::PageDown => {
                self.seek_by_hotkey(
                    self.committed_config_snapshot.hotkey_large_seek_step(),
                    true,
                );
                true
            }
            _ => false,
        }
    }

    /// Открывает локальный media-файл через file dialog.
    pub fn open_file(&mut self, window: &Window) {
        if self.has_pending_local_file_open() {
            self.notify_open_still_in_progress();
            return;
        }
        match LocalFileOpenJob::spawn_picker(window, self.local_file_open_wake_port.clone()) {
            Ok(job) => {
                self.local_file_open_job = Some(job);
                self.set_startup_pending("Selecting a media file...".to_string());
            }
            Err(error) => {
                warn!(error = %error, "Could not start the local file open job");
                self.set_startup_error(format!("Opening the media file error: {error}"));
            }
        }
    }
    /// Собирает frame counters из текущей телеметрии.
    pub(super) fn frame_counters_snapshot(&self) -> FrameCounters {
        FrameCounters {
            presented: self.telemetry.video_frames_presented(),
            dropped: self.telemetry.video_frames_dropped(),
            repeated: self.telemetry.video_frames_repeated(),
        }
    }
}
#[cfg(test)]
#[path = "center_overlay_tests.rs"]
mod center_overlay_tests;
