//! Публичный растеризатор субтитров: реплики + стиль + холст -> `SubtitleOverlay`.
//!
//! ## Архитектура
//! - [`SubtitleRasterizer`] владеет библиотекой шрифтов, кэшем лиц, кэшем готовых реплик и
//!   последним выданным кадром. Снаружи видны только намерения: `render`, `statistics`,
//!   `register_attached_font`, `load_system_fonts`.
//! - Кадр собирается так: для каждой реплики берётся готовая из кэша (ключ: `CueKey`, холст и
//!   стиль) или рисуется (`cue_render`); затем `collisions` раздвигает налезающие
//!   реплики (меняются только позиции, маски те же — это даёт `OverlayChange::Moved`);
//!   затем результат сравнивается с прошлым кадром (`detect_overlay_change`).
//! - Реплики бывают двух видов ([`CueContent`]): простых форматов (SRT, WebVTT: порядок по запросу,
//!   столкновения по картинкам) и ASS (порядок `(слой, ReadOrder)`, столкновения по слоям с памятью
//!   положений между кадрами — `ass_collisions`, рисование — `ass_cue` поверх `crate::ass`). Сначала
//!   идут картинки простых реплик, затем ASS.
//! - Клипы ASS (`\clip`, `\iclip`) в ключ кэша НЕ входят: кэш хранит картинки реплики без клипа
//!   (`RasterizedCue::bitmaps`), а клип накладывает стадия `crate::clip` при сборке кадра, до сдвига
//!   столкновений (`RasterizedCue::final_bitmaps`; как в libass).
//! - Анимированный прямоугольный клип (`\t(\clip(...))`) считается по часам кадра в той же стадии
//!   и в ключ кэша не входит; `\t`, меняющий вид букв, кладёт в ключ свой вес (реплика рисуется
//!   заново при каждом новом весе; общая работа соседних кадров лежит в кэшах глифов
//!   `crate::glyph_cache`: контуры, маски букв, склейки участков; их лимиты применяются в начале
//!   каждого `render`). Реплика с мгновенным состоянием анимации кладётся в кэш реплик «на
//!   испытание» (`ByteLruCache::insert_on_probation`): поток таких записей не вытесняет статичные.
//! - Караоке (`\k`, `\kf`, `\ko`) тоже стадия после кэша: буквы строки рисуются один раз, а
//!   `RasterizedCue::final_bitmaps` на каждом кадре раскрашивает их по часам события
//!   (`crate::karaoke`); срок годности кадра сообщает `RasterizedCue::karaoke_change`.
//! - Время: `RenderRequest::moment` + тайминг ASS-события (`AssEventTiming`) задают анимации
//!   (`\move`, `\fad`, `\t`). Реплика без анимации кэшируется по ключу без времени; анимированная —
//!   по её мгновенному состоянию. `SubtitleOverlay::animation_change` сообщает, когда
//!   картинка изменится сама.
//! - Если запрос совпал с прошлым (те же ключи в том же порядке, холст, стиль, языковое предпочтение,
//!   настройки ASS — если в кадре есть ASS), работа не делается вообще: возвращается прошлый
//!   результат с `Unchanged` (быстрый путь); для кадра с анимацией — только пока `AnimationChange::holds_at`
//!   подтверждает прежний кадр на новый момент.
//!
//! ## Политика ошибок
//! Ошибка субтитров не должна ронять воспроизведение: реплика, которую не удалось нарисовать,
//! пропускается (`tracing::warn` без текста реплики, счётчик `failed_cues`), остальные
//! рисуются. Фатальных ошибок у `render` нет, поэтому он возвращает `&SubtitleOverlay`, а не
//! `Result`: вызывающему нечего обрабатывать иначе, чем «показать что есть».

mod ass_collisions;
mod ass_cue;
mod ass_frame;
mod collisions;
mod cue_render;
mod frame;
mod limits;
mod picture_cue;
mod request;
mod reveal;
mod statistics;

use std::borrow::Cow;
use std::sync::Arc;

