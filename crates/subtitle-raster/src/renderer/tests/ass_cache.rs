//! Кэш и память положений ASS через публичный `SubtitleRasterizer::render`.

use super::ass::{fresh_source, glyph_ink, render_ass, render_ass_with, single};
use super::*;
use crate::ass::{AssRenderOptions, AssStyleSource, AssTextScale};
use crate::test_ass::{DEFAULT_STYLE_LINE, ass_file, dialogue, load_ass_cues};

fn two_bottom_events() -> Vec<crate::test_ass::AssTestCue> {
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE],
        &[
            &dialogue(0, r"{\bord0}Lower event"),
            &dialogue(0, r"{\bord0}Upper event"),
        ],
    );
    load_ass_cues(&file, fresh_source())
}

#[test]
fn repeating_an_ass_request_is_unchanged_and_option_change_rerenders() {
    let cues = single("Cache me");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = render_ass(&mut rasterizer, &cues);
    assert_eq!(first.change(), OverlayChange::Replaced);
    let rasterized_before = rasterizer.statistics().rasterized_cues;

    let second = render_ass(&mut rasterizer, &cues);
    assert_eq!(second.change(), OverlayChange::Unchanged);
    assert_eq!(second.revision(), first.revision());
    assert_eq!(rasterizer.statistics().rasterized_cues, rasterized_before);

    // Другой масштаб текста — другая картинка (обычная реплика не «явная»).
    let scaled = AssRenderOptions {
        text_scale: AssTextScale::new(1.4).expect("scale"),
        ..AssRenderOptions::default()
    };
    let appearance = SubtitleAppearance::youtube_like();
    let third = render_ass_with(&mut rasterizer, &cues, scaled, &appearance, canvas());
    assert_ne!(third.change(), OverlayChange::Unchanged);
    assert!(third.revision() > second.revision());
    assert_eq!(
        rasterizer.statistics().rasterized_cues,
        rasterized_before + 1
    );
    assert!(
        glyph_ink(&third).3 - glyph_ink(&third).1 > glyph_ink(&second).3 - glyph_ink(&second).1
    );
}

#[test]
fn ass_option_change_does_not_invalidate_text_cues() {
    let text = cues(1, &["Plain subtitle"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let appearance = SubtitleAppearance::youtube_like();
    let list = renderables(&text);
    let render_with = |rasterizer: &mut SubtitleRasterizer, ass: AssRenderOptions| {
        rasterizer
            .render(&RenderRequest {
                moment: subtitle_core::SubtitleTime::ZERO,
                cues: &list,
                canvas: canvas(),
                appearance: &appearance,
                text_language: crate::fonts::TextLanguagePreference::unknown(),
                ass,
            })
            .clone()
    };
    let first = render_with(&mut rasterizer, AssRenderOptions::default());
    let rasterized = rasterizer.statistics().rasterized_cues;
    let user = AssRenderOptions {
        style_source: AssStyleSource::User,
        text_scale: AssTextScale::new(2.0).expect("scale"),
        ..AssRenderOptions::default()
    };
    let second = render_with(&mut rasterizer, user);
    // Ни перерисовки реплики, ни нового кадра: настройки ASS простой текст не меняют.
    assert_eq!(rasterizer.statistics().rasterized_cues, rasterized);
    assert_eq!(second.change(), OverlayChange::Unchanged);
    assert_eq!(second.revision(), first.revision());
}

#[test]
fn author_look_is_not_rerendered_when_only_the_user_appearance_changes() {
    let cues = single("Author look");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let options = AssRenderOptions::default();
    render_ass_with(
        &mut rasterizer,
        &cues,
        options,
        &SubtitleAppearance::youtube_like(),
        canvas(),
    );
    let rasterized = rasterizer.statistics().rasterized_cues;
    render_ass_with(
        &mut rasterizer,
        &cues,
        options,
        &SubtitleAppearance::outline_and_shadow(),
        canvas(),
    );
    assert_eq!(rasterizer.statistics().rasterized_cues, rasterized);
    // В режиме «мой стиль» смена пользовательского стиля перерисовывает реплику.
    let user = AssRenderOptions {
        style_source: AssStyleSource::User,
        ..options
    };
    render_ass_with(
        &mut rasterizer,
        &cues,
        user,
        &SubtitleAppearance::youtube_like(),
        canvas(),
    );
    let after_user = rasterizer.statistics().rasterized_cues;
    render_ass_with(
        &mut rasterizer,
        &cues,
        user,
        &SubtitleAppearance::outline_and_shadow(),
        canvas(),
    );
    assert_eq!(rasterizer.statistics().rasterized_cues, after_user + 1);
}

#[test]
fn collision_positions_are_remembered_while_the_neighbour_goes_away() {
    let cues = two_bottom_events();
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let both = render_ass(&mut rasterizer, &cues);
    let upper_with_neighbour = ink_bounds(
        of_kind(&both, SubtitleBitmapKind::Glyph)[1..]
            .iter()
            .copied(),
    )
    .expect("upper ink");
    // Нижняя реплика исчезла: верхняя остаётся там, где встала (libass: `priv->top`).
    let alone = render_ass(&mut rasterizer, &cues[1..]);
    assert_eq!(glyph_ink(&alone), upper_with_neighbour);
    // Другой холст сбрасывает память: верхняя реплика встаёт на обычное место внизу.
    let other_canvas = CanvasSize::new(640, 360).expect("canvas");
    let fresh = render_ass_with(
        &mut rasterizer,
        &cues[1..],
        AssRenderOptions::default(),
        &SubtitleAppearance::youtube_like(),
        other_canvas,
    );
    let ink = glyph_ink(&fresh);
    assert!(ink.3 > 330, "bottom event on the smaller canvas: {ink:?}");
}

#[test]
fn collision_memory_is_bounded_by_active_cues() {
    let cues = two_bottom_events();
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    render_ass(&mut rasterizer, &cues);
    assert_eq!(rasterizer.ass_collisions.len(), 2);
    render_ass(&mut rasterizer, &cues[..1]);
    assert_eq!(rasterizer.ass_collisions.len(), 1);
    render_ass(&mut rasterizer, &[]);
    assert_eq!(rasterizer.ass_collisions.len(), 0);
}
