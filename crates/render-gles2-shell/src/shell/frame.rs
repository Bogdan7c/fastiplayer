//! Кадр GLES2-рендера: получение буфера и запись/показ кадра.
//!
//! Порядок стадий — как у wgpu (`render-wgpu-shell::shell::record_and_present_frame`), а
//! проходы внутри кадра задаёт сборщик `gles2-egui` (`FrameCompositor`):
//!
//! 1. изменения текстур egui применяются ПЕРВЫМ шагом и в отклонённом кадре (чужой билет, чужой
//!    ключ, невозможный план видео — `settle_content_without_frame`): приложение второй раз их
//!    не пришлёт;
//! 2. билет буфера и ключ видеокадра открывает только выдавший их рендер (чужие — `Failed`);
//! 3. картинка ключа возвращается на видеослой, если ключ уже не «текущий» (см.
//!    [`crate::video_import`]);
//! 4. сборщик рисует фон (если подложка не закрывает цель), видео, субтитры, egui, маску углов
//!    и показывает кадр;
//! 5. после КАЖДОГО показанного кадра один fence: видеослой освобождает слоты кольца, привязка
//!    освобождения возвращает кадры декодера (решения владельца 30 и 32).
//!
//! У GLES2 нет отдельного «получить буфер swapchain»: `eglSwapBuffers` блокируется на показе, а
//! начало кадра — привязка цели. Поэтому билет — лёгкий маркер, а `acquire_surface_frame`
//! приводит цель к размеру кадра и проверяет, что окно есть.

use std::time::{Duration, Instant};

use gles2_egui::{
    BackgroundFillOutcome, FrameLayerStack, Gles2Error, LayersDamage, PixelExtent, PresentKind,
    PresentedUiFrame, UiContent, UiFrameOutcome, UiFrameRequest, UiScreen,
};
use gles2_video::{VideoDrawOutcome, VideoUnderlay};
use render_core::{RenderViewport, RenderableFrame};
use render_shell_api::{
    AcquiredSurfaceFrame, EguiFrameContent, RenderFrameDropReason, RenderFrameFailure,
    RenderFrameInput, RenderFrameOutcome, RenderScreenDescriptor, RenderableFrameKey,
    SurfaceFrameAcquire,
};

use super::Gles2Renderer;
use super::frame_fence::{FrameVideoUse, FrameWorkRetirement};
use super::frame_layers::ShellUnderlay;
use super::frame_outcome::{drop_reason_of, failure_of, skip_reason_of};
use super::layers::GlLayers;
use super::layers_damage::{LayersSnapshot, SubtitlePlacement, VideoLayerState};
use crate::video_import::Gles2PreparedFrame;
use crate::{
    SubtitleOverlayLayer, VideoDrawPlan, VideoPlacement, render_frame_timing, renderable_frame,
    video_pass_diagnostics,
};

/// Заливка фона кадра: чёрный непрозрачный, как очистка цели у wgpu-видео.
const FRAME_BACKGROUND: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// Маркер билета буфера: у GLES2 буфера как объекта нет, билет только доказывает «буфер
/// получен этим рендером» (паспорт рендера проверяет `into_buffer`).
struct SurfaceTicket;

/// Ключ видеокадра, открытый этим рендером.
pub(super) struct OpenedVideoKey<'key> {
    pub(super) key: &'key RenderableFrameKey,
    pub(super) prepared: &'key Gles2PreparedFrame,
}

/// План рисования видео в этом кадре и кадр render-core, из которого он построен.
struct VideoPass {
    renderable: RenderableFrame,
    plan: VideoDrawPlan,
}