use crate::appearance::SubtitleAppearance;
use crate::ass::{
    AssAnimation, AssAnimationKey, AssAnimationState, AssEventClock, AssFrameScale,
    AssRenderOptions, AssStyleSource, evaluate_animation,
};
use crate::blur::BlurScratch;
use crate::cache::ByteLruCache;
use crate::face_cache::FaceCache;
use crate::fonts::{
    AttachedFontError, AttachedFontLimits, AttachedFontRegistration, AttachedFontStatus,
    AttachedFontsClearOutcome, AttachedFontsInstallReport, PreparedAttachedFont,
    PreparedAttachedFonts, SubtitleFontLibrary, SystemFontScanReport, SystemFontScanStatus,
    TextLanguagePreference,
};
use crate::glyph_cache::{GlyphCacheLimits, GlyphCaches};
use crate::overlay::{
    CanvasSize, OverlayChange, OverlayRevision, SubtitleBitmap, SubtitleOverlay,
    detect_overlay_change,
};
use ass_collisions::AssCollisionMemory;
use ass_cue::{AssCueInputs, rasterize_ass_cue};
use ass_frame::{AssFrameCue, assemble_ass_bitmaps};
use collisions::{CollisionItem, resolve_vertical_shifts};
use cue_render::{CuePainter, RasterizedCue, rasterize_cue};
use frame::{FrameSignature, FrameState, FrameTimeDependence};
use limits::{admit_cues, ass_cue_char_count, cue_char_count};

pub use limits::{RasterInputLimits, RasterLimits};
pub use request::{
    AssEventTiming, CueContent, CueKey, CueSourceId, RasterCacheLimits, RenderRequest,
    RenderableCue,
};
use statistics::Counters;
pub use statistics::RasterStatistics;

/// От чего зависит вид готовой реплики, кроме холста и языка.
///
/// Реплики простых форматов зависят от пользовательского стиля; реплики ASS — от настроек ASS,
/// а от пользовательского стиля только в режиме «мой стиль». Поэтому смена настроек ASS не
/// сбрасывает кэш простых реплик, а смена стиля — кэш ASS-реплик с авторским оформлением.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum CueLook {
    /// Реплика простого формата. `hidden_spans` — индексы спанов, ещё не появившихся на момент
    /// кадра (`TextSpan::reveal_at`, см. `reveal`): разный набор скрытых спанов — разная
    /// картинка. Пустой набор (всё видно) не выделяет память и общий с репликой без `reveal_at`.
    Text {
        appearance: SubtitleAppearance,
        hidden_spans: Box<[u32]>,
    },
    /// Растровая реплика (PGS): от стиля, языка и настроек ASS не зависит.
    Picture,
    /// Реплика ASS.
    Ass {
        options: AssRenderOptions,
        /// Пользовательский стиль, только если он применяется (`AssStyleSource::User`).
        user_appearance: Option<SubtitleAppearance>,
        /// Время-зависимое состояние (положение `\move`, затухание); у реплики без
        /// анимации — ключ без времени, поэтому статичная реплика рисуется один раз.
        animation: AssAnimationKey,
    },
}

impl CueLook {
    /// Ключ содержит мгновенное состояние анимации (`AssAnimationKey::Sampled`): на соседнем
    /// кадре он уже другой.
    fn is_time_sampled(&self) -> bool {
        matches!(
            self,
            Self::Ass {
                animation: AssAnimationKey::Sampled { .. },
                ..
            }
        )
    }
}

/// Ключ кэша готовых реплик.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CueCacheKey {
    cue: CueKey,
    canvas: CanvasSize,
    look: CueLook,
    /// Язык меняет выбор CJK-лица, а значит и маски: другой язык — другая запись.
    text_language: TextLanguagePreference,
}

/// Растеризатор субтитров.
pub struct SubtitleRasterizer {
    fonts: SubtitleFontLibrary,
    faces: FaceCache,
    cue_cache: ByteLruCache<CueCacheKey, RasterizedCue>,
    /// Кэши контуров, масок глифов и склеек участков (libass-подобный нижний уровень под
    /// кэшем реплик, см. `crate::glyph_cache`).
    glyph_caches: GlyphCaches,
    limits: RasterLimits,
    /// Рисовальщик реплик (в продакшене всегда `rasterize_cue`, см. [`CuePainter`]).
    painter: CuePainter,
    /// Запомненные положения ASS-реплик между кадрами (порт `priv->top/height` libass).
    ass_collisions: AssCollisionMemory,
    frame: Option<FrameState>,
    counters: Counters,
    /// Рабочая память размытия `\blur` / `\be`: одна на растеризатор (то есть на поток
    /// отрисовки), переиспользуется всеми репликами.
    blur_scratch: BlurScratch,
}

