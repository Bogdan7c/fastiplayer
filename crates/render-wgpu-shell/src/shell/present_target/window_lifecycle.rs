//! Жизненный цикл показа на окне у wgpu (сессия 02a плана `user/gles2-renderer/`).
//!
//! Инвариант «одно окно — одна surface»: Mesa WSI на Wayland заводит на каждую Vulkan-surface
//! `wp_fifo_v1`, а второй такой объект на одном `wl_surface` — протокольная ошибка, после
//! которой композитор рвёт соединение. Поэтому:
//! - перенастройка present mode/задержки идёт `Surface::configure` на ТОЙ ЖЕ surface;
//! - «отпустить окно» роняет `wgpu::Surface` (wgpu-core в `Drop` делает `unconfigure`:
//!   ждёт простоя устройства и уничтожает swapchain, затем уничтожается `VkSurfaceKHR`);
//! - «вернуть окно» создаёт новую surface тем же `wgpu::Instance` на том же окне и настраивает
//!   её прежней конфигурацией (с текущим размером окна).
//!
//! Ошибки `configure` wgpu отдаёт не возвратом, а в обработчик ошибок устройства (по
//! умолчанию — паника). Здесь `configure` обёрнут в error scope-ы и превращается в
//! типизированный отказ. Вызывать всё это можно только без живого `SurfaceTexture`: иначе
//! wgpu паникует (приложение применяет настройки до `acquire_surface_frame` своего тика).

use render_shell_api::{
    PresentationDetachOutcome, PresentationReattachError, PresentationReattachOutcome,
    PresentationReconfigureError, PresentationReconfigured, ShellPresentMode,
};
use tracing::{info, warn};

use super::{WindowPresentTarget, WindowSurface};
use crate::RenderFrameDropReason;
use crate::SurfacePresentSettings;
use crate::shell::gpu_context::GpuContext;
use crate::surface_settings::{choose_present_mode, requested_wgpu_present_mode};

impl WindowPresentTarget {
    /// Применяет новые настройки показа к текущей surface; при отказе возвращает прежнюю.
    pub(super) fn reconfigure_presentation(
        &mut self,
        gpu: &GpuContext,
        settings: SurfacePresentSettings,
    ) -> Result<PresentationReconfigured, PresentationReconfigureError> {
        let surface = match &self.surface {
            WindowSurface::Configured(surface) | WindowSurface::Unconfigured(surface) => surface,
            WindowSurface::Detached => {
                return Err(PresentationReconfigureError::PresentationDetached);
            }
        };
        if settings.alpha_preference != self.presentation.alpha_preference {
            // Альфа выбрана при создании и определяет пайплайн маски углов (GpuContext).
            return Err(PresentationReconfigureError::AlphaPreferenceChangeUnsupported);
        }

        // Режим выбирается теми же правилами и по тем же возможностям, что при создании.
        let capabilities = surface.get_capabilities(&gpu.adapter);
        let present_mode = choose_present_mode(&capabilities.present_modes, settings.present_mode)
            .map_err(
                |error| PresentationReconfigureError::RejectedPreviousRestored {
                    message: format!("{error:#}"),
                },
            )?;
        let next_config = wgpu::SurfaceConfiguration {
            present_mode,
            // 0 кадров latency недопустимо для swapchain — нормализуем к 1, как при создании.
            desired_maximum_frame_latency: settings.max_frame_latency.max(1),
            ..self.config.clone()
        };
        let surface_is_configured = matches!(self.surface, WindowSurface::Configured(_));
        if surface_is_configured && same_presentation(&next_config, &self.config) {
            self.presentation = settings;
            return Ok(PresentationReconfigured::AlreadyActive);
        }

        match configure_surface_checked(surface, &gpu.device, &next_config) {
            Ok(()) => {
                info!(
                    present_mode = ?next_config.present_mode,
                    desired_maximum_frame_latency = next_config.desired_maximum_frame_latency,
                    "Surface presentation reconfigured in place"
                );
                self.config = next_config;
                self.presentation = settings;
                self.mark_configured();
                Ok(reconfigured_outcome(settings.present_mode, present_mode))
            }
            Err(new_config_error) => self.restore_previous_config(gpu, new_config_error),
        }
    }

