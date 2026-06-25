use super::telemetry_panel::TelemetryPanelState;
use super::*;

/// Diagnostic route пользовательского timeline intent-а на границе app-egui -> player-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimelineCommandRoute {
    /// Одиночный click ушёл в exact final seek без scrub generation.
    ClickSeek,

    /// Pointer drag release ушёл как exact final seek без scrub generation.
    DragSeek,
}

impl TimelineCommandRoute {
    /// Возвращает стабильное имя route-а для logs/diagnostics.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::ClickSeek => "click-seek",
            Self::DragSeek => "drag-seek",
        }
    }
}

/// Конвертирует timeline action в player command и diagnostic route.
pub(super) fn timeline_command_from_action(
    action: TimelineAction,
) -> (PlayerCommand, TimelineCommandRoute) {
    match action {
        TimelineAction::ClickSeek(position) => (
            PlayerCommand::Seek(SeekRequest::absolute(position)),
            TimelineCommandRoute::ClickSeek,
        ),
        TimelineAction::CommitDragSeek(position) => (
            PlayerCommand::Seek(SeekRequest::absolute(position)),
            TimelineCommandRoute::DragSeek,
        ),
    }
}

/// Мапит neutral titlebar intent в старый settings UI action boundary.
pub(super) const fn settings_action_from_titlebar_icon_action(
    action: TitlebarIconAreaAction,
) -> SettingsUiAction {
    match action {
        TitlebarIconAreaAction::ToggleSettingsSidebar => SettingsUiAction::ToggleOpen,
    }
}

