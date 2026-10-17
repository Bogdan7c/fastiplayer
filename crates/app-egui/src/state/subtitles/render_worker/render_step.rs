//! Один шаг рисования внутри потока: реплики на момент `lookup_time` -> `PreparedOverlay`.
//!
//! Функция не знает ни о каналах, ни о планировании: ей отдают растеризатор, зеркало дорожки,
//! входы рисования и холст. Так шаг проверяется отдельно от потока.

use std::time::Instant;

use subtitle_core::{SubtitleContent, SubtitleEventId, SubtitleLanguage, SubtitleTime};
use subtitle_raster::{
    CanvasSize, CueKey, RenderRequest, RenderableCue, SubtitleRasterizer, TextLanguagePreference,
};

use super::commands::{LookInputs, RenderReport};
use super::prepared_queue::{OverlayIdentity, PreparedOverlay};
use super::track_mirror::MirroredTrack;
use super::validity::ValidityInterval;

/// Итог шага.
pub(super) enum StepOutcome {
    /// На этом времени реплик нет: рисовать нечего, но интервал «пусто» известен.
    NothingActive { interval: ValidityInterval },
    /// Картинка нарисована.
    Rendered {
        picture: PreparedOverlay,
        report: RenderReport,
    },
}

/// Всё, что нужно шагу кроме растеризатора.
pub(super) struct StepInput<'step> {
    pub(super) identity: OverlayIdentity,
    pub(super) canvas: CanvasSize,
    pub(super) track: &'step MirroredTrack,
    pub(super) look: &'step LookInputs,
    pub(super) system_language: Option<&'step SubtitleLanguage>,
    pub(super) lookup_time: SubtitleTime,
}

/// Рисует реплики, активные на `lookup_time`. `valid_until` берётся у таймлайна:
/// ближайшая смена набора реплик после этого времени.
pub(super) fn render_at(
    rasterizer: &mut SubtitleRasterizer,
    input: &StepInput<'_>,
    scratch_active: &mut Vec<SubtitleEventId>,
) -> StepOutcome {
    let timeline = &input.track.timeline;
    timeline.collect_active_at(input.lookup_time, scratch_active);
    let interval = ValidityInterval::new(
        input.lookup_time,
        timeline.next_change_after(input.lookup_time),
    );
    if scratch_active.is_empty() {
        return StepOutcome::NothingActive { interval };
    }

    let source = input.identity.source;
    let mut cues: Vec<RenderableCue<'_>> = Vec::with_capacity(scratch_active.len());
    let mut contains_ass = false;
    let mut skipped_unsupported_cues = 0_u64;
    for &event_id in scratch_active.iter() {
        let Some(event) = timeline.event(event_id) else {
            continue;
        };
        match event.content() {
            SubtitleContent::Text(cue) => {
                cues.push(RenderableCue::text(CueKey::new(source, event_id), cue));
            }
            SubtitleContent::Ass(cue) => {
                contains_ass = true;
                cues.push(RenderableCue::ass(
                    CueKey::new(source, event_id),
                    event.layer(),
                    cue,
                ));
            }
            // `SubtitleContent` расширяемый (bitmap и будущее): неподдерживаемое считаем.
            _ => skipped_unsupported_cues += 1,
        }
    }

    // Язык дорожки, иначе системы: тот же выбор, что раньше делал презентатор.
    let text_language = TextLanguagePreference::track_then_system(
        input.track.language.as_ref(),
        input.system_language,
    );
    let started_at = Instant::now();
    let overlay = rasterizer
        .render(&RenderRequest {
            cues: &cues,
            canvas: input.canvas,
            appearance: &input.look.appearance,
            text_language,
            ass: input.look.ass,
        })
        .clone();
    let report = RenderReport {
        elapsed: started_at.elapsed(),
        cue_count: cues.len(),
        skipped_unsupported_cues,
        text_language,
    };
    let picture = PreparedOverlay::new(
        input.identity,
        interval,
        scratch_active.clone(),
        contains_ass,
        overlay,
    );
    StepOutcome::Rendered { picture, report }
}
