//! Прогрев шрифтов в потоке-рисовальщике: выполняется только в простое и только для
//! показываемой дорожки.
//!
//! Тесты ведут состояние потока по одной порции работы (`do_next_work`), без настоящего
//! потока: порядок «картинки раньше прогрева» так проверяется детерминированно, без гонок со
//! временем. Нужное лицо даёт вложенный шрифт-приманка (символ, которого нет во встроенных).

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use subtitle_core::{
    CuePlacement, SpanStyle, StyledText, SubtitleContent, SubtitleEvent, SubtitleLayer,
    SubtitleTextCue, SubtitleTime, SubtitleTimeline, TextSpan,
};
use subtitle_raster::{
    AssRenderOptions, AttachedFontLimits, CanvasSize, CueSourceId, FontLookupCacheLimits,
    FontWarmUp, PreparedAttachedFonts, RasterCacheLimits, RasterInputLimits, RasterLimits,
    SubtitleAppearance, SubtitleFontLibrary,
};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::state::subtitles::render_worker::commands::TrackCommand;
use crate::state::subtitles::render_worker::prepared_queue::RenderInputsRevision;
use crate::state::subtitles::render_worker::schedule::{FrameStep, PlaybackRate};
use crate::state::subtitles::tests::embedded_test_font::font_with_private_glyph;

const ATTACHED_LIMITS: AttachedFontLimits = AttachedFontLimits {
    max_font_bytes: 16 * 1024 * 1024,
    max_total_bytes: 64 * 1024 * 1024,
    max_faces: 8,
};

fn ms(value: i64) -> SubtitleTime {
    SubtitleTime::from_millis(value)
}

fn look() -> LookInputs {
    LookInputs {
        revision: RenderInputsRevision::INITIAL,
        appearance: SubtitleAppearance::youtube_like(),
        ass: AssRenderOptions::default(),
    }
}

/// Дорожка из одной ASCII-реплики на минуту: рисуется встроенными шрифтами.
fn ascii_track(source: CueSourceId) -> TrackCommand {
    let event = SubtitleEvent::new(
        ms(0),
        ms(60_000),
        SubtitleLayer::DEFAULT,
        SubtitleContent::Text(SubtitleTextCue {
            text: StyledText::new(vec![TextSpan::text("Hi", SpanStyle::default())]),
            placement: CuePlacement::default(),
        }),
    )
    .expect("interval");
    let (timeline, _) = SubtitleTimeline::from_events([event]);
    TrackCommand::Replace {
        serial: 1,
        source,
        language: None,
        timeline,
    }
}

fn present(source: CueSourceId) -> WorkerCommand {
    WorkerCommand::Present {
        target: RenderTarget {
            source,
            canvas: CanvasSize::new(640, 360).expect("canvas"),
        },
        clock: PresentationClock {
            current: ms(1_000),
            cadence: FrameCadence::Known(FrameStep::from_micros(40_000).expect("step")),
            rate: PlaybackRate::NORMAL,
            mode: PresentationMode::Playing,
        },
        look: Some(look()),
    }
}

/// Поток с установленным вложенным шрифтом-приманкой и символом, который в нём есть.
fn state_with_decoy_font() -> (WorkerState, Receiver<WorkerEvent>, char) {
    let (events, receiver) = mpsc::channel();
    let sink = EventSink {
        events,
        wake_port: AppWakePort::disconnected(AppWakeOwner::Subtitles),
    };
    let fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 1024,
    })
    .expect("embedded fonts");
    let settings = WorkerSettings {
        raster_limits: RasterLimits {
            cache: RasterCacheLimits {
                cue_bitmaps_bytes: 32 * 1024 * 1024,
                shaper_faces: 8,
                blur_scratch_retained_bytes: 32 * 1024 * 1024,
                glyph_bitmaps_bytes: 40 * 1024 * 1024,
                glyph_composites_bytes: 20 * 1024 * 1024,
                glyph_outline_items: 10_000,
                shaping_cache_bytes: 4 * 1024 * 1024,
            },
            input: RasterInputLimits {
                max_chars_per_cue: 1_000,
                max_cues_per_frame: 16,
                max_ass_events_per_frame: 16,
                max_canvas_side: 8_192,
            },
        },
        system_language: None,
        prerender_frames: PrerenderFrames::new(4),
        prepared_overlays: 8,
    };
    let mut state = WorkerState::new(fonts, settings, sink);
    let (font, decoy) = font_with_private_glyph();
    let prepared = PreparedAttachedFonts::prepare([Arc::<[u8]>::from(font)], &ATTACHED_LIMITS);
    assert!(state.apply(WorkerCommand::InstallAttachedFonts {
        prepared,
        limits: ATTACHED_LIMITS,
        look: None,
    }));
    (state, receiver, decoy)
}

