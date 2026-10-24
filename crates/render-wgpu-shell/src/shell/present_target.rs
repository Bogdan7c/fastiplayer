//! Цель показа рендера: swapchain окна или (только в тестах) offscreen-текстура.
//!
//! Решение владельца в сессии 02 плана `user/gles2-renderer/`: conformance-набор контракта
//! гоняется на настоящем wgpu-рендере без окна. Поэтому всё, что зависит от окна (surface,
//! её конфигурация, `pre_present_notify`, present), собрано здесь за одним enum-ом, а весь
//! остальной путь кадра (egui, видео, субтитры, submit) в `shell.rs` общий для обеих целей.
//! В production-сборке вариант `Offscreen` не существует.
//!
//! С сессии 02a здесь же жизненный цикл показа («одно окно — одна surface»): перенастройка
//! present mode/задержки на месте и «отпустить/вернуть окно» при полном пересоздании рендера
//! (окно — `window_lifecycle.rs`, offscreen-модель тех же состояний — `offscreen_lifecycle.rs`).

use std::sync::Arc;

use render_shell_api::{
    PresentationDetachOutcome, PresentationReattachError, PresentationReattachOutcome,
    PresentationReconfigureError, PresentationReconfigured,
};
use tracing::{debug, instrument};
use winit::window::Window;

use super::gpu_context::GpuContext;
use crate::{RenderFrameDropReason, SurfacePresentSettings};

#[cfg(test)]
mod offscreen_lifecycle;
mod window_lifecycle;

#[cfg(test)]
use offscreen_lifecycle::OffscreenPresentationState;

/// Куда рендер рисует кадры.
pub(crate) enum PresentTarget {
    /// Swapchain окна приложения.
    Window(WindowPresentTarget),

    /// Текстура в памяти GPU для conformance-тестов без окна.
    #[cfg(test)]
    Offscreen(OffscreenPresentTarget),
}

/// Swapchain окна: winit-окно, surface и её текущая конфигурация.
pub(crate) struct WindowPresentTarget {
    /// Окно получает `pre_present_notify` перед каждым present (frame callback Wayland);
    /// на нём же создаётся новая surface при возврате окна.
    window: Arc<Window>,

    /// Surface окна и её состояние (настроена / без рабочей конфигурации / окно отпущено).
    surface: WindowSurface,

    /// Последняя принятая конфигурация surface (формат, размер, present mode, задержка).
    /// Переживает «отпустить окно»: возврат настраивает новую surface ею же.
    config: wgpu::SurfaceConfiguration,

    /// Запрошенные приложением настройки показа (до выбора режима по возможностям surface).
    presentation: SurfacePresentSettings,
}