impl SubtitleRasterizer {
    /// Создаёт растеризатор поверх библиотеки шрифтов (встроенные уже загружены;
    /// системные и вложенные подключаются методами растеризатора).
    pub fn new(fonts: SubtitleFontLibrary, limits: RasterLimits) -> Self {
        Self {
            fonts,
            faces: FaceCache::new(limits.cache.shaping_cache_bytes),
            cue_cache: ByteLruCache::new(limits.cache.cue_bitmaps_bytes),
            glyph_caches: GlyphCaches::new(GlyphCacheLimits {
                outline_items: limits.cache.glyph_outline_items,
                bitmap_bytes: limits.cache.glyph_bitmaps_bytes,
                composite_bytes: limits.cache.glyph_composites_bytes,
            }),
            limits,
            painter: rasterize_cue,
            ass_collisions: AssCollisionMemory::default(),
            frame: None,
            counters: Counters::default(),
            blur_scratch: BlurScratch::new(),
        }
    }

    /// Главный вход на кадр: рисует (или берёт из кэша) реплики запроса.
    ///
    /// Если набор реплик (по [`CueKey`]), холст и стиль те же, что в прошлый раз, никакой
    /// работы не делается: возвращается прошлый результат с [`OverlayChange::Unchanged`] и
    /// той же ревизией. Иначе ревизия растёт, а `change` описывает отличие от прошлого кадра.
    pub fn render(&mut self, request: &RenderRequest<'_>) -> &SubtitleOverlay {
        let reusable = self
            .frame
            .as_ref()
            .is_some_and(|frame| frame.can_serve(request));
        if reusable {
            self.counters.unchanged_frames += 1;
            if let Some(frame) = self.frame.as_mut() {
                frame.overlay.mark_unchanged();
            }
        } else {
            self.counters.rendered_frames += 1;
            let next = self.build_frame(request);
            self.frame = Some(next);
        }
        // `get_or_insert_with` не срабатывает (кадр уже есть), но избавляет от `unwrap`.
        let frame = self.frame.get_or_insert_with(|| FrameState {
            signature: FrameSignature::of(request),
            overlay: SubtitleOverlay::empty(
                request.canvas,
                OverlayRevision::INITIAL,
                OverlayChange::Replaced,
            ),
            stale: true,
            rendered_at: request.moment,
            time: FrameTimeDependence::FREE,
        });
        &frame.overlay
    }

    /// Счётчики кэшей и работы растеризатора.
    pub fn statistics(&self) -> RasterStatistics {
        let fonts = self.fonts.statistics();
        let glyph_caches = self.glyph_caches.statistics();
        RasterStatistics {
            cue_cache: self.cue_cache.statistics(),
            outline_cache: glyph_caches.outlines,
            glyph_bitmap_cache: glyph_caches.bitmaps,
            composite_cache: glyph_caches.composites,
            shaping_cache: self.faces.shaped_runs().statistics(),
            face_cache: self.faces.statistics(),
            rasterized_cues: self.counters.rasterized_cues,
            failed_cues: self.counters.failed_cues,
            rendered_frames: self.counters.rendered_frames,
            unchanged_frames: self.counters.unchanged_frames,
            missing_glyph_lookups: fonts.missing_glyph_lookups,
            rejected_cues_too_long: self.counters.rejected_cues_too_long,
            dropped_cues_over_frame_limit: self.counters.dropped_cues_over_frame_limit,
            dropped_ass_events_over_frame_limit: self.counters.dropped_ass_events_over_frame_limit,
            rejected_canvas: self.counters.rejected_canvas,
            skipped_glyphs: self.counters.skipped_glyphs,
            skipped_borders: self.counters.skipped_borders,
            invalid_ass_drawings: self.counters.invalid_ass_drawings,
            skipped_oversized_bitmaps: self.counters.skipped_oversized_bitmaps,
            failed_blur_layers: self.counters.failed_blur_layers,
            rejected_pictures_too_large: self.counters.rejected_pictures_too_large,
            failed_pictures: self.counters.failed_pictures,
            damaged_pictures: self.counters.damaged_pictures,
            dropped_pictures_over_frame_limit: self.counters.dropped_pictures_over_frame_limit,
            unusable_faces: fonts.unusable_faces,
            unreadable_faces: fonts.unreadable_faces,
            current_frame_bytes: self.frame.as_ref().map_or(0, FrameState::mask_bytes),
        }
    }

