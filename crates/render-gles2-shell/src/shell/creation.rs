//! Создание GLES2-рендера: на окне приложения или без окна (тесты и снимки).

use std::sync::Arc;

use gles2_egui::{
    Gles2Context, OffscreenPixels, PixelExtent, ReadbackError, SurfaceSize, TierStatus,
    WindowSource,
};
use render_core::RenderDiagnostics;
use render_shell_api::{
    RenderSurfaceSize, RendererCreateError, RendererCreateStage, RendererInstanceId,
    SurfacePresentSettings,
};

use super::create_error::{pipeline_creation_error, renderer_create_error, required_feature_for};
use super::layers::GlLayers;
use super::live_settings::LiveSettingsState;
use super::presentation::{PresentationController, context_settings_for};
use super::{Gles2FrameCounters, Gles2Renderer};
use crate::capabilities::{HardwareZeroCopy, gles2_render_capabilities_for};
use crate::video_import::{ImportEpoch, OwnedReleaseBinding, ShownFrameTracker};

/// Размер цели до первого сообщения окна о настоящем размере: `resize`/`acquire_surface_frame`
/// приводят цель к размеру кадра раньше, чем что-то рисуется.
const INITIAL_WINDOW_EXTENT: PixelExtent = PixelExtent {
    width: 1,
    height: 1,
};

impl Gles2Renderer {
    /// Рендер на окне (Wayland или X11). `window` — например `Arc<winit::window::Window>`,
    /// приведённый к `Arc<dyn WindowSource>`.
    ///
    /// Режим показа `Mailbox` в EGL не существует: рендер работает с вертикальной развёрткой
    /// (при создании это только лог, при `reconfigure_presentation` — явный исход).
    ///
    /// # Errors
    /// [`RendererCreateError`] со стадией: нет libEGL / нужной версии / конфигурации GLES2,
    /// окно не привязалось к EGL, контекст не создан, не собрались программы.
    pub fn new(
        window: Arc<dyn WindowSource>,
        surface_present: SurfacePresentSettings,
    ) -> Result<Self, RendererCreateError> {
        let context = Gles2Context::for_window(
            window,
            INITIAL_WINDOW_EXTENT,
            context_settings_for(surface_present),
        )
        .map_err(|error| renderer_create_error(&error))?;
        Self::assemble(context, PresentationController::for_window(surface_present))
    }

    /// Рендер без окна: кадры пишутся в цель `size`, пиксели читает
    /// [`Self::read_offscreen_pixels`]. Для автотестов и снимков; «отпуск окна» моделируется.
    ///
    /// # Errors
    /// Как у [`Self::new`]; нулевой размер — стадия `SurfaceCreation`.
    pub fn new_offscreen(
        size: RenderSurfaceSize,
        surface_present: SurfacePresentSettings,
    ) -> Result<Self, RendererCreateError> {
        let target = SurfaceSize::new(size.width, size.height).ok_or_else(|| {
            RendererCreateError::new(
                RendererCreateStage::SurfaceCreation,
                format!(
                    "offscreen target must not be empty, got {}x{}",
                    size.width, size.height
                ),
            )
        })?;
        let context =
            Gles2Context::offscreen(target).map_err(|error| renderer_create_error(&error))?;
        Self::assemble(
            context,
            PresentationController::for_offscreen(surface_present),
        )
    }

    /// Пиксели последнего кадра рендера без окна (RGBA8, строка 0 — верх).
    ///
    /// # Errors
    /// [`ReadbackError::NotOffscreen`] у рендера на окне; отказ GL.
    pub fn read_offscreen_pixels(&self) -> Result<OffscreenPixels, ReadbackError> {
        self.context.read_offscreen_pixels()
    }

    /// Собирает рендер поверх готового контекста: проверка базового уровня, GL-объекты,
    /// возможности по найденному на драйвере.
    fn assemble(
        context: Gles2Context,
        presentation: PresentationController,
    ) -> Result<Self, RendererCreateError> {
        if let TierStatus::Missing(missing) = &context.capabilities().tiers().baseline {
            let named = missing
                .iter()
                .map(|requirement| {
                    required_feature_for(requirement)
                        .specification_name()
                        .to_owned()
                })
                .collect::<Vec<_>>()
                .join(", ");
            return Err(pipeline_creation_error(
                "GLES2 baseline requirements are not met",
                &named,
            ));
        }
        let layers = GlLayers::create(&context)?;
        let hardware = HardwareZeroCopy::for_capabilities(context.capabilities());
        let capabilities = gles2_render_capabilities_for(
            layers.video.shader_precision(),
            context.capabilities().max_texture_size(),
            hardware,
        );
        let instance_id = RendererInstanceId::allocate();
        log::info!(
            "GLES2 renderer {} is ready: {} ({})",
            instance_id.value(),
            capabilities.display_name,
            context.capabilities().gl_renderer()
        );
        Ok(Self {
            instance_id,
            capabilities,
            hardware,
            layers: Some(layers),
            presentation,
            live_settings: LiveSettingsState::default(),
            diagnostics: RenderDiagnostics::default(),
            active_import: None,
            import_epoch: ImportEpoch::INITIAL,
            release: OwnedReleaseBinding::new(),
            shown: ShownFrameTracker::default(),
            counters: Gles2FrameCounters::default(),
            context,
        })
    }
}
