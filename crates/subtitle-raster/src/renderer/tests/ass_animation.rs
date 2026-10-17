//! Анимации ASS (`\move`, `\fad`, `\fade`) через публичный `SubtitleRasterizer::render`:
//! файл `.ass` на входе, момент показа в запросе, картинки и отчёт об изменениях на выходе.
//!
//! Проверяется то, что видно на экране (где стоят чернила, какая у них непрозрачность) и то,
//! что обещает рисовальщику результат (`animation_change`, быстрый путь, счётчики работы).

use subtitle_core::SubtitleTime;

use super::ass::{fresh_source, glyph_ink};
use super::*;
use crate::ass::AssRenderOptions;
use crate::overlay::{AnimationChange, OverlayPlacementGroup};
use crate::test_ass::{AssTestCue, DEFAULT_STYLE_LINE, ass_file, dialogue, load_ass_cues};

/// Реплики файла с событиями стиля `Default` по 10 секунд каждое.
fn cues_of(texts: &[&str]) -> Vec<AssTestCue> {
    let lines: Vec<String> = texts.iter().map(|text| dialogue(0, text)).collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let file = ass_file((1280, 720), true, &[DEFAULT_STYLE_LINE], &refs);
    load_ass_cues(&file, fresh_source())
}

/// Рисует реплики в момент `moment_ms` от начала событий.
fn render_at(
    rasterizer: &mut SubtitleRasterizer,
    cues: &[AssTestCue],
    moment_ms: i64,
) -> SubtitleOverlay {
    let list: Vec<RenderableCue<'_>> = cues.iter().map(AssTestCue::renderable).collect();
    let appearance = SubtitleAppearance::youtube_like();
    rasterizer
        .render(&RenderRequest {
            moment: SubtitleTime::from_millis(moment_ms),
            cues: &list,
            canvas: canvas(),
            appearance: &appearance,
            text_language: crate::fonts::TextLanguagePreference::unknown(),
            ass: AssRenderOptions::default(),
        })
        .clone()
}

fn at(millis: i64) -> AnimationChange {
    AnimationChange::At(SubtitleTime::from_millis(millis))
}

/// Левый край чернил букв.
fn ink_left(overlay: &SubtitleOverlay) -> i32 {
    glyph_ink(overlay).0
}

/// Непрозрачность заливки букв (первая картинка-глиф).
fn glyph_opacity(overlay: &SubtitleOverlay) -> u8 {
    of_kind(overlay, SubtitleBitmapKind::Glyph)
        .first()
        .map(|bitmap| bitmap.color().alpha)
        .expect("glyph bitmap")
}

const MOVE_RIGHT: &str = r"{\an7\bord0\move(100,100,500,100,0,1000)}HHHH";

