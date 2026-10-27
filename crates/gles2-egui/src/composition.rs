//! Сборка кадра GLES2 ([`FrameCompositor`]): порядок проходов — как у wgpu-шелла
//! (`render-wgpu-shell::shell::record_and_present_frame`).
//!
//! 1. Изменения текстур egui (`set`) — первым шагом, до любых проверок (наследство 02a: это
//!    состояние рендера, оно не теряется в отклонённом кадре).
//! 2. Проверки содержимого UI: рассинхрон текстур → `Dropped(UiTexturesOutOfSync)` с немедленным
//!    `free`; повтор без удержанного кадра или на другом экране → `Dropped(RetainedUiUnavailable)`.
//! 3. `begin_frame`. Нет кадра (окно отпущено, 0×0) → `Skipped`, `free` сразу.
//! 4. Свежий UI → буферы GPU (до видео, как `update_buffers` у wgpu).
//! 5. Фон: заливка (её пропускает подложка, закрывающая всю цель, — [`UnderlayCoverage`]), затем
//!    слой подложки (видео — с сессии 05) — [`FrameLayers::draw_underlay`].
//! 6. Оверлеи между видео и egui (субтитры — сессия 08) — [`FrameLayers::draw_overlays`].
//! 7. egui.
//! 8. Показ, затем `free` кадра.
//!
//! Альфа цели. Окно всегда непрозрачное (скругление углов удалено, решение 57 плана
//! gles2-renderer). Если у буфера кадра есть канал альфы (`FramebufferAlpha::Present`), альфа
//! обязана остаться 1, иначе композитор просветит окно. Заливка фона здесь пишет альфу 1 всегда, а
//! смешивание egui по альфе (`ONE_MINUS_DST_ALPHA, ONE`) единицу не меняет. Слои обязаны рисовать
//! непрозрачно (видео так и рисует) — это их контракт.

use std::time::{Duration, Instant};

use epaint::textures::TexturesDelta;
use epaint::{ClippedPrimitive, TextureId};
use thiserror::Error;

use crate::context::{GlAccessError, Gles2Context};
use crate::error::Gles2Error;
use crate::frame::{ActiveFrame, FrameSkipReason, FrameStart, Gles2FrameTimings};
use crate::painter::{
    EguiPainter, EguiPainterOptions, PainterError, RetainedUiUnavailable, TextureApplyReport,
    TextureFreeOutcome, UiPaintWork, UiPrepareWork, UiScreen, UiTextureUpload,
};

/// Содержимое UI кадра (аналог `render_shell_api::EguiFrameContent`).
pub enum UiContent {
    /// Новый проход egui.
    Fresh {
        /// Примитивы `Context::tessellate`.
        primitives: Vec<ClippedPrimitive>,
        /// Изменения текстур прохода.
        textures_delta: TexturesDelta,
    },
    /// Повторить прошлый UI без прохода egui (тот же экран, иначе отказ).
    ReusePrevious,
}

/// Слои кадра под и над видео — точка расширения для `gles2-video` (05) и субтитров (08).
/// Сборщик вызывает их в фиксированном порядке; контекст текущий, цель привязана.
///
/// Контракт: оставить VAO отвязанным, текстурный блок 0 и программу — как угодно (художник и
/// и слои задают своё явно). На непрозрачной цели рисовать непрозрачно (альфа 1).
pub trait FrameLayers {
    /// Подложка поверх заливки фона (видео). По умолчанию — ничего.
    fn draw_underlay(&mut self, frame: &ActiveFrame<'_>) -> Result<(), FrameLayerError> {
        let _ = frame;
        Ok(())
    }

    /// Оверлеи между подложкой и egui (субтитры). По умолчанию — ничего.
    fn draw_overlays(&mut self, frame: &ActiveFrame<'_>) -> Result<(), FrameLayerError> {
        let _ = frame;
        Ok(())
    }

    /// Закроет ли подложка всю цель (см. [`UnderlayCoverage`]). По умолчанию — нет.
    fn underlay_coverage(&self) -> UnderlayCoverage {
        UnderlayCoverage::LeavesTargetPartlyUntouched
    }
}