    /// Библиотека шрифтов только для чтения (описания лиц, статистика поиска).
    pub fn fonts(&self) -> &SubtitleFontLibrary {
        &self.fonts
    }

    /// Регистрирует вложенный шрифт (например, из MKV). При новой регистрации готовые реплики и
    /// прошлый кадр сбрасываются: выбор шрифта для тех же реплик мог измениться. Повтор уже
    /// зарегистрированных байт (`AlreadyRegistered`) и ошибка ничего не меняют.
    pub fn register_attached_font(
        &mut self,
        font_bytes: Arc<[u8]>,
        limits: &AttachedFontLimits,
    ) -> Result<AttachedFontRegistration, AttachedFontError> {
        let registration = self.fonts.register_attached_font(font_bytes, limits)?;
        // Повтор уже известного шрифта ничего не меняет: кэши остаются валидными.
        if registration.status == AttachedFontStatus::Registered {
            self.invalidate_font_dependent_state();
        }
        Ok(registration)
    }

    /// Дешёвая фаза вложенного шрифта, подготовленного вне UI-потока
    /// ([`PreparedAttachedFont::prepare`]). Кэши сбрасываются так же, как в
    /// [`Self::register_attached_font`]: только при новой регистрации.
    pub fn install_prepared_attached_font(
        &mut self,
        prepared: PreparedAttachedFont,
        limits: &AttachedFontLimits,
    ) -> Result<AttachedFontRegistration, AttachedFontError> {
        let registration = self
            .fonts
            .install_prepared_attached_font(prepared, limits)?;
        if registration.status == AttachedFontStatus::Registered {
            self.invalidate_font_dependent_state();
        }
        Ok(registration)
    }

    /// Дешёвая фаза для пакета вложений media ([`PreparedAttachedFonts::prepare`]).
    /// Кэши сбрасываются один раз и только если хоть один файл реально добавлен.
    pub fn install_prepared_attached_fonts(
        &mut self,
        prepared: PreparedAttachedFonts,
        limits: &AttachedFontLimits,
    ) -> AttachedFontsInstallReport {
        let report = self.fonts.install_prepared_attached_fonts(prepared, limits);
        if report.registered > 0 {
            self.invalidate_font_dependent_state();
        }
        report
    }

    /// Снимает все вложенные шрифты (смена media): готовые реплики и прошлый кадр сбрасываются,
    /// только если что-то было снято. Встроенные и системные шрифты не затрагиваются.
    pub fn clear_attached_fonts(&mut self) -> AttachedFontsClearOutcome {
        let outcome = self.fonts.clear_attached_fonts();
        if matches!(outcome, AttachedFontsClearOutcome::Cleared { .. }) {
            self.invalidate_font_dependent_state();
        }
        outcome
    }

    /// Явный IO-шаг: сканирует системные шрифты. Если появились новые лица, готовые реплики
    /// и прошлый кадр сбрасываются.
    pub fn load_system_fonts(&mut self) -> SystemFontScanReport {
        let report = self.fonts.load_system_fonts();
        if report.status == SystemFontScanStatus::Scanned {
            self.invalidate_font_dependent_state();
        }
        report
    }

    /// Сбрасывает всё, что зависит от набора шрифтов.
    fn invalidate_font_dependent_state(&mut self) {
        self.cue_cache.clear();
        self.glyph_caches.clear();
        self.faces.clear();
        self.ass_collisions.clear();
        if let Some(frame) = self.frame.as_mut() {
            frame.stale = true;
        }
    }