fn warm_up_command(source: CueSourceId, sample: String) -> WorkerCommand {
    WorkerCommand::WarmUpFonts {
        source,
        sample,
        language: None,
    }
}

/// Ведёт поток, пока он не уйдёт в простой; возвращает порции работы по порядку.
fn run_until_idle(state: &mut WorkerState) -> Vec<WorkStep> {
    let mut steps = Vec::new();
    loop {
        let step = state.do_next_work();
        if step == WorkStep::Idle {
            return steps;
        }
        steps.push(step);
        assert!(steps.len() < 200, "the worker must settle, got {steps:?}");
    }
}

#[test]
fn warm_up_runs_only_after_all_pending_pictures_and_prepares_the_face() {
    let (mut state, _events, decoy) = state_with_decoy_font();
    let source = CueSourceId::new(1);
    assert!(state.apply(WorkerCommand::Track(ascii_track(source))));
    assert!(state.apply(present(source)));
    assert!(state.apply(warm_up_command(source, decoy.to_string())));

    let steps = run_until_idle(&mut state);

    let warm_position = steps
        .iter()
        .position(|step| *step == WorkStep::WarmedFonts)
        .expect("the warm-up must run once the worker is idle");
    assert!(warm_position > 0, "a picture is pending, it goes first");
    assert!(
        steps[..warm_position]
            .iter()
            .all(|step| *step == WorkStep::Rendered),
        "only pictures before the warm-up: {steps:?}"
    );
    assert_eq!(
        warm_position,
        steps.len() - 1,
        "no picture work remains after the warm-up: {steps:?}"
    );
    assert_eq!(
        state.last_font_warm_up,
        Some(FontWarmUp::Warmed { faces_loaded: 1 })
    );
    // Лиц два: основное встроенное (для картинок) и вложенное (из прогрева). Его загрузила
    // именно команда: реплика о нём не знает, её текст ASCII.
    assert_eq!(state.rasterizer.statistics().face_cache.cached_faces, 2);
}

#[test]
fn warm_up_without_pending_pictures_runs_immediately_and_only_once() {
    let (mut state, _events, decoy) = state_with_decoy_font();
    let source = CueSourceId::new(1);
    assert!(state.apply(WorkerCommand::Track(ascii_track(source))));
    assert!(state.apply(warm_up_command(source, decoy.to_string())));

    assert_eq!(state.do_next_work(), WorkStep::WarmedFonts);
    assert_eq!(state.do_next_work(), WorkStep::Idle);
    assert_eq!(state.rasterizer.statistics().face_cache.cached_faces, 1);
}

#[test]
fn newer_warm_up_request_replaces_the_pending_one() {
    let (mut state, _events, decoy) = state_with_decoy_font();
    let source = CueSourceId::new(1);
    assert!(state.apply(WorkerCommand::Track(ascii_track(source))));
    assert!(state.apply(warm_up_command(source, "漢字".to_owned())));
    assert!(state.apply(warm_up_command(source, decoy.to_string())));

    assert_eq!(run_until_idle(&mut state), vec![WorkStep::WarmedFonts]);
    assert_eq!(
        state.last_font_warm_up,
        Some(FontWarmUp::Warmed { faces_loaded: 1 })
    );
}

#[test]
fn warm_up_of_a_dismissed_track_is_dropped() {
    let (mut state, _events, decoy) = state_with_decoy_font();
    let first = CueSourceId::new(1);
    let second = CueSourceId::new(2);
    assert!(state.apply(WorkerCommand::Track(ascii_track(first))));
    assert!(state.apply(warm_up_command(first, decoy.to_string())));
    // Пользователь переключил дорожку до того, как поток освободился.
    assert!(state.apply(WorkerCommand::Track(ascii_track(second))));

    assert_eq!(run_until_idle(&mut state), Vec::<WorkStep>::new());
    assert_eq!(state.last_font_warm_up, None);
    assert_eq!(state.rasterizer.statistics().face_cache.misses, 0);
}

#[test]
fn warm_up_is_dropped_when_the_font_set_changes() {
    let (mut state, _events, decoy) = state_with_decoy_font();
    let source = CueSourceId::new(1);
    assert!(state.apply(WorkerCommand::Track(ascii_track(source))));
    assert!(state.apply(warm_up_command(source, decoy.to_string())));
    assert!(state.apply(WorkerCommand::ClearAttachedFonts { look: None }));

    assert_eq!(run_until_idle(&mut state), Vec::<WorkStep>::new());
    assert_eq!(state.last_font_warm_up, None);
}