impl Gles2Renderer {
    /// Приводит цель к размеру кадра и выдаёт билет; без окна или у свёрнутого окна — причина
    /// пропуска.
    pub(super) fn acquire_target_frame(
        &mut self,
        screen: RenderScreenDescriptor,
    ) -> SurfaceFrameAcquire {
        let acquire_started_at = Instant::now();
        if self.presentation.is_detached(&self.context) {
            return SurfaceFrameAcquire::Dropped(RenderFrameDropReason::PresentationDetached);
        }
        let [width, height] = screen.size_in_pixels;
        if width == 0 || height == 0 {
            return SurfaceFrameAcquire::Dropped(RenderFrameDropReason::SurfaceOccluded);
        }
        let wanted_extent = PixelExtent { width, height };
        if self.context.extent() != wanted_extent {
            match self.context.resize(wanted_extent) {
                Ok(()) => {}
                // Потерю контекста честно сообщит `render_frame` (`Failed`): иначе приложение
                // вечно получало бы «поверхность потеряна» и не узнало бы причину.
                Err(Gles2Error::ContextLost(_)) => {}
                Err(error) => {
                    log::warn!(
                        "GLES2: цель не приведена к размеру кадра {width}x{height}: {error}"
                    );
                    return SurfaceFrameAcquire::Dropped(RenderFrameDropReason::SurfaceLost);
                }
            }
        }
        SurfaceFrameAcquire::Acquired(AcquiredSurfaceFrame::new(
            self.instance_id,
            SurfaceTicket,
            acquire_started_at.elapsed(),
        ))
    }

    /// Записывает и показывает кадр (см. документацию модуля).
    pub(super) fn record_and_present_frame(
        &mut self,
        input: RenderFrameInput<'_>,
    ) -> RenderFrameOutcome {
        let renderer_started_at = Instant::now();
        let RenderFrameInput {
            surface,
            video_frame,
            egui,
            screen,
            video_viewport,
            video_exclusion_rects,
            subtitle_overlay,
        } = input;
        let content = ui_content_of(egui);

        let acquire_elapsed = match surface.into_buffer::<SurfaceTicket>(self.instance_id) {
            Ok(opened) => opened.acquire_elapsed,
            Err(_foreign) => {
                return self.reject_frame(content, RenderFrameFailure::foreign_surface_frame());
            }
        };
        let opened_video = match self.open_video_key(video_frame) {
            Ok(opened_video) => opened_video,
            Err(failure) => return self.reject_frame(content, failure),
        };
        let video_pass = match &opened_video {
            Some(opened) => {
                if let Err(failure) = self.ensure_key_on_layer(opened) {
                    return self.reject_frame(content, failure);
                }
                match self.build_video_pass(
                    opened.key,
                    screen,
                    video_viewport,
                    &video_exclusion_rects,
                ) {
                    Ok(video_pass) => Some(video_pass),
                    Err(failure) => return self.reject_frame(content, failure),
                }
            }
            None => None,
        };

        let [width, height] = screen.size_in_pixels;
        let visible_area = video_viewport.clamp_to_surface(width, height);
        let Some(GlLayers {
            compositor,
            video,
            subtitles,
        }) = self.layers.as_mut()
        else {
            return RenderFrameOutcome::Failed(renderer_shut_down());
        };
        let frame_video_use = if video_pass.is_some() {
            FrameVideoUse::WithVideo
        } else {
            FrameVideoUse::WithoutVideo
        };
        // Менялись ли слои с прошлого показанного кадра: решается ДО рисования (слой субтитров
        // обновляет своё состояние в `render`).
        let layers_snapshot = LayersSnapshot {
            screen_size: screen.size_in_pixels,
            video: video_pass.as_ref().map(|pass| VideoLayerState {
                shown: self.shown.current(),
                plan: pass.plan.clone(),
            }),
            subtitles: subtitle_overlay
                .as_ref()
                .map(|overlay_frame| SubtitlePlacement {
                    origins: overlay_frame.canvas_origins(),
                    visible_area,
                }),
        };
        let subtitle_pixels_unchanged = subtitle_overlay
            .as_ref()
            .is_some_and(|overlay_frame| subtitles.has_prepared(overlay_frame.overlay));
        let layers_damage = self.presented_layers.damage_of(
            &layers_snapshot,
            subtitle_pixels_unchanged,
            video.has_pending_hdr_table(),
        );
        let (rendered, video_outcome) = {
            let mut underlay = match &video_pass {
                Some(pass) => ShellUnderlay::Video(VideoUnderlay::new(video, pass.plan.request())),
                None => {
                    // Видео нет: таблицу HDR, которую строил фон, ждать некому, а рендер не
                    // должен просить кадры ради неё (`needs_follow_up_frame`).
                    video.abandon_pending_hdr_table();
                    ShellUnderlay::NoVideo
                }
            };
            let mut overlays = SubtitleOverlayLayer::new(subtitles, subtitle_overlay, visible_area);
            let rendered = {
                let mut stack = FrameLayerStack::new(&mut underlay, &mut overlays);
                compositor.render_with_layers_damage(
                    &mut self.context,
                    UiFrameRequest {
                        screen: UiScreen {
                            size_in_pixels: screen.size_in_pixels,
                            pixels_per_point: screen.pixels_per_point,
                        },
                        content,
                        background: FRAME_BACKGROUND,
                        layers: &mut stack,
                    },
                    layers_damage,
                )
            };
            (rendered, underlay.video_outcome())
        };

        match &rendered {
            Ok(UiFrameOutcome::Presented(_)) => {
                self.presented_layers.record_presented(layers_snapshot);
                if layers_damage == LayersDamage::Unchanged {
                    self.counters.frames_with_unchanged_layers += 1;
                }
            }
            // Не показан: слои заднего буфера и «прошлого кадра» больше не совпадают.
            _ => self.presented_layers.forget(),
        }
        match rendered {
            Ok(UiFrameOutcome::Presented(presented)) => self.complete_presented_frame(
                presented,
                video_pass.as_ref(),
                video_outcome,
                acquire_elapsed + renderer_started_at.elapsed(),
            ),
            Ok(UiFrameOutcome::Skipped(reason)) => {
                RenderFrameOutcome::Dropped(skip_reason_of(reason))
            }
            Ok(UiFrameOutcome::Dropped(reason)) => {
                log_drop(reason);
                RenderFrameOutcome::Dropped(drop_reason_of(reason))
            }
            Err(error) => {
                log::error!("GLES2: кадр не нарисован: {error}");
                // GL-команды с видео могли уйти до ошибки: работа прикрывается fence как у
                // показанного кадра (корень: старый fence в привязке уже сработал).
                self.retire_work_of_failed_frame(&error, frame_video_use);
                RenderFrameOutcome::Failed(failure_of(&error))
            }
        }
    }