    /// Собирает новый кадр по запросу.
    fn build_frame(&mut self, request: &RenderRequest<'_>) -> FrameState {
        // Как `check_cache_limits` в начале кадра libass: лимиты кэшей глифов применяются здесь,
        // а не при вставке, чтобы рабочий набор кадра не вытеснял сам себя.
        self.glyph_caches.cut_to_limits();
        let signature = FrameSignature::of(request);
        // Холст сверх лимита: рисовать нечего, результат пустой (Replaced, если до этого
        // что-то было показано; ревизию и признак считает общая сборка).
        if request.canvas.width() > self.limits.input.max_canvas_side
            || request.canvas.height() > self.limits.input.max_canvas_side
        {
            self.counters.rejected_canvas += 1;
            tracing::warn!(
                width = request.canvas.width(),
                height = request.canvas.height(),
                max_side = self.limits.input.max_canvas_side,
                "subtitle canvas is larger than the limit, nothing is drawn"
            );
            let overlay = self.assemble_overlay(request.canvas, Vec::new());
            return FrameState {
                signature,
                overlay,
                stale: false,
                rendered_at: request.moment,
                time: FrameTimeDependence::FREE,
            };
        }
        // Лица вытесняем между кадрами: внутри реплики на них держатся ссылки.
        self.faces.trim_to(self.limits.cache.shaper_faces);

        // Лимиты числа реплик: простые форматы — первые по запросу; ASS — по (слой, ReadOrder).
        let admitted = admit_cues(request.cues, &self.limits.input);
        if admitted.dropped_text > 0 {
            self.counters.dropped_cues_over_frame_limit += admitted.dropped_text as u64;
            tracing::warn!(
                dropped = admitted.dropped_text,
                max_cues = self.limits.input.max_cues_per_frame,
                "too many subtitle cues in one frame, the extra ones are dropped"
            );
        }
        if admitted.dropped_ass > 0 {
            self.counters.dropped_ass_events_over_frame_limit += admitted.dropped_ass as u64;
            tracing::warn!(
                dropped = admitted.dropped_ass,
                max_events = self.limits.input.max_ass_events_per_frame,
                "too many ASS events in one frame, the ones on the highest layers are dropped"
            );
        }
        if admitted.dropped_pictures > 0 {
            self.counters.dropped_pictures_over_frame_limit += admitted.dropped_pictures as u64;
            tracing::warn!(
                dropped = admitted.dropped_pictures,
                max_cues = self.limits.input.max_cues_per_frame,
                "too many picture subtitle cues in one frame, the extra ones are dropped"
            );
        }
        // Реплики простых форматов и ASS живут по разным правилам порядка и столкновений.
        let mut text_cues: Vec<Arc<RasterizedCue>> = Vec::new();
        let mut ass_cues: Vec<AssFrameCue> = Vec::new();
        let mut time = FrameTimeDependence::FREE;
        for cue in admitted.text.into_iter().chain(admitted.ass) {
            // Анимация нужна до кэша: от неё зависит ключ реплики.
            let animation = Self::animation_of(cue, request);
            if let Some(animation) = &animation {
                time.include(animation);
            }
            // Слова с `reveal_at` делают кадр функцией времени: картинка верна до появления
            // следующего слова, а при перемотке назад её надо пересчитать.
            if let CueContent::Text(text) = cue.content
                && reveal::has_reveal_times(text)
            {
                time.include_reveal(reveal::next_reveal_after(text, request.moment));
            }
            let Some(rasterized) = self.cue_for(cue, request, animation.as_ref()) else {
                continue;
            };
            match cue.content {
                CueContent::Text(_) => text_cues.push(rasterized),
                // Растровые реплики сюда не попадают (`admit_cues` их отделяет и они
                // рисуются ниже), поэтому ветка пуста.
                CueContent::Picture(_) => {}
                CueContent::Ass { cue: ass, .. } => {
                    let clock = animation.as_ref().and_then(|current| current.state.clock);
                    // Караоке зависит от времени уже после кэша: срок годности картинки
                    // узнаётся по готовой реплике.
                    if let Some(change) = clock
                        .as_ref()
                        .and_then(|clock| rasterized.karaoke_change(clock))
                    {
                        time.include_stage_change(change);
                    }
                    ass_cues.push(AssFrameCue {
                        key: cue.key,
                        layer: cue.layer,
                        read_order: ass.read_order,
                        rasterized,
                        clock,
                    });
                }
            }
        }
        let mut bitmaps = Self::text_bitmaps(&text_cues, request.canvas);
        // Для кадра без ASS память положений пуста: `resolve` на пустом списке её очищает.
        bitmaps.extend(assemble_ass_bitmaps(
            ass_cues,
            &mut self.ass_collisions,
            request.canvas,
            &request.ass,
        ));
        // Растровые реплики идут поверх текста и ASS, в порядке запроса.
        bitmaps.extend(self.picture_bitmaps(&admitted.pictures, request.canvas));

        // Рабочая память переиспользуется между репликами, но не растёт навсегда после
        // одного очень большого размытия.
        self.blur_scratch
            .release_if_larger_than(self.limits.cache.blur_scratch_retained_bytes);
        let overlay = self
            .assemble_overlay(request.canvas, bitmaps)
            .with_animation_change(time.change);
        FrameState {
            signature,
            overlay,
            stale: false,
            rendered_at: request.moment,
            time,
        }
    }