/// Состояние surface окна.
///
/// Инвариант «одно окно — одна surface»: `Detached` означает, что у рендера нет ни
/// `wgpu::Surface`, ни swapchain-а на этом окне, и другой рендер может создать свою.
enum WindowSurface {
    /// Surface настроена конфигурацией `config`, буферы выдаются.
    Configured(wgpu::Surface<'static>),

    /// Surface есть, но не приняла ни новую, ни прежнюю конфигурацию (перенастройка на месте
    /// провалилась целиком). Каждый acquire пробует снова настроить её прежней `config`.
    Unconfigured(wgpu::Surface<'static>),

    /// Окно отпущено: surface (и swapchain) уничтожены, буферов нет до возврата окна.
    Detached,
}

/// Offscreen-цель: одна текстура, в которую пишется каждый кадр.
#[cfg(test)]
pub(crate) struct OffscreenPresentTarget {
    /// Текстура кадра (RENDER_ATTACHMENT | COPY_SRC для readback в тестах).
    texture: wgpu::Texture,

    /// Формат текстуры (тот же, под который собраны пайплайны рендера).
    format: wgpu::TextureFormat,

    /// Модель «окна» без окна: держит ли цель «окно» и действующие настройки показа.
    presentation: OffscreenPresentationState,
}

/// Буфер одного кадра, полученный из цели показа; лежит в билете `AcquiredSurfaceFrame`.
pub(crate) enum TargetFrame {
    /// Буфер swapchain окна; drop без present возвращает его swapchain-у (discard).
    Swapchain(wgpu::SurfaceTexture),

    /// Offscreen-текстура (клон handle-а на ту же текстуру).
    #[cfg(test)]
    Offscreen(wgpu::Texture),
}

impl TargetFrame {
    /// Текстура, в которую записывается кадр.
    pub(crate) fn texture(&self) -> &wgpu::Texture {
        match self {
            Self::Swapchain(surface_texture) => &surface_texture.texture,
            #[cfg(test)]
            Self::Offscreen(texture) => texture,
        }
    }
}

impl WindowPresentTarget {
    /// Собирает цель окна из уже сконфигурированной surface.
    pub(crate) const fn new(
        window: Arc<Window>,
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
        presentation: SurfacePresentSettings,
    ) -> Self {
        Self {
            window,
            surface: WindowSurface::Configured(surface),
            config,
            presentation,
        }
    }

    /// Получает текстуру swapchain; без окна — `PresentationDetached`, без рабочей
    /// конфигурации — сначала попытка восстановить прежнюю.
    fn acquire_current_texture(
        &mut self,
        device: &wgpu::Device,
    ) -> Result<wgpu::SurfaceTexture, RenderFrameDropReason> {
        if matches!(self.surface, WindowSurface::Detached) {
            return Err(RenderFrameDropReason::PresentationDetached);
        }
        if matches!(self.surface, WindowSurface::Unconfigured(_)) {
            self.recover_unconfigured_surface(device)?;
        }
        let WindowSurface::Configured(surface) = &self.surface else {
            // Недостижимо после проверок выше; честный отказ вместо паники.
            return Err(RenderFrameDropReason::SurfaceLost);
        };
        Self::acquire_configured_texture(surface, &self.config, device)
    }

    /// Один шаг получения текстуры swapchain с восстановлением устаревшей/потерянной surface.
    fn acquire_configured_texture(
        surface: &wgpu::Surface<'static>,
        config: &wgpu::SurfaceConfiguration,
        device: &wgpu::Device,
    ) -> Result<wgpu::SurfaceTexture, RenderFrameDropReason> {
        match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => Ok(frame),
            wgpu::CurrentSurfaceTexture::Outdated => {
                // Surface был уничтожен и воссоздан (например, при смене монитора).
                surface.configure(device, config);
                match surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(frame)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => Ok(frame),
                    other => {
                        tracing::error!(
                            "Failed to obtain surface texture afterward reconfigure: {:?}",
                            other
                        );
                        Err(RenderFrameDropReason::SurfaceOutdatedRecoveryFailed)
                    }
                }
            }
            // Таймаут — пропускаем кадр, не блокируем.
            wgpu::CurrentSurfaceTexture::Timeout => Err(RenderFrameDropReason::SurfaceTimeout),
            wgpu::CurrentSurfaceTexture::Occluded => {
                // Окно скрыто другим окном — пропускаем кадр.
                tracing::debug!("Surface occluded — skipping frame");
                Err(RenderFrameDropReason::SurfaceOccluded)
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                // Surface потерян: сначала пробуем штатный reconfigure текущей surface.
                // Если драйвер не восстановит surface, следующий redraw снова попадёт
                // сюда, и внешний lifecycle сможет пересоздать runtime через resumed/suspend.
                tracing::warn!("Surface lost — try out reconfigure");
                surface.configure(device, config);
                Err(RenderFrameDropReason::SurfaceLost)
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                // Validation error при получении surface texture — пропускаем кадр.
                tracing::warn!("Surface validation error — skipping frame");
                Err(RenderFrameDropReason::SurfaceValidation)
            }
        }
    }
}

#[cfg(test)]
impl OffscreenPresentTarget {
    /// Создаёт offscreen-цель размера `size` в формате `format`; настройки показа — умолчания
    /// (как у рендера без окна в conformance-стенде).
    pub(crate) fn new(device: &wgpu::Device, size: [u32; 2], format: wgpu::TextureFormat) -> Self {
        Self {
            texture: create_offscreen_texture(device, size, format),
            format,
            presentation: OffscreenPresentationState::new(SurfacePresentSettings::default()),
        }
    }

    /// Текстура последнего записанного кадра (для readback в тестах).
    pub(crate) const fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}