    /// Новая конфигурация отклонена: возвращаем прежнюю, иначе surface остаётся без рабочей.
    fn restore_previous_config(
        &mut self,
        gpu: &GpuContext,
        new_config_error: String,
    ) -> Result<PresentationReconfigured, PresentationReconfigureError> {
        let surface = match &self.surface {
            WindowSurface::Configured(surface) | WindowSurface::Unconfigured(surface) => surface,
            WindowSurface::Detached => {
                return Err(PresentationReconfigureError::PresentationDetached);
            }
        };
        warn!(error = %new_config_error, "Surface rejected the new presentation; restoring the previous one");
        match configure_surface_checked(surface, &gpu.device, &self.config) {
            Ok(()) => {
                self.mark_configured();
                Err(PresentationReconfigureError::RejectedPreviousRestored {
                    message: new_config_error,
                })
            }
            Err(previous_config_error) => {
                self.mark_unconfigured();
                Err(PresentationReconfigureError::PresentationLost {
                    message: format!(
                        "new configuration: {new_config_error}; previous configuration: {previous_config_error}"
                    ),
                })
            }
        }
    }

    /// Пробует снова настроить surface без рабочей конфигурации её прежней `config`.
    pub(super) fn recover_unconfigured_surface(
        &mut self,
        device: &wgpu::Device,
    ) -> Result<(), RenderFrameDropReason> {
        let WindowSurface::Unconfigured(surface) = &self.surface else {
            return Ok(());
        };
        match configure_surface_checked(surface, device, &self.config) {
            Ok(()) => {
                info!("Surface presentation recovered with the previous configuration");
                self.mark_configured();
                Ok(())
            }
            Err(error) => {
                tracing::debug!(%error, "Surface still rejects the previous configuration");
                Err(RenderFrameDropReason::SurfaceLost)
            }
        }
    }

    /// Отпускает окно: роняет surface (swapchain и `VkSurfaceKHR`), конфигурация остаётся.
    pub(super) fn detach_presentation(&mut self) -> PresentationDetachOutcome {
        match std::mem::replace(&mut self.surface, WindowSurface::Detached) {
            WindowSurface::Detached => PresentationDetachOutcome::AlreadyDetached,
            WindowSurface::Configured(surface) | WindowSurface::Unconfigured(surface) => {
                // Drop wgpu-core `Surface` = unconfigure (ожидание простоя + уничтожение
                // swapchain); после него окно свободно для surface другого рендера.
                drop(surface);
                info!("Window presentation detached from the renderer");
                PresentationDetachOutcome::Detached
            }
        }
    }

    /// Возвращает окно: новая surface тем же instance-ом на том же окне, прежняя конфигурация.
    pub(super) fn reattach_presentation(
        &mut self,
        gpu: &GpuContext,
    ) -> Result<PresentationReattachOutcome, PresentationReattachError> {
        if !matches!(self.surface, WindowSurface::Detached) {
            return Ok(PresentationReattachOutcome::AlreadyAttached);
        }
        let surface = gpu
            .instance
            .create_surface(self.window.clone())
            .map_err(|error| PresentationReattachError::SurfaceCreation {
                message: error.to_string(),
            })?;
        ensure_surface_matches_config(&surface, &gpu.adapter, &self.config)?;

        // Размер окна мог измениться, пока оно было отпущено (resize только запоминал его).
        let window_size = self.window.inner_size();
        if window_size.width > 0 && window_size.height > 0 {
            self.config.width = window_size.width;
            self.config.height = window_size.height;
        }
        configure_surface_checked(&surface, &gpu.device, &self.config)
            .map_err(|message| PresentationReattachError::ConfigureRejected { message })?;
        self.surface = WindowSurface::Configured(surface);
        self.suboptimal_recovery.surface_reconfigured();
        info!(
            width = self.config.width,
            height = self.config.height,
            "Window presentation reattached to the renderer"
        );
        Ok(PresentationReattachOutcome::Reattached)
    }