/// Закрывает ли подложка слоёв всю цель непрозрачными пикселями.
///
/// Сборщик кадра спрашивает это до заливки фона: если подложка гарантированно перекрасит каждый
/// пиксель цели (альфа 1), заливка фона — лишняя работа GPU (на Intel Ironlake полная заливка
/// 1440×900 стоит ~2 мс на кадр, `results/05.md` плана gles2-renderer). Ответ — обещание слоя:
/// слой, который обещал `PaintsWholeTarget` и не покрыл часть цели, оставит там мусор прошлого
/// кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnderlayCoverage {
    /// Подложки нет или она рисует не всю цель: фон заливается всегда.
    LeavesTargetPartlyUntouched,
    /// Подложка сама заливает всю цель (видео чёрным вне картинки): заливка фона не нужна.
    PaintsWholeTarget,
}

/// Что сборщик сделал с фоном кадра (для счётчиков и тестов «лишней заливки нет»).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackgroundFillOutcome {
    /// Цель залита цветом фона перед слоями.
    Cleared,
    /// Подложка обещала покрыть всю цель ([`UnderlayCoverage::PaintsWholeTarget`]): не заливали.
    SkippedUnderlayCoversTarget,
}

/// Слоёв нет: только заливка фона (до сессии 05 вместо видео).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFrameLayers;

impl FrameLayers for NoFrameLayers {}

/// Отказ слоя.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("frame layer failed: {message}")]
pub struct FrameLayerError {
    /// Что случилось.
    pub message: String,
}

/// Запрос кадра.
pub struct UiFrameRequest<'layers> {
    /// Экран egui (размер цели и масштаб).
    pub screen: UiScreen,
    /// UI.
    pub content: UiContent,
    /// Заливка фона, premultiplied RGBA 0..=1 (у плеера — чёрный, как очистка wgpu-видео).
    pub background: [f32; 4],
    /// Слои под и над видео.
    pub layers: &'layers mut dyn FrameLayers,
}

/// Почему кадр отброшен до рисования (аналоги `render_shell_api::RenderFrameDropReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UiFrameDropReason {
    /// Повтор невозможен: приложение должно сделать полный проход egui.
    RetainedUiUnavailable(RetainedUiUnavailable),
    /// Правка текстуры, которой нет: приложение должно прислать полный набор текстур.
    UiTexturesOutOfSync {
        /// Первая такая текстура.
        texture_id: TextureId,
    },
    /// Изменения текстур не применены: GL недоступен (окно отпущено без surfaceless-контекста).
    /// Ответ приложения тот же — полный набор текстур.
    UiTexturesNotApplied,
}

/// Тайминги стадий. Имена — как у `render_shell_api::RenderFrameStageTimings`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UiFrameTimings {
    /// Изменения текстур egui.
    pub egui_texture_update: Duration,
    /// Вершины и индексы egui.
    pub egui_buffer_update: Duration,
    /// Привязка контекста и цели.
    pub surface_acquire: Duration,
    /// Заливка и подложка (видео).
    pub video_render: Duration,
    /// Оверлеи (субтитры).
    pub subtitle_overlay: Duration,
    /// Проход egui.
    pub egui_render: Duration,
    /// Показ.
    pub surface_present: Duration,
}

/// Работа показанного кадра (для счётчиков и тестов «повтор ничего не загружает»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiFrameWork {
    /// Изменения текстур (`None` у повтора: их нет).
    pub textures: Option<TextureApplyReport>,
    /// Сборка свежего UI (`None` у повтора).
    pub prepare: Option<UiPrepareWork>,
    /// Что сделано с фоном: залит или пропущен, потому что подложка закрывает цель.
    pub background: BackgroundFillOutcome,
    /// Рисование egui.
    pub paint: UiPaintWork,
    /// `free` кадра.
    pub texture_free: TextureFreeOutcome,
}

/// Показанный кадр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentedUiFrame {
    /// Тайминги.
    pub timings: UiFrameTimings,
    /// Работа.
    pub work: UiFrameWork,
}

/// Итог кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "итог живёт один кадр и возвращается по значению; Box дал бы аллокацию на каждый \
              кадр старого железа ради экономии стека на редких отказах"
)]
pub enum UiFrameOutcome {
    /// Кадр показан.
    Presented(PresentedUiFrame),
    /// Кадра нет (окно отпущено, 0×0). Изменения текстур применены, `free` выполнен.
    Skipped(FrameSkipReason),
    /// Кадр отброшен до рисования; приложение должно ответить по причине.
    Dropped(UiFrameDropReason),
}