    /// Анимация ASS-реплики на момент запроса; `None` для реплик простых форматов.
    fn animation_of(cue: &RenderableCue<'_>, request: &RenderRequest<'_>) -> Option<AssAnimation> {
        match cue.content {
            CueContent::Text(_) | CueContent::Picture(_) => None,
            CueContent::Ass { cue: ass, timing } => {
                let frame =
                    AssFrameScale::new(ass.script.info(), request.canvas, request.ass.storage_size);
                Some(evaluate_animation(
                    ass,
                    &AssEventClock::new(request.moment, timing),
                    &frame,
                ))
            }
        }
    }

    /// Картинки реплик простых форматов: порядок запроса, столкновения по картинкам.
    fn text_bitmaps(cues: &[Arc<RasterizedCue>], canvas: CanvasSize) -> Vec<SubtitleBitmap> {
        let items: Vec<CollisionItem> = cues
            .iter()
            .map(|cue| CollisionItem {
                extent: cue.extent,
                direction: cue.collision,
            })
            .collect();
        let shifts = resolve_vertical_shifts(&items, canvas.height());
        let mut bitmaps: Vec<SubtitleBitmap> = Vec::new();
        for (cue, shift) in cues.iter().zip(shifts) {
            bitmaps.extend(cue.final_bitmaps(None).iter().map(|bitmap| {
                if shift == 0 {
                    bitmap.clone()
                } else {
                    bitmap.shifted_vertically(shift)
                }
            }));
        }
        bitmaps
    }

    /// Готовая реплика из кэша или свежая; `None` — реплику не удалось нарисовать или она
    /// отклонена лимитом длины.
    fn cue_for(
        &mut self,
        cue: &RenderableCue<'_>,
        request: &RenderRequest<'_>,
        animation: Option<&AssAnimation>,
    ) -> Option<Arc<RasterizedCue>> {
        let key = cue.key;
        let cache_key = CueCacheKey {
            cue: key,
            canvas: request.canvas,
            look: Self::cue_look(cue.content, request, animation),
            text_language: request.text_language,
        };
        // Попадание в кэш означает, что реплика уже прошла проверку длины: повторно не считаем.
        if let Some(hit) = self.cue_cache.get(&cache_key) {
            return Some(hit);
        }
        let chars = match cue.content {
            CueContent::Text(text) => cue_char_count(&text.text),
            CueContent::Ass { cue: ass, .. } => ass_cue_char_count(&ass.text),
            // Растровые реплики идут через `picture_for`, а не через `cue_for`.
            CueContent::Picture(_) => return None,
        };
        if chars > self.limits.input.max_chars_per_cue {
            self.counters.rejected_cues_too_long += 1;
            // Текст реплики в журнал не пишем: он может быть приватным.
            tracing::warn!(
                chars,
                max_chars = self.limits.input.max_chars_per_cue,
                source = key.source().get(),
                event = key.event().get(),
                "subtitle cue skipped: text is longer than the limit"
            );
            return None;
        }
        let painted = match cue.content {
            CueContent::Text(text) => {
                // Не появившиеся слова не рисуем и места под них не оставляем.
                let visible = match &cache_key.look {
                    CueLook::Text { hidden_spans, .. } => {
                        reveal::cue_without_spans(text, hidden_spans)
                    }
                    _ => Cow::Borrowed(text),
                };
                (self.painter)(
                    &mut self.fonts,
                    &mut self.faces,
                    &visible,
                    request.appearance,
                    request.canvas,
                    request.text_language,
                )
            }
            CueContent::Picture(_) => return None,
            CueContent::Ass { cue: ass, .. } => rasterize_ass_cue(
                &mut self.fonts,
                &mut self.faces,
                ass,
                &AssCueInputs {
                    appearance: request.appearance,
                    options: &request.ass,
                    animation: &animation.map_or(AssAnimationState::STILL, |current| current.state),
                    canvas: request.canvas,
                    language: request.text_language,
                    max_bitmap_side: self.limits.input.max_canvas_side,
                },
                &mut self.blur_scratch,
                &mut self.glyph_caches,
            ),
        };
        match painted {
            Ok(mut rasterized) => {
                // Кэш ключуется `CueKey`, поэтому владелец картинок реплики постоянен и
                // проставляется один раз; попадания в кэш уже несут его.
                rasterized.bitmaps = std::mem::take(&mut rasterized.bitmaps)
                    .into_iter()
                    .map(|bitmap| bitmap.with_owner(key))
                    .collect();
                self.counters.rasterized_cues += 1;
                self.counters.skipped_glyphs += u64::from(rasterized.diagnostics.skipped_glyphs);
                self.counters.skipped_borders += u64::from(rasterized.diagnostics.skipped_borders);
                self.counters.skipped_oversized_bitmaps +=
                    u64::from(rasterized.diagnostics.skipped_oversized_bitmaps);
                self.counters.failed_blur_layers +=
                    u64::from(rasterized.diagnostics.failed_blur_layers);
                self.counters.invalid_ass_drawings +=
                    u64::from(rasterized.diagnostics.invalid_ass_drawings);
                let bytes = rasterized.cache_bytes();
                let shared = Arc::new(rasterized);
                // Реплика с мгновенным состоянием анимации (`\move`, `\fad`, `\t`, прокрутка) на
                // соседнем кадре получает другой ключ, и запись стала бы «мёртвой», вытесняя
                // полезные статичные реплики. Поэтому она ложится в кэш «на испытание»: её
                // вытеснят раньше любой записи, которой уже пользовались. Состояние, к которому
                // вернулись или которое застыло после конца движения, при первом попадании
                // становится обычной записью. Общая работа между кадрами анимации живёт в кэшах
                // глифов (`crate::glyph_cache`).
                if cache_key.look.is_time_sampled() {
                    self.cue_cache
                        .insert_on_probation(cache_key, Arc::clone(&shared), bytes);
                } else {
                    self.cue_cache.insert(cache_key, Arc::clone(&shared), bytes);
                }
                Some(shared)
            }
            Err(error) => {
                self.counters.failed_cues += 1;
                // Текст реплики в журнал не пишем: он может быть приватным.
                tracing::warn!(
                    %error,
                    source = key.source().get(),
                    event = key.event().get(),
                    "subtitle cue skipped: rendering failed"
                );
                None
            }
        }
    }