impl AppState {
    /// Рендерит egui UI поверх видео.
    ///
    /// UI читает только `PlayerSnapshot`, а действия после egui closure отправляет worker-у.
    #[instrument(skip(self, window, frame_context, egui_input, settings_ui_model))]
    pub fn render_ui(
        &mut self,
        window: &Window,
        egui_input: egui::RawInput,
        frame_context: &AppFrameContext,
        settings_ui_model: &SettingsUiModel,
    ) -> RenderedAppUi {
        let render_ui_started_at = Instant::now();

        let pre_ui_setup_started_at = Instant::now();
        let player_snapshot = frame_context.player_snapshot();
        let is_playing = player_snapshot.playback_state == PlaybackState::Playing;

        if is_playing {
            self.next_frame();
        }

        let backend_name = player_snapshot
            .active_backend
            .name
            .clone()
            .unwrap_or_else(|| "Synthetic (test)".to_string());
        let telemetry = Arc::clone(&self.telemetry);
        let start_time = self.start_time;
        let frame_duration_estimate_ms =
            player_snapshot.video_frame_duration_estimate.as_secs_f64() * 1000.0;
        let player_error_message = player_snapshot
            .last_error
            .as_ref()
            .map(std::string::ToString::to_string);
        let error_message = player_error_message
            .as_deref()
            .or(self.startup_error.as_deref());
        let pending_message = if error_message.is_none() {
            self.startup_pending.as_deref()
        } else {
            None
        };
        let render_diagnostics = frame_context.render_diagnostics();
        let selected_skin =
            skin::skin_from_config(self.committed_config_snapshot.ui_skin()).unwrap_or_else(|| {
            warn!(
                skin = %self.committed_config_snapshot.ui_skin(),
                "Config validation должна была отклонить неизвестный UI skin; используем minimal"
            );
            skin::MinimalSkin
        });
        let animation_state = AnimationState::from_timeline(&player_snapshot.timeline);
        // Позиция анимации продвинута раньше в prepare_ui_frame; здесь только чтение.
        let sidebar_slide_progress = self.sidebar_slide.eased_progress(Easing::EaseInOutCubic);
        let show_telemetry = self.committed_config_snapshot.show_telemetry();
        let titlebar_height_points = self.committed_config_snapshot.titlebar_height_points();
        let window_is_maximized = window.is_maximized();
        let window_is_fullscreen = window.fullscreen().is_some();
        let mut control_actions = Vec::new();
        let mut settings_actions = Vec::new();
        let mut window_chrome_actions = Vec::new();
        let mut timeline_ui_state = std::mem::take(&mut self.timeline_ui_state);
        let pre_ui_setup_elapsed = pre_ui_setup_started_at.elapsed();
        let mut telemetry_panel_cache_elapsed = Duration::ZERO;
        let telemetry_panel_rows = if show_telemetry {
            let telemetry_panel_cache_started_at = Instant::now();
            let panel_rows = self.telemetry_panel_cache.rows_for_frame(
                Instant::now(),
                TelemetryPanelState {
                    player_snapshot,
                    telemetry: &telemetry,
                    render_diagnostics,
                    timeline_ui_state: &timeline_ui_state,
                    backend_name: &backend_name,
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
        let full_output = self.egui_ctx.run_ui(egui_input, |ui| {
            video_viewport_rect = ui.max_rect();

            let stage_started_at = Instant::now();
            let window_chrome_output = window_chrome::show(
                ui,
                WindowChromeInput {
                    title: "Rustiplayer",
                    height_points: titlebar_height_points,
                    is_maximized: window_is_maximized,
                    style: WindowChromeStyle::from_controls_style(selected_skin.controls_style()),
                },
            );
            window_chrome_actions = window_chrome_output.window_actions;
            settings_actions.extend(
                window_chrome_output
                    .titlebar_icon_actions
                    .into_iter()
                    .map(settings_action_from_titlebar_icon_action),
            );
            top_bar_elapsed = stage_started_at.elapsed();

            let stage_started_at = Instant::now();
            control_actions = player_controls::render_bottom_controls(
                ui,
                player_snapshot,
                &mut timeline_ui_state,
                &selected_skin,
                window_is_fullscreen,
            );
            bottom_controls_elapsed = stage_started_at.elapsed();

            let sidebar_rect = sidebar::show(
                ui,
                AppSidebarContent::Settings {
                    model: settings_ui_model,
                },
                sidebar_slide_progress,
                &mut settings_actions,
            );
            if let Some(sidebar_rect) = sidebar_rect {
                // Sidebar вытесняет видео: content viewport начинается от правого
                // края панели, letterbox/aspect ratio рендер пересчитает сам.
                video_viewport_rect.min.x = sidebar_rect
                    .right()
                    .clamp(video_viewport_rect.min.x, video_viewport_rect.max.x);
            }

            if let Some(telemetry_panel_rows) = telemetry_panel_rows.as_ref() {
                let stage_started_at = Instant::now();
                Self::render_telemetry_panel(ui, telemetry_panel_rows);
                telemetry_panel_elapsed =
                    telemetry_panel_cache_elapsed + stage_started_at.elapsed();
            }

            let stage_started_at = Instant::now();
            Self::render_center_overlay(
                ui,
                is_playing,
                error_message,
                pending_message,
                &selected_skin,
                animation_state,
            );
            center_overlay_elapsed = stage_started_at.elapsed();
        });
        let egui_run_elapsed = egui_run_started_at.elapsed();

        if self.sidebar_slide.is_animating() {
            // Пока анимация sidebar активна, просим следующий кадр явно:
            // без playback нет другого источника непрерывных redraw-ов.
            self.egui_ctx.request_repaint();
        }

        let post_ui_actions_started_at = Instant::now();
        self.timeline_ui_state = timeline_ui_state;
        self.handle_control_actions(window, control_actions);
        let post_ui_actions_elapsed = post_ui_actions_started_at.elapsed();

        RenderedAppUi {
            full_output,
            settings_actions,
            window_chrome_actions,
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
    pub(super) fn handle_control_actions(&mut self, window: &Window, actions: Vec<ControlAction>) {
        for action in actions {
            match action {
                ControlAction::TogglePlayback => self.toggle_playback(),
                ControlAction::OpenFile => self.open_file(window),
                ControlAction::SetVolume(requested_volume) => {
                    if let Err(error) = self
                        .player_worker
                        .try_send_command(PlayerCommand::SetVolume(requested_volume))
                    {
                        warn!(error = %error, "Некорректное значение громкости из UI");
                    } else {
                        self.mark_pending_worker_redraw();
                    }
                }
                ControlAction::ToggleMute => {
                    if let Err(error) =
                        self.player_worker
                            .try_send_command(PlayerCommand::ToggleMute {
                                fallback_volume: self
                                    .committed_config_snapshot
                                    .default_volume_for_new_media(),
                            })
                    {
                        warn!(error = %error, "Не удалось переключить mute из UI");
                    } else {
                        self.mark_pending_worker_redraw();
                    }
                }
                ControlAction::ToggleFullscreen => Self::toggle_fullscreen(window),
                ControlAction::Timeline(timeline_action) => {
                    self.send_timeline_action(timeline_action);
                }
            }
        }
    }

    /// Конвертирует pointer timeline action в typed player command.
    ///
    /// Здесь находится единственный app-egui route, который переводит timeline intent
    /// в worker command. Click и drag release отправляются как `PlayerCommand::Seek`,
    /// чтобы UI не зависел от interactive scrub lifecycle внутри player-core.
    pub(super) fn send_timeline_action(&mut self, action: TimelineAction) {
        let (command, route) = timeline_command_from_action(action);
        let command_for_diagnostics = command.clone();

        if let Err(error) = self.player_worker.try_send_command(command) {
            warn!(error = %error, "Не удалось отправить timeline command");
            return;
        }

        debug!(
            action = ?action,
            command = ?command_for_diagnostics,
            route = route.as_str(),
            "Timeline action отправлен в player worker"
        );
        self.mark_pending_worker_redraw();
    }

    /// Переключает fullscreen состояние окна.
    pub(super) fn toggle_fullscreen(window: &Window) {
        let is_fullscreen = window.fullscreen().is_some();
        if is_fullscreen {
            window.set_fullscreen(None);
            return;
        }

        if let Some(monitor) = window.current_monitor() {
            window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(Some(monitor))));
        }
    }

    /// Обрабатывает горячие клавиши shell и отправляет команды в playback worker.
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
            winit::keyboard::KeyCode::Space => {
                self.toggle_playback();
                true
            }
            winit::keyboard::KeyCode::KeyF => {
                Self::toggle_fullscreen(window);
                true
            }
            winit::keyboard::KeyCode::KeyM => {
                let player_snapshot = self.refresh_player_snapshot();
                self.publish_desktop_snapshot(&player_snapshot);
                if let Err(error) = self
                    .player_worker
                    .try_send_command(PlayerCommand::ToggleMute {
                        fallback_volume: self
                            .committed_config_snapshot
                            .default_volume_for_new_media(),
                    })
                {
                    warn!(error = %error, "Не удалось переключить mute");
                } else {
                    self.mark_pending_worker_redraw();
                }
                true
            }
            _ => false,
        }
    }

    /// Открывает локальный media-файл через file dialog.
    pub fn open_file(&mut self, window: &Window) {
        if self.has_pending_local_file_open() {
            debug!("Local file open job уже активен, повторный dialog не запускаем");
            return;
        }

        match LocalFileOpenJob::spawn(
            window,
            self.committed_config_snapshot.demux_config_for_open(),
        ) {
            Ok(job) => {
                self.local_file_open_job = Some(job);
                self.set_startup_pending("Выбор media-файла...".to_string());
            }
            Err(error) => {
                warn!(error = %error, "Не удалось запустить local file open job");
                self.set_startup_error(format!("Ошибка открытия media-файла: {error}"));
            }
        }
    }

    /// Рендерит центральный overlay состояния.
    fn render_center_overlay(
        ui: &mut egui::Ui,
        is_playing: bool,
        error_message: Option<&str>,
        pending_message: Option<&str>,
        skin: &impl PlayerSkin,
        animation_state: AnimationState,
    ) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                if let Some(dim_color) = skin.stale_frame_dim_color(animation_state) {
                    ui.painter().rect_filled(ui.max_rect(), 0.0, dim_color);
                }

                if let Some(error) = error_message {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.colored_label(egui::Color32::RED, error);
                    });
                } else if let Some(message) = pending_message {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.colored_label(egui::Color32::LIGHT_BLUE, message);
                    });
                } else if !is_playing {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.heading("Press Play to start");
                    });
                }
            });
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