    /// Surface снова приняла конфигурацию (из «без рабочей конфигурации» — в «настроена»).
    ///
    /// Каждая такая настройка создаёт новый swapchain, поэтому политика неоптимального
    /// swapchain-а начинает с чистого листа.
    fn mark_configured(&mut self) {
        self.suboptimal_recovery.surface_reconfigured();
        self.surface = match std::mem::replace(&mut self.surface, WindowSurface::Detached) {
            WindowSurface::Configured(surface) | WindowSurface::Unconfigured(surface) => {
                WindowSurface::Configured(surface)
            }
            WindowSurface::Detached => WindowSurface::Detached,
        };
    }

    /// Surface не приняла ни одной конфигурации: дальше acquire пробует восстановить прежнюю.
    pub(super) fn mark_unconfigured(&mut self) {
        self.surface = match std::mem::replace(&mut self.surface, WindowSurface::Detached) {
            WindowSurface::Configured(surface) | WindowSurface::Unconfigured(surface) => {
                WindowSurface::Unconfigured(surface)
            }
            WindowSurface::Detached => WindowSurface::Detached,
        };
    }
}

/// Совпадает ли то, что меняет перенастройка на месте (режим и задержка).
fn same_presentation(
    next: &wgpu::SurfaceConfiguration,
    current: &wgpu::SurfaceConfiguration,
) -> bool {
    next.present_mode == current.present_mode
        && next.desired_maximum_frame_latency == current.desired_maximum_frame_latency
}

/// «Как просили» или «запасной режим» — по тому же переводу режимов, что при создании.
fn reconfigured_outcome(
    requested: ShellPresentMode,
    chosen: wgpu::PresentMode,
) -> PresentationReconfigured {
    match requested_wgpu_present_mode(requested) {
        Some(requested_mode) if requested_mode != chosen => {
            PresentationReconfigured::PresentModeFallback { requested }
        }
        Some(_) | None => PresentationReconfigured::AsRequested,
    }
}

/// Новая surface обязана принять прежнюю конфигурацию: тот же adapter, формат, режимы.
fn ensure_surface_matches_config(
    surface: &wgpu::Surface<'static>,
    adapter: &wgpu::Adapter,
    config: &wgpu::SurfaceConfiguration,
) -> Result<(), PresentationReattachError> {
    if !adapter.is_surface_supported(surface) {
        return Err(PresentationReattachError::IncompatibleSurface {
            message: "renderer adapter cannot present to the new window surface".to_owned(),
        });
    }
    let capabilities = surface.get_capabilities(adapter);
    let mismatch = if !capabilities.formats.contains(&config.format) {
        Some(format!("format {:?} is not supported", config.format))
    } else if !capabilities.present_modes.contains(&config.present_mode) {
        Some(format!(
            "present mode {:?} is not supported",
            config.present_mode
        ))
    } else if !capabilities.alpha_modes.contains(&config.alpha_mode) {
        Some(format!(
            "alpha mode {:?} is not supported",
            config.alpha_mode
        ))
    } else {
        None
    };
    match mismatch {
        Some(message) => Err(PresentationReattachError::IncompatibleSurface { message }),
        None => Ok(()),
    }
}

/// `Surface::configure` с перехватом ошибок устройства вместо паники обработчика по умолчанию.
///
/// Error scope-ы ловят ошибки валидации, нехватки памяти и внутренние; снимаются в обратном
/// порядке (требование wgpu). У native-backend-а future `pop` готов сразу.
pub(super) fn configure_surface_checked(
    surface: &wgpu::Surface<'static>,
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> Result<(), String> {
    let validation_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let out_of_memory_scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let internal_scope = device.push_error_scope(wgpu::ErrorFilter::Internal);
    surface.configure(device, config);
    let captured_errors: Vec<String> = [
        internal_scope.pop(),
        out_of_memory_scope.pop(),
        validation_scope.pop(),
    ]
    .into_iter()
    .filter_map(pollster::block_on)
    .map(|error| error.to_string())
    .collect();
    if captured_errors.is_empty() {
        Ok(())
    } else {
        Err(captured_errors.join("; "))
    }
}
