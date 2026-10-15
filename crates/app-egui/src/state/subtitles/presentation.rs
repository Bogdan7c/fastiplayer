//! Решение «что нарисовать на этом кадре»: реплики по pts кадра -> overlay растеризатора.
//!
//! # Инварианты
//! - Растеризатор вызывается ТОЛЬКО когда изменилось то, что на экране: набор видимых реплик,
//!   холст (размер картинки видео) или стиль. На остальных кадрах (пауза, середина реплики,
//!   обычное воспроизведение) возвращается прежний overlay без вызова растеризатора и без
//!   аллокаций: для сравнения используется переиспользуемый буфер идентификаторов реплик.
//! - Overlay хранится здесь (клон дешёвый: маски лежат в `Arc`), поэтому GPU-проход видит
//!   те же `Arc` масок и не загружает текстуру повторно.
//! - Растеризация синхронная на потоке UI прямо перед рендером; длительность измеряется
//!   и сравнивается с бюджетом (`RASTERIZATION_TIME_BUDGET`).
//! - Ошибки отдельных реплик растеризатор гасит сам (пропуск + счётчик); здесь нет
//!   фатальных путей, способных остановить воспроизведение.

use std::time::{Duration, Instant};

use render_core::{PictureLayout, PictureRect, PictureSize, RenderViewport, RenderableFrame};
use subtitle_core::{SubtitleContent, SubtitleEventId, SubtitleLanguage};
use subtitle_raster::{
    CanvasSize, CueKey, CueSourceId, RasterLimits, RenderRequest, RenderableCue,
    SubtitleAppearance, SubtitleFontLibrary, SubtitleOverlay, SubtitleRasterizer,
    TextLanguagePreference,
};
use video_present_core::VideoPresentFrameIdentity;

use super::active_track::ActiveSubtitleTrack;
use super::limits::RASTERIZATION_TIME_BUDGET;

/// Кадр, который реально уходит в renderer, в терминах нужных субтитрам.
///
/// Собирается в точке submit из того, что уже известно там: identity кадра (его нет у
/// stale-lease, тогда субтитры скрыты) и геометрия. Поле `frame_pts` — pts именно
/// отправляемого кадра, а не позиция плеера: на паузе кадр тот же, значит и реплика та же;
/// после seek первый новый кадр сразу даёт новую реплику.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SubmittedFrameForSubtitles {
    frame_pts: Duration,
    render_generation: u64,
    picture_rect: PictureRect,
    /// Зажатая область видео в пикселях окна: в ней рисуются субтитры (чёрные полосы
    /// вокруг картинки - её часть).
    video_viewport: RenderViewport,
}

impl SubmittedFrameForSubtitles {
    /// Собирает описание кадра или `None`, если субтитры на нём показывать нельзя:
    /// - нет identity (stale-lease: во время seek показывается старый кадр — реплику
    ///   скрываем, чтобы «старая» реплика исчезала сразу);
    /// - у картинки или области видео нулевой размер.
    ///
    /// Область видео зажимается к экрану так же, как это делает `Renderer::render_frame`
    /// (`RenderViewport::clamp_to_surface`), поэтому прямоугольник картинки совпадает с тем,
    /// что нарисует видео-проход.
    pub(crate) fn from_submission(
        frame_identity: Option<VideoPresentFrameIdentity>,
        frame: &RenderableFrame,
        video_viewport: RenderViewport,
        screen_size_in_pixels: [u32; 2],
    ) -> Option<Self> {
        let frame_identity = frame_identity?;
        let clamped_viewport =
            video_viewport.clamp_to_surface(screen_size_in_pixels[0], screen_size_in_pixels[1]);
        let layout = PictureLayout::fit(PictureSize::from_frame(frame), clamped_viewport)?;
        Some(Self {
            frame_pts: frame.pts,
            render_generation: frame_identity.render_generation(),
            picture_rect: layout.rect(),
            video_viewport: clamped_viewport,
        })
    }

    /// Прямоугольник картинки видео на экране (без чёрных полос).
    pub(crate) fn picture_rect(&self) -> PictureRect {
        self.picture_rect
    }

    /// Зажатая область видео на экране: слой субтитров может стоять где угодно внутри неё.
    pub(super) fn video_viewport(&self) -> RenderViewport {
        self.video_viewport
    }

    /// Поколение ресурсов рендера, которому принадлежит кадр.
    pub(super) fn render_generation(&self) -> u64 {
        self.render_generation
    }