/// Текстура offscreen-цели нужного размера.
#[cfg(test)]
fn create_offscreen_texture(
    device: &wgpu::Device,
    size: [u32; 2],
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("fastiplayer offscreen present target"),
        size: wgpu::Extent3d {
            width: size[0].max(1),
            height: size[1].max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

impl PresentTarget {
    /// Текущий размер цели в пикселях.
    pub(crate) fn size(&self) -> [u32; 2] {
        match self {
            Self::Window(window_target) => {
                [window_target.config.width, window_target.config.height]
            }
            #[cfg(test)]
            Self::Offscreen(offscreen) => [offscreen.texture.width(), offscreen.texture.height()],
        }
    }

    /// Меняет размер цели. Нулевой размер (окно свёрнуто) игнорируется.
    ///
    /// Без настроенной surface (окно отпущено или конфигурация не принята) размер только
    /// запоминается: его применит возврат окна или восстановление конфигурации.
    #[instrument(skip(self, device), fields(width, height))]
    pub(crate) fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width == 0 || height == 0 {
            debug!("Resize skip: Zero size");
            return;
        }
        match self {
            Self::Window(window_target) => {
                window_target.config.width = width;
                window_target.config.height = height;
                if let WindowSurface::Configured(surface) = &window_target.surface {
                    surface.configure(device, &window_target.config);
                }
            }
            #[cfg(test)]
            Self::Offscreen(offscreen) => {
                offscreen.texture =
                    create_offscreen_texture(device, [width, height], offscreen.format);
            }
        }
        debug!(width, height, "Surface resized");
    }

    /// Получает буфер под кадр (у окна может ждать FIFO).
    pub(crate) fn acquire(
        &mut self,
        device: &wgpu::Device,
    ) -> Result<TargetFrame, RenderFrameDropReason> {
        match self {
            Self::Window(window_target) => window_target
                .acquire_current_texture(device)
                .map(TargetFrame::Swapchain),
            #[cfg(test)]
            Self::Offscreen(offscreen) => {
                offscreen.presentation.check_acquire()?;
                Ok(TargetFrame::Offscreen(offscreen.texture.clone()))
            }
        }
    }

    /// Перенастройка показа на месте (сессия 02a). Вызывать только без живого буфера кадра.
    pub(crate) fn reconfigure_presentation(
        &mut self,
        gpu: &GpuContext,
        settings: SurfacePresentSettings,
    ) -> Result<PresentationReconfigured, PresentationReconfigureError> {
        match self {
            Self::Window(window_target) => window_target.reconfigure_presentation(gpu, settings),
            #[cfg(test)]
            Self::Offscreen(offscreen) => offscreen.presentation.reconfigure(settings),
        }
    }

    /// Отпускает окно (сессия 02a). Вызывать только без живого буфера кадра.
    pub(crate) fn detach_presentation(&mut self) -> PresentationDetachOutcome {
        match self {
            Self::Window(window_target) => window_target.detach_presentation(),
            #[cfg(test)]
            Self::Offscreen(offscreen) => offscreen.presentation.detach(),
        }
    }

    /// Возвращает окно (сессия 02a): новая surface с прежней конфигурацией.
    pub(crate) fn reattach_presentation(
        &mut self,
        gpu: &GpuContext,
    ) -> Result<PresentationReattachOutcome, PresentationReattachError> {
        match self {
            Self::Window(window_target) => window_target.reattach_presentation(gpu),
            #[cfg(test)]
            Self::Offscreen(offscreen) => Ok(offscreen.presentation.reattach()),
        }
    }

    /// Сообщает окну, что сейчас будет present (Wayland frame callback, cpu-parity 07a).
    pub(crate) fn pre_present_notify(&self) {
        match self {
            Self::Window(window_target) => window_target.window.pre_present_notify(),
            #[cfg(test)]
            Self::Offscreen(_) => {}
        }
    }

    /// Показывает записанный кадр. У offscreen-цели показ — no-op: кадр уже в текстуре.
    pub(crate) fn present(queue: &wgpu::Queue, frame: TargetFrame) {
        match frame {
            // wgpu 30: `SurfaceTexture::present()` заменён на `Queue::present`.
            TargetFrame::Swapchain(surface_texture) => queue.present(surface_texture),
            #[cfg(test)]
            TargetFrame::Offscreen(_) => {}
        }
    }

    /// Offscreen-цель для readback в тестах; `None` у окна.
    #[cfg(test)]
    pub(crate) const fn offscreen(&self) -> Option<&OffscreenPresentTarget> {
        match self {
            Self::Window(_) => None,
            Self::Offscreen(offscreen) => Some(offscreen),
        }
    }
}