#[test]
fn move_stands_between_the_endpoints_in_the_middle_of_the_interval() {
    let cues = cues_of(&[MOVE_RIGHT]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let start = ink_left(&render_at(&mut rasterizer, &cues, 0));
    let middle = ink_left(&render_at(&mut rasterizer, &cues, 500));
    let end = ink_left(&render_at(&mut rasterizer, &cues, 1000));
    assert!(start < middle && middle < end, "{start} {middle} {end}");
    // Линейно: середина — посередине пути (запас на округление краёв чернил).
    assert!(
        (middle - (start + end) / 2).abs() <= 2,
        "{start} {middle} {end}"
    );
    // Путь по горизонтали — 400 px, как в теге.
    assert!(((end - start) - 400).abs() <= 2, "{start} {end}");
}

#[test]
fn move_keeps_the_vertical_position_when_only_x_changes() {
    let cues = cues_of(&[MOVE_RIGHT]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = glyph_ink(&render_at(&mut rasterizer, &cues, 0));
    let later = glyph_ink(&render_at(&mut rasterizer, &cues, 700));
    assert_eq!((first.1, first.3), (later.1, later.3));
}

#[test]
fn move_holds_endpoints_before_t1_and_after_t2() {
    let cues = cues_of(&[r"{\an7\bord0\move(100,100,500,100,200,800)}HHHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let before = ink_left(&render_at(&mut rasterizer, &cues, 50));
    let at_t1 = ink_left(&render_at(&mut rasterizer, &cues, 200));
    let after = ink_left(&render_at(&mut rasterizer, &cues, 800));
    let much_later = ink_left(&render_at(&mut rasterizer, &cues, 5000));
    assert_eq!(before, at_t1);
    assert_eq!(after, much_later);
    assert!(after - before >= 398, "{before} {after}");
}

#[test]
fn move_with_swapped_times_behaves_like_ordered_times() {
    let ordered = cues_of(&[r"{\an7\bord0\move(100,100,500,100,200,800)}HHHH"]);
    let swapped = cues_of(&[r"{\an7\bord0\move(100,100,500,100,800,200)}HHHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    for moment in [100, 500, 900] {
        let expected = ink_left(&render_at(&mut rasterizer, &ordered, moment));
        let actual = ink_left(&render_at(&mut rasterizer, &swapped, moment));
        assert_eq!(expected, actual, "момент {moment}");
    }
}

#[test]
fn move_after_pos_is_ignored() {
    let pos_only = cues_of(&[r"{\an7\bord0\pos(300,100)}HHHH"]);
    let pos_then_move = cues_of(&[r"{\an7\bord0\pos(300,100)\move(100,100,900,100,0,1000)}HHHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let expected = glyph_ink(&render_at(&mut rasterizer, &pos_only, 500));
    let actual = render_at(&mut rasterizer, &pos_then_move, 500);
    assert_eq!(glyph_ink(&actual), expected);
    // Реплика стоит, значит и отчёта об анимации нет.
    assert_eq!(actual.animation_change(), AnimationChange::None);
}

#[test]
fn fade_in_is_dimmer_at_the_start_than_in_the_middle_of_the_fade() {
    let cues = cues_of(&[r"{\an7\bord0\pos(100,100)\fad(1000,0)}HHHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let early = glyph_opacity(&render_at(&mut rasterizer, &cues, 200));
    let middle = glyph_opacity(&render_at(&mut rasterizer, &cues, 600));
    let full = glyph_opacity(&render_at(&mut rasterizer, &cues, 1500));
    assert!(early < middle && middle < full, "{early} {middle} {full}");
    assert_eq!(full, 255);
}

#[test]
fn fade_to_full_transparency_draws_no_bitmaps() {
    let cues = cues_of(&[r"{\an7\bord3\pos(100,100)\fad(1000,1000)}HHHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    // В самом начале реплики прозрачность затухания 255: libass выбрасывает картинки.
    let invisible = render_at(&mut rasterizer, &cues, 0);
    assert!(
        invisible.is_empty(),
        "{} картинок",
        invisible.bitmaps().len()
    );
    // Тот же ключ реплики чуть позже уже виден.
    assert!(!render_at(&mut rasterizer, &cues, 500).is_empty());
}

#[test]
fn fade_out_dims_the_text_toward_the_end_of_the_event() {
    let cues = cues_of(&[r"{\an7\bord0\pos(100,100)\fad(0,1000)}HHHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let steady = glyph_opacity(&render_at(&mut rasterizer, &cues, 5000));
    let fading = glyph_opacity(&render_at(&mut rasterizer, &cues, 9500));
    assert_eq!(steady, 255);
    assert!(fading < steady && fading > 0, "{fading}");
}

#[test]
fn fade_dims_outline_and_shadow_together_with_the_fill() {
    let cues = cues_of(&[r"{\an7\bord4\shad4\pos(100,100)\fad(1000,0)}HHHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let half = render_at(&mut rasterizer, &cues, 500);
    let full = render_at(&mut rasterizer, &cues, 2000);
    for kind in [
        SubtitleBitmapKind::Shadow,
        SubtitleBitmapKind::Outline,
        SubtitleBitmapKind::Glyph,
    ] {
        let dim = of_kind(&half, kind).first().map(|b| b.color().alpha);
        let bright = of_kind(&full, kind).first().map(|b| b.color().alpha);
        let (Some(dim), Some(bright)) = (dim, bright) else {
            panic!("слой {kind:?} должен нарисоваться в обоих моментах");
        };
        assert!(dim < bright, "{kind:?}: {dim} против {bright}");
    }
}

#[test]
fn animation_change_is_none_for_a_static_scene() {
    let cues = cues_of(&[r"{\an7\bord0\pos(100,100)}Static", "Plain text"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    assert_eq!(
        render_at(&mut rasterizer, &cues, 1234).animation_change(),
        AnimationChange::None
    );
}

#[test]
fn animation_change_for_move_reports_before_during_and_after() {
    let cues = cues_of(&[r"{\an7\bord0\move(100,100,500,100,200,800)}HHHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    assert_eq!(
        render_at(&mut rasterizer, &cues, 50).animation_change(),
        at(201)
    );
    assert_eq!(
        render_at(&mut rasterizer, &cues, 500).animation_change(),
        AnimationChange::EveryFrame
    );
    assert_eq!(
        render_at(&mut rasterizer, &cues, 800).animation_change(),
        AnimationChange::None
    );
}

#[test]
fn animation_change_for_fade_reports_the_phases() {
    let cues = cues_of(&[r"{\an7\bord0\pos(100,100)\fad(300,400)}HHHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    assert_eq!(
        render_at(&mut rasterizer, &cues, 100).animation_change(),
        AnimationChange::EveryFrame
    );
    // Середина: картинка стоит до начала угасания (событие 10 с, угасание с 9600 мс).
    assert_eq!(
        render_at(&mut rasterizer, &cues, 5000).animation_change(),
        at(9600)
    );
    assert_eq!(
        render_at(&mut rasterizer, &cues, 9800).animation_change(),
        AnimationChange::EveryFrame
    );
}

#[test]
fn animation_change_takes_the_earliest_over_several_events() {
    let cues = cues_of(&[
        r"{\an7\bord0\move(100,100,500,100,3000,4000)}Late",
        r"{\an7\bord0\pos(100,300)\fad(0,400)}Fade",
    ]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    // Движение стартует с 3001, угасание — с 9600: ближайшее — движение.
    assert_eq!(
        render_at(&mut rasterizer, &cues, 1000).animation_change(),
        at(3001)
    );
}

#[test]
fn static_scene_is_served_without_work_at_any_moment() {
    let cues = cues_of(&[r"{\an7\bord0\pos(100,100)}Static"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = render_at(&mut rasterizer, &cues, 100);
    let stats = rasterizer.statistics();
    // Другой момент, даже более ранний: ничего не рисуется заново.
    let second = render_at(&mut rasterizer, &cues, 7000);
    let third = render_at(&mut rasterizer, &cues, 10);
    let after = rasterizer.statistics();
    assert_eq!(after.rendered_frames, stats.rendered_frames);
    assert_eq!(after.unchanged_frames, stats.unchanged_frames + 2);
    assert_eq!(after.rasterized_cues, stats.rasterized_cues);
    assert_eq!(second.change(), OverlayChange::Unchanged);
    assert_eq!(third.revision(), first.revision());
}

#[test]
fn animated_scene_is_not_served_stale_between_frames() {
    let cues = cues_of(&[MOVE_RIGHT]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = render_at(&mut rasterizer, &cues, 100);
    let second = render_at(&mut rasterizer, &cues, 200);
    assert_ne!(second.change(), OverlayChange::Unchanged);
    assert!(second.revision() > first.revision());
    assert!(ink_left(&second) > ink_left(&first));
    // Тот же момент подряд (пауза): быстрый путь, работы нет.
    let rendered = rasterizer.statistics().rendered_frames;
    let repeat = render_at(&mut rasterizer, &cues, 200);
    assert_eq!(repeat.change(), OverlayChange::Unchanged);
    assert_eq!(rasterizer.statistics().rendered_frames, rendered);
}

#[test]
fn scene_before_a_move_is_served_until_the_move_starts_but_not_after() {
    let cues = cues_of(&[r"{\an7\bord0\move(100,100,500,100,1000,2000)}HHHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = render_at(&mut rasterizer, &cues, 100);
    let still = render_at(&mut rasterizer, &cues, 900);
    assert_eq!(still.change(), OverlayChange::Unchanged);
    assert_eq!(still.revision(), first.revision());
    let moving = render_at(&mut rasterizer, &cues, 1500);
    assert_ne!(moving.change(), OverlayChange::Unchanged);
    assert!(ink_left(&moving) > ink_left(&first));
}

#[test]
fn finished_animation_is_served_forward_but_rewinding_redraws() {
    let cues = cues_of(&[r"{\an7\bord0\move(100,100,500,100,0,1000)}HHHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let settled = render_at(&mut rasterizer, &cues, 2000);
    let later = render_at(&mut rasterizer, &cues, 4000);
    assert_eq!(later.change(), OverlayChange::Unchanged);
    // Перемотка назад в середину движения: кадр обязан измениться.
    let rewound = render_at(&mut rasterizer, &cues, 500);
    assert_ne!(rewound.change(), OverlayChange::Unchanged);
    assert!(ink_left(&rewound) < ink_left(&settled));
}

#[test]
fn static_cue_is_not_rasterized_again_next_to_an_animated_one() {
    let cues = cues_of(&[
        r"{\an7\bord0\pos(100,500)}Static",
        r"{\an7\bord0\move(100,100,500,100,0,1000)}Moving",
    ]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    render_at(&mut rasterizer, &cues, 0);
    assert_eq!(rasterizer.statistics().rasterized_cues, 2);
    render_at(&mut rasterizer, &cues, 100);
    render_at(&mut rasterizer, &cues, 200);
    // Статичная реплика взята из кэша, перерисована только движущаяся.
    assert_eq!(rasterizer.statistics().rasterized_cues, 4);
}

#[test]
fn identical_animation_states_hit_the_cache() {
    let cues = cues_of(&[MOVE_RIGHT]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    render_at(&mut rasterizer, &cues, 300);
    render_at(&mut rasterizer, &cues, 600);
    assert_eq!(rasterizer.statistics().rasterized_cues, 2);
    // Возврат в уже виденное состояние: в кэше, рисовать не нужно.
    render_at(&mut rasterizer, &cues, 300);
    assert_eq!(rasterizer.statistics().rasterized_cues, 2);
    // Положение застыло после конца движения: все моменты после `t2` — одно состояние.
    render_at(&mut rasterizer, &cues, 1500);
    render_at(&mut rasterizer, &cues, 3000);
    assert_eq!(rasterizer.statistics().rasterized_cues, 3);
}

#[test]
fn moving_cues_are_exempt_from_collisions_and_belong_to_the_fixed_group() {
    // Две одинаковые движущиеся реплики стоят друг на друге: сдвига нет, как у `\pos`.
    let cues = cues_of(&[
        r"{\an2\bord0\move(640,600,640,600,0,1000)}Same",
        r"{\an2\bord0\move(640,600,640,600,0,1000)}Same",
    ]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = render_at(&mut rasterizer, &cues, 500);
    let glyphs = of_kind(&overlay, SubtitleBitmapKind::Glyph);
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].origin(), glyphs[1].origin());

    let moving = cues_of(&[r"{\an2\bord0\move(100,600,900,600,0,1000)}Sign"]);
    let overlay = render_at(&mut rasterizer, &moving, 500);
    assert!(!overlay.is_empty());
    assert!(
        overlay
            .bitmaps()
            .iter()
            .all(|bitmap| bitmap.placement_group() == OverlayPlacementGroup::Fixed)
    );
}

#[test]
fn move_rotating_around_the_current_position_without_org() {
    // Без `\org` центр поворота следует за текущей точкой: реплика не «орбитирует».
    let cues = cues_of(&[r"{\an5\bord0\frz90\move(300,360,900,360,0,1000)}HHHH"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let start = glyph_ink(&render_at(&mut rasterizer, &cues, 0));
    let end = glyph_ink(&render_at(&mut rasterizer, &cues, 1000));
    // Размер и вертикальное положение не меняются, сдвиг ровно на путь `\move`.
    assert_eq!(
        (start.3 - start.1, start.2 - start.0),
        (end.3 - end.1, end.2 - end.0)
    );
    assert_eq!((start.1, start.3), (end.1, end.3));
    assert!(((end.0 - start.0) - 600).abs() <= 2, "{start:?} {end:?}");
}

#[test]
fn text_and_ass_renders_share_the_rasterizer_without_interference() {
    // Кадр, где рядом простой текст и анимированный ASS: текст берётся из кэша, ASS движется.
    let ass = cues_of(&[MOVE_RIGHT]);
    let text = cues(fresh_source(), &["Plain"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let appearance = SubtitleAppearance::youtube_like();
    let mut render = |moment_ms: i64| {
        let mut list = renderables(&text);
        list.extend(ass.iter().map(AssTestCue::renderable));
        rasterizer
            .render(&RenderRequest {
                moment: SubtitleTime::from_millis(moment_ms),
                cues: &list,
                canvas: canvas(),
                appearance: &appearance,
                text_language: crate::fonts::TextLanguagePreference::unknown(),
                ass: AssRenderOptions::default(),
            })
            .clone()
    };
    let first = render(100);
    let second = render(400);
    assert_ne!(second.change(), OverlayChange::Unchanged);
    assert_eq!(second.animation_change(), AnimationChange::EveryFrame);
    assert_eq!(first.bitmaps().len(), second.bitmaps().len());
}

#[test]
fn fade_tag_mid_line_fades_only_the_run_that_starts_after_it() {
    // Цвет рвёт участок: «World» (красный) начинается после тега и гаснет, «Hello» — нет.
    let cues = cues_of(&[r"{\an5\bord0\pos(640,360)}Hello{\1c&H0000FF&\fad(1000,0)}World"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = render_at(&mut rasterizer, &cues, 500);
    let glyphs = of_kind(&overlay, SubtitleBitmapKind::Glyph);
    assert_eq!(glyphs.len(), 2);
    let white = glyphs.iter().find(|bitmap| bitmap.color().green == 255);
    let red = glyphs.iter().find(|bitmap| bitmap.color().green == 0);
    let (Some(white), Some(red)) = (white, red) else {
        panic!("ожидались белый и красный участки");
    };
    assert_eq!(white.color().alpha, 255, "текст до тега не гаснет");
    assert!(red.color().alpha < 255 && red.color().alpha > 0);
}

#[test]
fn fade_tag_mid_line_without_a_run_split_fades_nothing_like_libass() {
    // Одинаковый стиль до и после тега: участок один и берёт затухание первого символа (0).
    let cues = cues_of(&[r"{\an5\bord0\pos(640,360)}Hello{\fad(1000,0)}World"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let early = render_at(&mut rasterizer, &cues, 100);
    assert_eq!(glyph_opacity(&early), 255);
    assert!(!early.is_empty());
}