/// Ошибки кадра.
#[derive(Debug, Error)]
pub enum UiFrameError {
    /// Контекст: потеря, отказ EGL.
    #[error(transparent)]
    Context(#[from] Gles2Error),
    /// Художник.
    #[error(transparent)]
    Painter(#[from] PainterError),
    /// Слой.
    #[error(transparent)]
    Layer(#[from] FrameLayerError),
}

/// Сборщик кадра: художник egui поверх контекста.
#[derive(Debug)]
pub struct FrameCompositor {
    painter: EguiPainter,
}

/// Проверенное содержимое UI, готовое к рисованию.
enum PendingUi {
    Fresh {
        primitives: Vec<ClippedPrimitive>,
        textures_delta: TexturesDelta,
        textures: TextureApplyReport,
    },
    Reused,
}

impl FrameCompositor {
    /// Художник в этом контексте.
    pub fn new(context: &Gles2Context, options: EguiPainterOptions) -> Result<Self, UiFrameError> {
        let painter = EguiPainter::new(context, options)?;
        Ok(Self { painter })
    }

    /// Художник (счётчики, `max_texture_side`, проверка повтора).
    pub fn painter(&self) -> &EguiPainter {
        &self.painter
    }

    /// Можно ли повторить прошлый UI на этом экране (решение «полный проход или повтор» —
    /// до прохода egui, как `RenderShell::can_reuse_retained_ui`).
    pub fn can_reuse_retained_ui(&self, screen: UiScreen) -> bool {
        self.painter.retained_availability(screen).is_ok()
    }

    /// Нарисовать и показать кадр.
    pub fn render(
        &mut self,
        context: &mut Gles2Context,
        request: UiFrameRequest<'_>,
    ) -> Result<UiFrameOutcome, UiFrameError> {
        let UiFrameRequest {
            screen,
            content,
            background,
            layers,
        } = request;
        let mut timings = UiFrameTimings::default();
        let pending = match self.accept_content(context, content, screen, &mut timings)? {
            Ok(pending) => pending,
            Err(drop_reason) => return Ok(UiFrameOutcome::Dropped(drop_reason)),
        };
        let frame = match context.begin_frame() {
            Ok(FrameStart::Ready(frame)) => frame,
            Ok(FrameStart::Skipped(reason)) => {
                self.finish_unpainted(context, pending)?;
                return Ok(UiFrameOutcome::Skipped(reason));
            }
            Err(error) => {
                // Основная ошибка — контекста; отказ `free` вторичен и только в лог.
                if let Err(free_error) = self.finish_unpainted(context, pending) {
                    log::warn!(
                        "frame compositor: texture free after failed frame start: {free_error}"
                    );
                }
                return Err(error.into());
            }
        };
        let drawn = self.draw_frame(
            &frame,
            DrawPlan {
                screen,
                background,
                layers,
            },
            pending,
            &mut timings,
        );
        let DrawnFrame {
            background,
            textures,
            prepare,
            textures_delta,
            paint,
        } = match drawn {
            Ok(drawn) => drawn,
            Err(failed) => {
                // Кадр выброшен (без показа): `frame` дальше не используется, контекст снова
                // свободен для `free`.
                let FailedDraw {
                    error,
                    textures_delta,
                } = *failed;
                self.free_textures_after_failure(context, textures_delta);
                return Err(error);
            }
        };
        let presented = frame.present();
        let texture_free = self.free_frame_textures(context, textures_delta)?;
        let Gles2FrameTimings {
            surface_acquire,
            surface_present,
        } = presented?.timings;
        timings.surface_acquire = surface_acquire;
        timings.surface_present = surface_present;
        Ok(UiFrameOutcome::Presented(PresentedUiFrame {
            timings,
            work: UiFrameWork {
                textures,
                prepare,
                background,
                paint,
                texture_free,
            },
        }))
    }

    /// Применить изменения текстур UI, когда кадр рисовать нельзя (чужой билет буфера, ключ
    /// видеокадра чужого рендера, непостроенный план видео): `set` применяется, `free` — сразу.
    ///
    /// Изменения текстур `UiContent::Fresh` — состояние рендера: приложение второй раз их не
    /// пришлёт, поэтому кадр, отказанный ПОСЛЕ получения содержимого, всё равно обязан их принять
    /// (иначе набор текстур рендера и egui разойдётся, а следующий кадр получит
    /// `UiTexturesOutOfSync`). Удержанный UI не меняется. Правка неизвестной текстуры здесь не
    /// сообщается: вызывающий уже отказывает кадру по другой причине, а полный набор текстур
    /// приложение пришлёт и так.
    ///
    /// # Errors
    /// Ошибки художника и контекста — как у [`Self::render`].
    pub fn settle_content_without_frame(
        &mut self,
        context: &Gles2Context,
        content: UiContent,
    ) -> Result<(), UiFrameError> {
        let UiContent::Fresh { textures_delta, .. } = content else {
            return Ok(());
        };
        match self.painter.apply_textures(context, &textures_delta) {
            Ok(_) => {}
            Err(PainterError::GlAccess(GlAccessError::DetachedWithoutSurfacelessContext)) => {
                // GL недоступен (окно отпущено без surfaceless-контекста): применять нечем,
                // полный набор текстур приложение пришлёт при следующем кадре.
            }
            Err(error) => {
                self.free_textures_after_failure(context, Some(textures_delta));
                return Err(error.into());
            }
        }
        self.painter.free_textures(context, textures_delta)?;
        Ok(())
    }

    /// Разрушить GL-объекты (контекст жив) или забыть их (контекст потерян).
    pub fn destroy(self, context: &Gles2Context) -> Result<(), UiFrameError> {
        self.painter.destroy(context)?;
        Ok(())
    }

    /// Шаги 1–2: применить `set`, проверить содержимое.
    fn accept_content(
        &mut self,
        context: &Gles2Context,
        content: UiContent,
        screen: UiScreen,
        timings: &mut UiFrameTimings,
    ) -> Result<Result<PendingUi, UiFrameDropReason>, UiFrameError> {
        match content {
            UiContent::ReusePrevious => Ok(self
                .painter
                .retained_availability(screen)
                .map(|()| PendingUi::Reused)
                .map_err(UiFrameDropReason::RetainedUiUnavailable)),
            UiContent::Fresh {
                primitives,
                textures_delta,
            } => {
                let started = Instant::now();
                let applied = self.painter.apply_textures(context, &textures_delta);
                timings.egui_texture_update = started.elapsed();
                let textures = match applied {
                    Ok(report) => report,
                    Err(PainterError::GlAccess(
                        GlAccessError::DetachedWithoutSurfacelessContext,
                    )) => {
                        self.painter.free_textures(context, textures_delta)?;
                        return Ok(Err(UiFrameDropReason::UiTexturesNotApplied));
                    }
                    Err(error) => {
                        self.free_textures_after_failure(context, Some(textures_delta));
                        return Err(error.into());
                    }
                };
                if let UiTextureUpload::OutOfSync { texture_id } = textures.upload {
                    // Отклонённый кадр: `free` сразу, удержанный UI остаётся прежним.
                    self.painter.free_textures(context, textures_delta)?;
                    return Ok(Err(UiFrameDropReason::UiTexturesOutOfSync { texture_id }));
                }
                Ok(Ok(PendingUi::Fresh {
                    primitives,
                    textures_delta,
                    textures,
                }))
            }
        }
    }

    /// Шаги 4–7 внутри начатого кадра. При ошибке возвращает `TexturesDelta`, чтобы `free`
    /// всё равно выполнился.
    fn draw_frame(
        &mut self,
        frame: &ActiveFrame<'_>,
        mut plan: DrawPlan<'_>,
        pending: PendingUi,
        timings: &mut UiFrameTimings,
    ) -> Result<DrawnFrame, Box<FailedDraw>> {
        let (textures, prepare, textures_delta) = match pending {
            PendingUi::Fresh {
                primitives,
                textures_delta,
                textures,
            } => {
                let started = Instant::now();
                let prepared = self.painter.prepare_fresh(frame, plan.screen, primitives);
                timings.egui_buffer_update = started.elapsed();
                match prepared {
                    Ok(work) => (Some(textures), Some(work), Some(textures_delta)),
                    Err(error) => {
                        return Err(Box::new(FailedDraw {
                            error: error.into(),
                            textures_delta: Some(textures_delta),
                        }));
                    }
                }
            }
            PendingUi::Reused => (None, None, None),
        };
        let failed = |error: UiFrameError, textures_delta: Option<TexturesDelta>| {
            Box::new(FailedDraw {
                error,
                textures_delta,
            })
        };
        let background = match Self::draw_layers(frame, &mut plan, timings) {
            Ok(background) => background,
            Err(error) => return Err(failed(error, textures_delta)),
        };
        let started = Instant::now();
        let painted = self.painter.paint_retained(frame, plan.screen);
        timings.egui_render = started.elapsed();
        let paint = match painted {
            Ok(paint) => paint,
            Err(error) => return Err(failed(error.into(), textures_delta)),
        };
        Ok(DrawnFrame {
            background,
            textures,
            prepare,
            textures_delta,
            paint,
        })
    }

    /// Шаги 5–6: заливка (если подложка не закрывает цель сама), подложка, оверлеи.
    fn draw_layers(
        frame: &ActiveFrame<'_>,
        plan: &mut DrawPlan<'_>,
        timings: &mut UiFrameTimings,
    ) -> Result<BackgroundFillOutcome, UiFrameError> {
        let started = Instant::now();
        let background = match plan.layers.underlay_coverage() {
            UnderlayCoverage::PaintsWholeTarget => {
                BackgroundFillOutcome::SkippedUnderlayCoversTarget
            }
            UnderlayCoverage::LeavesTargetPartlyUntouched => {
                frame.clear(opaque_background(plan.background));
                BackgroundFillOutcome::Cleared
            }
        };
        plan.layers.draw_underlay(frame)?;
        timings.video_render = started.elapsed();
        let started = Instant::now();
        plan.layers.draw_overlays(frame)?;
        timings.subtitle_overlay = started.elapsed();
        Ok(background)
    }

    /// Кадр не нарисован: `free` сразу (изменения `set` уже применены).
    fn finish_unpainted(
        &mut self,
        context: &Gles2Context,
        pending: PendingUi,
    ) -> Result<(), UiFrameError> {
        if let PendingUi::Fresh { textures_delta, .. } = pending {
            self.painter.free_textures(context, textures_delta)?;
        }
        Ok(())
    }

    fn free_frame_textures(
        &mut self,
        context: &Gles2Context,
        textures_delta: Option<TexturesDelta>,
    ) -> Result<TextureFreeOutcome, UiFrameError> {
        match textures_delta {
            Some(textures_delta) => Ok(self.painter.free_textures(context, textures_delta)?),
            None => Ok(TextureFreeOutcome::Applied { deleted: 0 }),
        }
    }

    /// После ошибки кадра `free` всё равно нужен (иначе debug-паника `TexturesDelta`), а его
    /// собственная ошибка — вторична к основной и только пишется в лог.
    fn free_textures_after_failure(
        &mut self,
        context: &Gles2Context,
        textures_delta: Option<TexturesDelta>,
    ) {
        if let Some(textures_delta) = textures_delta
            && let Err(error) = self.painter.free_textures(context, textures_delta)
        {
            log::warn!("frame compositor: texture free after failed frame: {error}");
        }
    }
}

/// Нарисованный, но ещё не показанный кадр.
struct DrawnFrame {
    background: BackgroundFillOutcome,
    textures: Option<TextureApplyReport>,
    prepare: Option<UiPrepareWork>,
    /// `free` выполняется после показа.
    textures_delta: Option<TexturesDelta>,
    paint: UiPaintWork,
}

/// Отказ внутри кадра: ошибка и `TexturesDelta`, чей `free` всё равно нужно выполнить.
struct FailedDraw {
    error: UiFrameError,
    textures_delta: Option<TexturesDelta>,
}

/// Часть запроса на время рисования.
struct DrawPlan<'layers> {
    screen: UiScreen,
    background: [f32; 4],
    layers: &'layers mut dyn FrameLayers,
}

/// Заливка фона всегда с альфой 1: окно непрозрачное, а у буфера кадра канал альфы может быть
/// (другого EGL-конфига драйвер не дал) — тогда альфа < 1 просветила бы окно у композитора.
fn opaque_background(background: [f32; 4]) -> [f32; 4] {
    [background[0], background[1], background[2], 1.0]
}