    /// Забыть, что показано на цели: у новой EGLSurface буферы пустые, и ближайший кадр обязан
    /// быть полным, как бы ни отвечал драйвер про возраст буфера.
    pub(super) fn forget_presented_state(&mut self) {
        self.presented_layers.forget();
        if let Some(layers) = self.layers.as_mut() {
            layers.compositor.forget_damage_history();
        }
    }

    /// Открывает ключ видеокадра: чужой рендер — `Failed(ForeignVideoFrame)`.
    fn open_video_key<'key>(
        &self,
        video_frame: Option<&'key RenderableFrameKey>,
    ) -> Result<Option<OpenedVideoKey<'key>>, RenderFrameFailure> {
        let Some(key) = video_frame else {
            return Ok(None);
        };
        match key.prepared_for::<Gles2PreparedFrame>(self.instance_id) {
            Some(prepared) => Ok(Some(OpenedVideoKey { key, prepared })),
            None => Err(RenderFrameFailure::foreign_video_frame()),
        }
    }

    /// План рисования видео для ключа; невозможный план — отказ кадра с причиной.
    fn build_video_pass(
        &self,
        key: &RenderableFrameKey,
        screen: RenderScreenDescriptor,
        video_viewport: RenderViewport,
        video_exclusion_rects: &[RenderViewport],
    ) -> Result<VideoPass, RenderFrameFailure> {
        let renderable = renderable_frame(key.decoded_frame()).map_err(|error| {
            log::error!("GLES2: кадр видео не годится для рисования: {error}");
            RenderFrameFailure::new(error.to_string())
        })?;
        let [width, height] = screen.size_in_pixels;
        let plan = VideoDrawPlan::for_frame(
            &renderable,
            VideoPlacement {
                surface_size: (width, height),
                video_viewport,
                exclusions: video_exclusion_rects,
            },
            self.live_settings.settings(),
        )
        .map_err(|error| {
            log::error!("GLES2: план рисования видео не построен: {error}");
            RenderFrameFailure::new(error.to_string())
        })?;
        Ok(VideoPass { renderable, plan })
    }

    /// Кадр отказан после получения содержимого: изменения текстур UI всё равно применяются.
    fn reject_frame(
        &mut self,
        content: UiContent,
        failure: RenderFrameFailure,
    ) -> RenderFrameOutcome {
        if let Some(layers) = self.layers.as_mut()
            && let Err(error) = layers
                .compositor
                .settle_content_without_frame(&self.context, content)
        {
            log::warn!("GLES2: изменения текстур UI отклонённого кадра не применены: {error}");
        }
        RenderFrameOutcome::Failed(failure)
    }

    /// Кадр показан: fence кадра, диагностика, счётчики и тайминги.
    fn complete_presented_frame(
        &mut self,
        presented: PresentedUiFrame,
        video_pass: Option<&VideoPass>,
        video_outcome: Option<VideoDrawOutcome>,
        renderer_elapsed: Duration,
    ) -> RenderFrameOutcome {
        match self.retire_submitted_frame_work() {
            FrameWorkRetirement::FencePublished | FrameWorkRetirement::FinishedSynchronously => {
                // Проход показа уже в очереди GPU: теперь аппаратный декодер может встать за ним.
                self.signal_present_pass_submitted();
            }
            FrameWorkRetirement::ContextLost => {
                return RenderFrameOutcome::Failed(RenderFrameFailure::new(
                    "GLES2 frame fence failed: context lost",
                ));
            }
            FrameWorkRetirement::Unprotected(error) => {
                log::error!("GLES2: fence кадра показа не поставлен: {error}");
                return RenderFrameOutcome::Failed(RenderFrameFailure::new(format!(
                    "GLES2 frame fence failed: {error}"
                )));
            }
        }

        if matches!(video_outcome, Some(VideoDrawOutcome::ClearedWithoutFrame)) {
            // Учёт «что на видеослое» разошёлся со слоем: показан чёрный кадр вместо картинки.
            log::error!("GLES2: видеослой не имел кадра, хотя ключ был подготовлен");
        }
        self.diagnostics = video_pass.map_or_else(Default::default, |pass| {
            video_pass_diagnostics(&pass.renderable, &pass.plan)
        });
        self.counters.frames_presented += 1;
        if self.counters.frames_presented == 1
            && presented.present_kind != PresentKind::OffscreenFlush
        {
            // Один раз на первом кадре окна: что реально дал драйвер для частичного показа.
            log::info!(
                "GLES2 first window frame: {}; present kind {:?}, repaint {:?}",
                gles2_egui::describe_partial_present_support(self.context.capabilities()),
                presented.present_kind,
                presented.repaint
            );
        }
        match presented.work.background {
            BackgroundFillOutcome::Cleared => self.counters.background_fills += 1,
            BackgroundFillOutcome::SkippedUnderlayCoversTarget => {
                self.counters.background_fills_skipped += 1;
            }
        }
        RenderFrameOutcome::Presented(render_frame_timing(&presented.timings, renderer_elapsed))
    }
}