    /// Собирает описание напрямую (тесты задают pts и геометрию без видео-кадра).
    #[cfg(test)]
    pub(super) fn for_test(
        frame_pts: Duration,
        render_generation: u64,
        picture_rect: PictureRect,
    ) -> Self {
        // Область видео по умолчанию - ровно картинка (без полос).
        let video_viewport = RenderViewport::new(
            picture_rect.x.max(0.0).floor() as u32,
            picture_rect.y.max(0.0).floor() as u32,
            picture_rect.width.ceil() as u32,
            picture_rect.height.ceil() as u32,
        );
        Self::for_test_with_viewport(frame_pts, render_generation, picture_rect, video_viewport)
    }

    /// То же, но с явной областью видео (тесты полос вокруг картинки).
    #[cfg(test)]
    pub(super) fn for_test_with_viewport(
        frame_pts: Duration,
        render_generation: u64,
        picture_rect: PictureRect,
        video_viewport: RenderViewport,
    ) -> Self {
        Self {
            frame_pts,
            render_generation,
            picture_rect,
            video_viewport,
        }
    }
}

/// Счётчики работы презентатора (для диагностики и тестов).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct PresentationStats {
    /// Сколько раз вызывался растеризатор.
    pub(super) rasterizer_calls: u64,
    /// Сколько кадров получили прежний overlay без вызова растеризатора.
    pub(super) reused_frames: u64,
    /// Сколько растеризаций превысили бюджет времени.
    pub(super) over_budget_rasterizations: u64,
    /// Длительность последней растеризации.
    pub(super) last_rasterization: Duration,
    /// Самая долгая растеризация.
    pub(super) slowest_rasterization: Duration,
    /// Сколько реплик пропущено: их тип содержимого ещё не поддерживается.
    pub(super) skipped_unsupported_cues: u64,
    /// Языковое предпочтение текста в последнем запросе к растеризатору.
    pub(super) last_text_language: TextLanguagePreference,
}

/// То, что показано сейчас, и по каким признакам это опознаётся.
struct ShownOverlay {
    source_id: CueSourceId,
    active_events: Vec<SubtitleEventId>,
    canvas: CanvasSize,
    overlay: SubtitleOverlay,
}

impl ShownOverlay {
    /// Тот же ли это кадр субтитров (без аллокаций).
    fn is_same_as(
        &self,
        source_id: CueSourceId,
        active_events: &[SubtitleEventId],
        canvas: CanvasSize,
    ) -> bool {
        self.source_id == source_id
            && self.canvas == canvas
            && self.active_events.as_slice() == active_events
    }
}

/// Были ли на кадре активные реплики у проверяемой дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveCues {
    /// Реплик нет (или кадр нельзя разместить).
    None,
    /// Реплики есть; показать их удалось или нет - решает растеризатор.
    Present,
}

/// Владелец растеризатора и последнего показанного overlay.
pub(super) struct SubtitlePresenter {
    /// Появляется, когда фоновая задача принесла библиотеку шрифтов.
    rasterizer: Option<SubtitleRasterizer>,
    appearance: SubtitleAppearance,
    /// Лимиты, с которыми создаётся растеризатор (меняются только до его создания).
    raster_limits: RasterLimits,
    /// Язык системы: запасной язык текста, когда дорожка своего не объявляет. Читается один
    /// раз при сборке приложения и дальше неизменен.
    system_language: Option<SubtitleLanguage>,
    /// Переиспользуемый буфер: реплики, активные на текущем кадре.
    scratch_active_events: Vec<SubtitleEventId>,
    shown: Option<ShownOverlay>,
    stats: PresentationStats,
}

impl SubtitlePresenter {
    /// Создаёт презентатор со стилем и лимитами из настроек пользователя.
    pub(super) fn new(
        system_language: Option<SubtitleLanguage>,
        appearance: SubtitleAppearance,
        raster_limits: RasterLimits,
    ) -> Self {
        Self {
            rasterizer: None,
            appearance,
            raster_limits,
            system_language,
            scratch_active_events: Vec::new(),
            shown: None,
            stats: PresentationStats::default(),
        }
    }

    /// Есть ли растеризатор (то есть нужна ли новая библиотека шрифтов следующей задаче).
    pub(super) fn has_rasterizer(&self) -> bool {
        self.rasterizer.is_some()
    }

