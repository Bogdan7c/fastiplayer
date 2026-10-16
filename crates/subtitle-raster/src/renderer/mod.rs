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
//! - Если запрос совпал с прошлым (те же ключи в том же порядке, холст, стиль, языковое предпочтение), работа не
//!   делается вообще: возвращается прошлый результат с `Unchanged` (быстрый путь).
//!
//! ## Политика ошибок
//! Ошибка субтитров не должна ронять воспроизведение: реплика, которую не удалось нарисовать,
//! пропускается (`tracing::warn` без текста реплики, счётчик `failed_cues`), остальные
//! рисуются. Фатальных ошибок у `render` нет, поэтому он возвращает `&SubtitleOverlay`, а не
//! `Result`: вызывающему нечего обрабатывать иначе, чем «показать что есть».

mod collisions;
mod cue_render;
mod frame;
mod limits;
mod request;
mod statistics;

use std::sync::Arc;

use subtitle_core::SubtitleTextCue;

use crate::appearance::SubtitleAppearance;
use crate::cache::ByteLruCache;
use crate::face_cache::FaceCache;
use crate::fonts::{
    AttachedFontError, AttachedFontLimits, AttachedFontRegistration, AttachedFontStatus,
    AttachedFontsClearOutcome, AttachedFontsInstallReport, PreparedAttachedFont,
    PreparedAttachedFonts, SubtitleFontLibrary, SystemFontScanReport, SystemFontScanStatus,
    TextLanguagePreference,
};
use crate::overlay::{
    CanvasSize, OverlayChange, OverlayRevision, SubtitleBitmap, SubtitleOverlay,
    detect_overlay_change,
};
use collisions::{CollisionItem, resolve_vertical_shifts};
use cue_render::{CuePainter, RasterizedCue, rasterize_cue};
use frame::{FrameSignature, FrameState};
use limits::cue_char_count;

pub use limits::{RasterInputLimits, RasterLimits};
pub use request::{CueKey, CueSourceId, RasterCacheLimits, RenderRequest, RenderableCue};
use statistics::Counters;
pub use statistics::RasterStatistics;

/// Ключ кэша готовых реплик.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CueCacheKey {
    cue: CueKey,
    canvas: CanvasSize,
    appearance: SubtitleAppearance,
    /// Язык меняет выбор CJK-лица, а значит и маски: другой язык — другая запись.
    text_language: TextLanguagePreference,
}

/// Растеризатор субтитров.
pub struct SubtitleRasterizer {
    fonts: SubtitleFontLibrary,
    faces: FaceCache,
    cue_cache: ByteLruCache<CueCacheKey, RasterizedCue>,
    limits: RasterLimits,
    /// Рисовальщик реплик (в продакшене всегда `rasterize_cue`, см. [`CuePainter`]).
    painter: CuePainter,
    frame: Option<FrameState>,
    counters: Counters,
}

impl SubtitleRasterizer {
    /// Создаёт растеризатор поверх библиотеки шрифтов (встроенные уже загружены;
    /// системные и вложенные подключаются методами растеризатора).
    pub fn new(fonts: SubtitleFontLibrary, limits: RasterLimits) -> Self {
        Self {
            fonts,
            faces: FaceCache::new(),
            cue_cache: ByteLruCache::new(limits.cache.cue_bitmaps_bytes),
            limits,
            painter: rasterize_cue,
            frame: None,
            counters: Counters::default(),
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
        });
        &frame.overlay
    }

    /// Счётчики кэшей и работы растеризатора.
    pub fn statistics(&self) -> RasterStatistics {
        let fonts = self.fonts.statistics();
        RasterStatistics {
            cue_cache: self.cue_cache.statistics(),
            face_cache: self.faces.statistics(),
            rasterized_cues: self.counters.rasterized_cues,
            failed_cues: self.counters.failed_cues,
            rendered_frames: self.counters.rendered_frames,
            unchanged_frames: self.counters.unchanged_frames,
            missing_glyph_lookups: fonts.missing_glyph_lookups,
            rejected_cues_too_long: self.counters.rejected_cues_too_long,
            dropped_cues_over_frame_limit: self.counters.dropped_cues_over_frame_limit,
            rejected_canvas: self.counters.rejected_canvas,
            skipped_glyphs: self.counters.skipped_glyphs,
            skipped_borders: self.counters.skipped_borders,
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
        self.faces.clear();
        if let Some(frame) = self.frame.as_mut() {
            frame.stale = true;
        }
    }

    /// Собирает новый кадр по запросу.
    fn build_frame(&mut self, request: &RenderRequest<'_>) -> FrameState {
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
            };
        }
        // Лица вытесняем между кадрами: внутри реплики на них держатся ссылки.
        self.faces.trim_to(self.limits.cache.shaper_faces);

        // Лишние реплики отбрасываем с конца: первые по порядку запроса важнее.
        let admitted = request.cues.len().min(self.limits.input.max_cues_per_frame);
        let dropped = request.cues.len() - admitted;
        if dropped > 0 {
            self.counters.dropped_cues_over_frame_limit += dropped as u64;
            tracing::warn!(
                dropped,
                max_cues = self.limits.input.max_cues_per_frame,
                "too many subtitle cues in one frame, the extra ones are dropped"
            );
        }
        let cues: Vec<Arc<RasterizedCue>> = request
            .cues
            .iter()
            .take(admitted)
            .filter_map(|cue| self.cue_for(cue.key, cue.cue, request))
            .collect();
        let items: Vec<CollisionItem> = cues
            .iter()
            .map(|cue| CollisionItem {
                extent: cue.extent,
                direction: cue.collision,
            })
            .collect();
        let shifts = resolve_vertical_shifts(&items, request.canvas.height());
        let mut bitmaps: Vec<SubtitleBitmap> = Vec::new();
        for (cue, shift) in cues.iter().zip(shifts) {
            bitmaps.extend(cue.bitmaps.iter().map(|bitmap| {
                if shift == 0 {
                    bitmap.clone()
                } else {
                    bitmap.shifted_vertically(shift)
                }
            }));
        }

        let overlay = self.assemble_overlay(request.canvas, bitmaps);
        FrameState {
            signature,
            overlay,
            stale: false,
        }
    }

    /// Готовая реплика из кэша или свежая; `None` — реплику не удалось нарисовать или она
    /// отклонена лимитом длины.
    fn cue_for(
        &mut self,
        key: CueKey,
        cue: &SubtitleTextCue,
        request: &RenderRequest<'_>,
    ) -> Option<Arc<RasterizedCue>> {
        let cache_key = CueCacheKey {
            cue: key,
            canvas: request.canvas,
            appearance: *request.appearance,
            text_language: request.text_language,
        };
        // Попадание в кэш означает, что реплика уже прошла проверку длины: повторно не считаем.
        if let Some(hit) = self.cue_cache.get(&cache_key) {
            return Some(hit);
        }
        let chars = cue_char_count(&cue.text);
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
        match (self.painter)(
            &mut self.fonts,
            &mut self.faces,
            cue,
            request.appearance,
            request.canvas,
            request.text_language,
        ) {
            Ok(rasterized) => {
                self.counters.rasterized_cues += 1;
                self.counters.skipped_glyphs += u64::from(rasterized.diagnostics.skipped_glyphs);
                self.counters.skipped_borders += u64::from(rasterized.diagnostics.skipped_borders);
                let bytes = rasterized.cache_bytes();
                let shared = Arc::new(rasterized);
                self.cue_cache.insert(cache_key, Arc::clone(&shared), bytes);
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