/// Содержимое UI контракта → содержимое сборщика кадра.
fn ui_content_of(egui: EguiFrameContent) -> UiContent {
    match egui {
        EguiFrameContent::Fresh {
            paint_jobs,
            textures_delta,
        } => UiContent::Fresh {
            primitives: paint_jobs,
            textures_delta,
        },
        EguiFrameContent::ReusePrevious => UiContent::ReusePrevious,
    }
}

/// Кадр после уничтожения GL-объектов (недостижимо вне `Drop`): честный отказ вместо паники.
pub(super) fn renderer_shut_down() -> RenderFrameFailure {
    RenderFrameFailure::new("GLES2 renderer is shut down")
}

/// Лог причины отброшенного кадра: рассинхрон текстур — предупреждение, остальное — отладка.
fn log_drop(reason: gles2_egui::UiFrameDropReason) {
    match reason {
        gles2_egui::UiFrameDropReason::UiTexturesOutOfSync { texture_id } => log::warn!(
            "UI texture edit for a texture the renderer does not have ({texture_id:?}); \
             the app must send the full UI texture set"
        ),
        gles2_egui::UiFrameDropReason::UiTexturesNotApplied => log::warn!(
            "UI texture edits were not applied (GL unavailable); the app must send the full set"
        ),
        gles2_egui::UiFrameDropReason::RetainedUiUnavailable(why) => log::debug!(
            "Retained egui frame unavailable ({why:?}); the app must redraw with a fresh UI pass"
        ),
    }
}