    /// Создаёт растеризатор поверх библиотеки, подготовленной в фоне.
    pub(super) fn install_font_library(&mut self, fonts: SubtitleFontLibrary) {
        self.rasterizer = Some(SubtitleRasterizer::new(fonts, self.raster_limits));
        self.shown = None;
    }

    /// Забывает показанный overlay (смена media или дорожки): следующий кадр с репликами
    /// растеризуется заново, а прежний не может «просочиться».
    pub(super) fn forget_shown_overlay(&mut self) {
        self.shown = None;
    }

    /// Меняет стиль; следующий кадр растеризуется заново (кэш растеризатора ключуется стилем,
    /// поэтому старые картинки не переиспользуются), последующие кадры берут новый overlay.
    ///
    /// Тот же стиль - no-op: показанный overlay остаётся, перерастеризации нет. Это позволяет
    /// владельцу вызывать метод при любом событии (сохранение правки, подтверждение конфига)
    /// и не бояться лишней работы на кадре.
    pub(super) fn replace_appearance(&mut self, appearance: SubtitleAppearance) {
        if self.appearance == appearance {
            return;
        }
        self.appearance = appearance;
        self.shown = None;
    }

    /// Запоминает лимиты для СЛЕДУЮЩЕГО растеризатора. Действующий не трогает: его лимиты
    /// заданы при создании.
    pub(super) fn replace_raster_limits(&mut self, raster_limits: RasterLimits) {
        self.raster_limits = raster_limits;
    }

    /// Освобождает растеризатор (вместе с библиотекой шрифтов и кэшами). Следующая загрузка
    /// построит новый с актуальными лимитами. Вызывает владелец только на смене media.
    pub(super) fn discard_rasterizer(&mut self) {
        self.rasterizer = None;
        self.shown = None;
    }

    /// Счётчики работы.
    #[cfg(test)]
    pub(super) fn stats(&self) -> PresentationStats {
        self.stats
    }

    /// Счётчики растеризатора (если он создан).
    #[cfg(test)]
    pub(super) fn rasterizer_statistics(&self) -> Option<subtitle_raster::RasterStatistics> {
        self.rasterizer.as_ref().map(SubtitleRasterizer::statistics)
    }

    /// Главный вход: overlay для кадра или `None`, если рисовать нечего.
    ///
    /// `None` означает «нет активных реплик / нет растеризатора / нельзя собрать холст».
    pub(super) fn overlay_for_frame(
        &mut self,
        track: &ActiveSubtitleTrack,
        frame: &SubmittedFrameForSubtitles,
    ) -> Option<&SubtitleOverlay> {
        self.prepare_overlay(track, frame);
        self.shown_overlay()
    }

    /// Overlay кадра в режиме правки: настоящая реплика, а если на кадре её нет - образец.
    ///
    /// Приоритет у настоящей реплики (решение владельца 2): образец подставляется только когда
    /// `primary` отсутствует либо на этом кадре у неё нет активных реплик.
    pub(super) fn overlay_for_frame_or_sample(
        &mut self,
        primary: Option<&ActiveSubtitleTrack>,
        sample: &ActiveSubtitleTrack,
        frame: &SubmittedFrameForSubtitles,
    ) -> Option<&SubtitleOverlay> {
        let primary_has_cues =
            primary.is_some_and(|track| self.prepare_overlay(track, frame) == ActiveCues::Present);
        if !primary_has_cues {
            self.prepare_overlay(sample, frame);
        }
        self.shown_overlay()
    }

    /// Готовит `shown` для кадра по репликам дорожки и сообщает, были ли реплики на кадре.
    ///
    /// Растеризатор вызывается только когда изменился набор реплик, холст или стиль.
    fn prepare_overlay(
        &mut self,
        track: &ActiveSubtitleTrack,
        frame: &SubmittedFrameForSubtitles,
    ) -> ActiveCues {
        let Some(canvas) = canvas_for_picture(frame.picture_rect()) else {
            return ActiveCues::None;
        };
        let lookup_time = track.lookup_time_for(frame.frame_pts);

        self.scratch_active_events.clear();
        track
            .timeline()
            .collect_active_at(lookup_time, &mut self.scratch_active_events);
        if self.scratch_active_events.is_empty() {
            // На кадре нет реплик: показанного больше нет, растеризатор не нужен.
            self.shown = None;
            return ActiveCues::None;
        }

        let reuse_shown = self.shown.as_ref().is_some_and(|shown| {
            shown.is_same_as(track.source_id(), &self.scratch_active_events, canvas)
        });
        if reuse_shown {
            self.stats.reused_frames += 1;
        } else if self.rasterize_active_events(track, canvas).is_none() {
            // Растеризатора ещё нет: реплики есть, показать пока нечего.
            self.shown = None;
        }
        ActiveCues::Present
    }