    /// Часть ключа кэша, зависящая от вида реплики (см. [`CueLook`]).
    fn cue_look(
        content: CueContent<'_>,
        request: &RenderRequest<'_>,
        animation: Option<&AssAnimation>,
    ) -> CueLook {
        match content {
            CueContent::Text(text) => CueLook::Text {
                appearance: *request.appearance,
                hidden_spans: reveal::hidden_spans(text, request.moment),
            },
            CueContent::Picture(_) => CueLook::Picture,
            CueContent::Ass { .. } => CueLook::Ass {
                options: request.ass,
                user_appearance: (request.ass.style_source == AssStyleSource::User)
                    .then_some(*request.appearance),
                animation: animation.map_or(AssAnimationKey::TimeFree, AssAnimation::cache_key),
            },
        }
    }

    /// Собирает `SubtitleOverlay`, определяя ревизию и признак изменения относительно прошлого кадра.
    fn assemble_overlay(
        &mut self,
        canvas: CanvasSize,
        bitmaps: Vec<SubtitleBitmap>,
    ) -> SubtitleOverlay {
        let (revision, change) = match self.frame.as_ref().map(|frame| &frame.overlay) {
            None => (OverlayRevision::INITIAL.next(), OverlayChange::Replaced),
            Some(previous) if previous.canvas() != canvas => {
                (previous.revision().next(), OverlayChange::Replaced)
            }
            Some(previous) => match detect_overlay_change(previous.bitmaps(), &bitmaps) {
                OverlayChange::Unchanged => (previous.revision(), OverlayChange::Unchanged),
                changed => (previous.revision().next(), changed),
            },
        };
        match SubtitleOverlay::new(canvas, bitmaps, revision, change) {
            Ok(overlay) => overlay,
            Err(error) => {
                // Недостижимо при исправном рисовании (картинки обрезаны по холсту); но если
                // инвариант нарушен, лучше показать пустой кадр, чем уронить воспроизведение.
                tracing::error!(%error, "subtitle overlay violated the canvas invariant, dropping it");
                SubtitleOverlay::empty(canvas, revision, OverlayChange::Replaced)
            }
        }
    }
}

#[cfg(test)]
mod tests;
