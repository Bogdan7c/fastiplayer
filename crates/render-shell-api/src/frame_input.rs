//! Вход одного полного кадра в нейтральной форме.
//!
//! Отличие от прежнего wgpu-входа (`render-wgpu-shell::RenderFrameInput`):
//! - нет окна winit: рендер получает окно при создании и сам уведомляет его перед показом;
//! - видео — непрозрачный [`RenderableFrameKey`] вместо wgpu-текстур;
//! - буфер swapchain — нейтральный билет [`AcquiredSurfaceFrame`].

use render_core::RenderViewport;

use crate::{
    AcquiredSurfaceFrame, EguiFrameContent, RenderScreenDescriptor, RenderableFrameKey,
    SubtitleOverlayFrame, WindowCornerMask,
};

/// Входные данные одного полного кадра рендера.
///
/// App layer отвечает за egui tessellation, выбор видеокадра и субтитров; рендер получает
/// готовый пакет и записывает/показывает кадр.
#[derive(Debug)]
pub struct RenderFrameInput<'frame> {
    /// Буфер swapchain, полученный `acquire_surface_frame` ДО выбора видеокадра
    /// (сессия 06a): ожидание FIFO не делает выбранный кадр старше на vsync.
    pub surface: AcquiredSurfaceFrame,

    /// Видеокадр; `None` означает, что target нужно очистить в чёрный.
    pub video_frame: Option<&'frame RenderableFrameKey>,

    /// Свежие egui-примитивы либо просьба повторить прежние.
    pub egui: EguiFrameContent,

    /// Размер target-а и UI scale.
    pub screen: RenderScreenDescriptor,

    /// Content viewport, по которому video pass сохраняет aspect ratio (пиксели surface).
    pub video_viewport: RenderViewport,

    /// Области, где video pass не должен рисовать кадр (сейчас — только левый sidebar).
    pub video_exclusion_rects: Vec<RenderViewport>,

    /// Разрешённая app-слоем форма текущего desktop-кадра.
    pub window_corner_mask: WindowCornerMask,

    /// Субтитры между video и egui; `None` — рисовать нечего.
    pub subtitle_overlay: Option<SubtitleOverlayFrame<'frame>>,
}