    /// Показанный overlay, если он не пуст.
    fn shown_overlay(&self) -> Option<&SubtitleOverlay> {
        self.shown
            .as_ref()
            .map(|shown| &shown.overlay)
            .filter(|overlay| !overlay.is_empty())
    }

    /// Растеризует реплики из `scratch_active_events` и запоминает результат.
    ///
    /// Возвращает `None`, если растеризатора нет (шрифты ещё не доехали).
    fn rasterize_active_events(
        &mut self,
        track: &ActiveSubtitleTrack,
        canvas: CanvasSize,
    ) -> Option<()> {
        let rasterizer = self.rasterizer.as_mut()?;

        // Аллокация здесь допустима: мы на пути «набор реплик изменился», а не на обычном кадре.
        let mut cues: Vec<RenderableCue<'_>> = Vec::with_capacity(self.scratch_active_events.len());
        for &event_id in &self.scratch_active_events {
            let Some(event) = track.timeline().event(event_id) else {
                continue;
            };
            match event.content() {
                SubtitleContent::Text(cue) => cues.push(RenderableCue {
                    key: CueKey::new(track.source_id(), event_id),
                    cue,
                }),
                // `SubtitleContent` расширяемый (ASS, bitmap): неподдерживаемое считаем.
                _ => self.stats.skipped_unsupported_cues += 1,
            }
        }

        // Язык дорожки, иначе системы. Считается только на пути «реплики изменились»; смена
        // дорожки меняет `source_id`, поэтому повторное использование показанного кадра
        // (`ShownOverlay::is_same_as`) не может пережить смену языка.
        let text_language = TextLanguagePreference::track_then_system(
            track.language(),
            self.system_language.as_ref(),
        );
        self.stats.last_text_language = text_language;
        let started_at = Instant::now();
        let overlay = rasterizer
            .render(&RenderRequest {
                cues: &cues,
                canvas,
                appearance: &self.appearance,
                text_language,
            })
            .clone();
        let elapsed = started_at.elapsed();
        self.record_rasterization(elapsed, cues.len(), canvas);

        let shown = ShownOverlay {
            source_id: track.source_id(),
            active_events: self.scratch_active_events.clone(),
            canvas,
            overlay,
        };
        self.shown = Some(shown);
        Some(())
    }

    /// Учитывает длительность растеризации и предупреждает о превышении бюджета.
    fn record_rasterization(&mut self, elapsed: Duration, cue_count: usize, canvas: CanvasSize) {
        self.stats.rasterizer_calls += 1;
        self.stats.last_rasterization = elapsed;
        self.stats.slowest_rasterization = self.stats.slowest_rasterization.max(elapsed);
        let over_budget = elapsed > RASTERIZATION_TIME_BUDGET;
        if over_budget {
            self.stats.over_budget_rasterizations += 1;
            // Без текста реплик: только размеры и время.
            tracing::warn!(
                elapsed_us = elapsed.as_micros(),
                budget_us = RASTERIZATION_TIME_BUDGET.as_micros(),
                cues = cue_count,
                canvas_width = canvas.width(),
                canvas_height = canvas.height(),
                "Subtitle rasterization exceeded its UI-thread time budget"
            );
        } else {
            tracing::debug!(
                elapsed_us = elapsed.as_micros(),
                cues = cue_count,
                canvas_width = canvas.width(),
                canvas_height = canvas.height(),
                "Subtitle cues rasterized"
            );
        }
    }
}

/// Холст = размер картинки видео, округлённый до целых пикселей (контракт GPU-прохода:
/// 1 пиксель холста = 1 пиксель экрана). `None` для вырожденной картинки.
fn canvas_for_picture(picture_rect: PictureRect) -> Option<CanvasSize> {
    let width = picture_rect.width.round();
    let height = picture_rect.height.round();
    // Сравнение в форме `>=` сознательно: NaN не проходит его и даёт `None`.
    if !(width >= 1.0 && height >= 1.0) {
        return None;
    }
    // `as` насыщает значения вне диапазона; сверхбольшой холст отсечёт растеризатор лимитом.
    CanvasSize::new(width as u32, height as u32)
}
