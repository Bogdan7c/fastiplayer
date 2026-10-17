//! Караоке через публичный `SubtitleRasterizer::render`: файл `.ass` на входе, момент показа в
//! запросе, картинки, счётчик растеризаций и срок годности кадра на выходе.
//!
//! Главное свойство (решение владельца): буквы караоке-реплики рисуются ОДИН раз, а каждый
//! кадр меняется только раскраска. События длятся 10 секунд (`dialogue`), холст 1280x720.

use subtitle_core::{SubtitleColor, SubtitleTime};

use super::ass::{fresh_source, glyph_ink};
use super::*;
use crate::ass::AssRenderOptions;
use crate::overlay::AnimationChange;
use crate::test_ass::{AssTestCue, DEFAULT_STYLE_LINE, ass_file, dialogue, load_ass_cues};

const GREEN: SubtitleColor = SubtitleColor::opaque(0, 255, 0);
const BLUE: SubtitleColor = SubtitleColor::opaque(0, 0, 255);
/// Основной цвет стиля `Default`.
const WHITE: SubtitleColor = SubtitleColor::opaque(255, 255, 255);

/// Зелёный основной цвет, синий вторичный (в тегах ASS цвета записаны как BGR).
const GREEN_BLUE: &str = r"\1c&H00FF00&\2c&HFF0000&";

fn cues_of(text: &str) -> Vec<AssTestCue> {
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE],
        &[&dialogue(0, text)],
    );
    load_ass_cues(&file, fresh_source())
}

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

fn glyph_colors(overlay: &SubtitleOverlay) -> Vec<SubtitleColor> {
    of_kind(overlay, SubtitleBitmapKind::Glyph)
        .iter()
        .map(|bitmap| bitmap.color())
        .collect()
}

/// Цвет пикселя композита.
fn pixel(composite: &[u8], x: i32, y: i32) -> [u8; 4] {
    let offset = (y as usize * WIDTH as usize + x as usize) * 4;
    composite[offset..offset + 4].try_into().expect("pixel")
}

/// Сколько плотных пикселей композита в столбцах `[from, to)` ближе всего к зелёному и к синему.
fn dominant_pixels(overlay: &SubtitleOverlay, from: i32, to: i32) -> (usize, usize) {
    let composite = overlay.composite_rgba_over_transparent();
    let (mut green, mut blue) = (0, 0);
    for y in 0..HEIGHT as i32 {
        for x in from..to {
            let [red, g, b, alpha] = pixel(&composite, x, y);
            if alpha < 200 {
                continue;
            }
            if g > red && g > b {
                green += 1;
            } else if b > red && b > g {
                blue += 1;
            }
        }
    }
    (green, blue)
}

/// Слог `\kf` в 1000 мс, начинающийся с 200-й мс (`\kf20` перед ним — пустой слог-пропуск).
const FILL_LINE: &str = r"{\an7\pos(100,100)\bord0\fs80\1c&H00FF00&\2c&HFF0000&\kf20\kf100}HHHH";

#[test]
fn kf_paints_primary_left_and_secondary_right_of_a_moving_split_and_rasterizes_once() {
    let cues = cues_of(FILL_LINE);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);

    let before = render_at(&mut rasterizer, &cues, 100);
    assert_eq!(glyph_colors(&before), vec![BLUE], "unsung: secondary only");

    let middle = render_at(&mut rasterizer, &cues, 700);
    let (left, top, right, bottom) = glyph_ink(&middle);
    let pieces = of_kind(&middle, SubtitleBitmapKind::Glyph);
    assert_eq!(pieces.len(), 2, "one piece per side of the split");
    assert_eq!((pieces[0].color(), pieces[1].color()), (GREEN, BLUE));
    let split = pieces[1].origin().x;
    assert_eq!(
        pieces[0].origin().x + pieces[0].width() as i32,
        split,
        "the pieces meet on one vertical line"
    );
    assert!(
        left + 40 < split && split < right - 40,
        "the split is inside the letters: {left} < {split} < {right}"
    );
    // Пиксели композита: слева от границы только зелёный, справа только синий.
    let (green_left, blue_left) = dominant_pixels(&middle, left, split - 1);
    let (green_right, blue_right) = dominant_pixels(&middle, split + 1, right);
    assert!(
        green_left > 500 && blue_left == 0,
        "{green_left} {blue_left}"
    );
    assert!(
        blue_right > 500 && green_right == 0,
        "{green_right} {blue_right}"
    );
    assert!(top < bottom);

    // Граница идёт вправо вместе со временем, а потом слово целиком основного цвета.
    let split_at = |rasterizer: &mut SubtitleRasterizer, moment_ms: i64| {
        let overlay = render_at(rasterizer, &cues, moment_ms);
        let glyphs = of_kind(&overlay, SubtitleBitmapKind::Glyph);
        assert_eq!(glyphs.len(), 2, "mid-fill at {moment_ms} ms");
        glyphs[1].origin().x
    };
    let early = split_at(&mut rasterizer, 400);
    let late = split_at(&mut rasterizer, 1000);
    assert!(early < split && split < late, "{early} {split} {late}");

    let after = render_at(&mut rasterizer, &cues, 1300);
    assert_eq!(glyph_colors(&after), vec![GREEN], "sung: primary only");
    assert_eq!(
        rasterizer.statistics().rasterized_cues,
        1,
        "time is not part of the cache key: letters are drawn once"
    );
}

#[test]
fn the_sung_part_is_exactly_the_unsung_part_complement() {
    // Картинка до начала и две половинки внутри слога несут ту же маску: по покрытию
    // половинки в сумме дают целое слово.
    let cues = cues_of(FILL_LINE);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let whole = render_at(&mut rasterizer, &cues, 100);
    let total: u64 = of_kind(&whole, SubtitleBitmapKind::Glyph)
        .iter()
        .flat_map(|bitmap| bitmap.coverage().iter())
        .map(|value| u64::from(*value))
        .sum();
    let middle = render_at(&mut rasterizer, &cues, 700);
    let parts: u64 = of_kind(&middle, SubtitleBitmapKind::Glyph)
        .iter()
        .map(|bitmap| {
            (0..bitmap.height())
                .flat_map(|row| {
                    bitmap.coverage_row(row).expect("row")[..bitmap.width() as usize].to_vec()
                })
                .map(u64::from)
                .sum::<u64>()
        })
        .sum();
    assert_eq!(total, parts, "splitting must neither lose nor add coverage");
}

#[test]
fn k_switches_colour_at_the_syllable_start_and_ko_shows_its_outline_only_then() {
    let cues = cues_of(&format!(
        r"{{\an7\pos(100,100)\bord4\fs80{GREEN_BLUE}\ko50\ko100}}HH"
    ));
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let before = render_at(&mut rasterizer, &cues, 499);
    assert_eq!(glyph_colors(&before), vec![BLUE]);
    assert!(
        of_kind(&before, SubtitleBitmapKind::Outline).is_empty(),
        "ko hides the outline until the syllable starts"
    );
    assert_eq!(before.animation_change(), at(500));

    let started = render_at(&mut rasterizer, &cues, 500);
    assert_eq!(glyph_colors(&started), vec![GREEN]);
    assert!(!of_kind(&started, SubtitleBitmapKind::Outline).is_empty());
    assert_eq!(started.animation_change(), AnimationChange::None);
    assert_eq!(rasterizer.statistics().rasterized_cues, 1);
}

#[test]
fn plain_k_keeps_its_outline_and_changes_only_the_fill_colour() {
    let cues = cues_of(&format!(
        r"{{\an7\pos(100,100)\bord4\fs80{GREEN_BLUE}\k50\k100}}HH"
    ));
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let before = render_at(&mut rasterizer, &cues, 100);
    let after = render_at(&mut rasterizer, &cues, 600);
    assert_eq!(glyph_colors(&before), vec![BLUE]);
    assert_eq!(glyph_colors(&after), vec![GREEN]);
    assert_eq!(
        of_kind(&before, SubtitleBitmapKind::Outline).len(),
        1,
        "k never hides the outline"
    );
    assert_eq!(of_kind(&after, SubtitleBitmapKind::Outline).len(), 1);
}

#[test]
fn animation_change_follows_the_karaoke_timeline() {
    let cues = cues_of(r"{\an7\pos(100,100)\bord0\fs60\kf20\kf50}ab{\kf50}cd");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    // Слова: [200, 700] и [700, 1200].
    let change = |rasterizer: &mut SubtitleRasterizer, moment_ms: i64| {
        render_at(rasterizer, &cues, moment_ms).animation_change()
    };
    assert_eq!(
        change(&mut rasterizer, 0),
        at(200),
        "waits for the first syllable"
    );
    assert_eq!(change(&mut rasterizer, 199), at(200));
    assert_eq!(change(&mut rasterizer, 200), AnimationChange::EveryFrame);
    assert_eq!(change(&mut rasterizer, 699), AnimationChange::EveryFrame);
    assert_eq!(
        change(&mut rasterizer, 700),
        AnimationChange::EveryFrame,
        "second fills"
    );
    assert_eq!(change(&mut rasterizer, 1199), AnimationChange::EveryFrame);
    assert_eq!(
        change(&mut rasterizer, 1200),
        AnimationChange::None,
        "all sung"
    );
}

#[test]
fn between_two_plain_syllables_the_picture_waits_for_the_next_one() {
    let cues = cues_of(r"{\an7\pos(100,100)\bord0\fs60\k30}ab{\k40}cd");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    // `ab` поёт с 0, `cd` — с 300 мс.
    let first = render_at(&mut rasterizer, &cues, 10);
    assert_eq!(first.animation_change(), at(300));
    assert_eq!(
        render_at(&mut rasterizer, &cues, 300).animation_change(),
        AnimationChange::None
    );
}

#[test]
fn a_still_frame_is_reused_while_the_karaoke_says_nothing_changes() {
    let cues = cues_of(r"{\an7\pos(100,100)\bord0\fs60\k50\k50}ab");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    // Слог начинается на 500 мс: до него кадр один и тот же, после — тоже.
    let first = render_at(&mut rasterizer, &cues, 100);
    let still = render_at(&mut rasterizer, &cues, 400);
    assert_eq!(still.revision(), first.revision());
    assert_eq!(still.change(), OverlayChange::Unchanged);
    let started = render_at(&mut rasterizer, &cues, 500);
    assert_ne!(started.revision(), first.revision());
    assert_eq!(glyph_colors(&started), vec![WHITE]);
    let held = render_at(&mut rasterizer, &cues, 2000);
    assert_eq!(held.revision(), started.revision());
    assert_eq!(held.change(), OverlayChange::Unchanged);
    assert_eq!(rasterizer.statistics().rasterized_cues, 1);
}

#[test]
fn every_frame_of_a_fill_gets_its_own_picture_from_one_rasterization() {
    let cues = cues_of(FILL_LINE);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let mut splits = Vec::new();
    for moment_ms in (250..=1100).step_by(50) {
        let overlay = render_at(&mut rasterizer, &cues, moment_ms);
        if let [left, right] = of_kind(&overlay, SubtitleBitmapKind::Glyph).as_slice() {
            assert_eq!((left.color(), right.color()), (GREEN, BLUE));
            splits.push(right.origin().x);
        }
    }
    assert!(splits.len() >= 14, "{splits:?}");
    assert!(
        splits.windows(2).all(|pair| pair[0] <= pair[1]),
        "the split never moves back: {splits:?}"
    );
    assert!(splits.first() < splits.last());
    assert_eq!(rasterizer.statistics().rasterized_cues, 1);
}

#[test]
fn transparent_secondary_colour_hides_the_unsung_part() {
    let cues = cues_of(r"{\an7\pos(100,100)\bord0\fs80\2a&HFF&\kf20\kf100}HHHH");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    assert!(
        render_at(&mut rasterizer, &cues, 100).is_empty(),
        "nothing is visible before the syllable"
    );
    let middle = render_at(&mut rasterizer, &cues, 700);
    let pieces = of_kind(&middle, SubtitleBitmapKind::Glyph);
    assert_eq!(pieces.len(), 1, "only the sung part is drawn");
    assert_eq!(pieces[0].color(), WHITE);
    let (_, _, ink_right, _) = glyph_ink(&middle);
    let after = render_at(&mut rasterizer, &cues, 1300);
    let (_, _, full_right, _) = glyph_ink(&after);
    assert!(ink_right + 40 < full_right, "{ink_right} {full_right}");
}

#[test]
fn rotation_past_90_degrees_fills_right_to_left_with_swapped_colours() {
    let cues = cues_of(&format!(
        r"{{\an5\pos(640,360)\bord0\fs80\frz180{GREEN_BLUE}\kf20\kf100}}HHHH"
    ));
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let middle = render_at(&mut rasterizer, &cues, 700);
    let pieces = of_kind(&middle, SubtitleBitmapKind::Glyph);
    assert_eq!(pieces.len(), 2);
    // Слева теперь вторичный (ещё не спетый), справа основной: слово допевается справа налево.
    assert_eq!((pieces[0].color(), pieces[1].color()), (BLUE, GREEN));
    let first = render_at(&mut rasterizer, &cues, 500);
    let later = render_at(&mut rasterizer, &cues, 900);
    let boundary =
        |overlay: &SubtitleOverlay| of_kind(overlay, SubtitleBitmapKind::Glyph)[1].origin().x;
    assert!(
        boundary(&later) < boundary(&first),
        "the split moves to the left"
    );
    // Без разворота (-150) цвета остаются в обычном порядке.
    let kept = cues_of(&format!(
        r"{{\an5\pos(640,360)\bord0\fs80\frz-150{GREEN_BLUE}\kf20\kf100}}HHHH"
    ));
    let kept_middle = render_at(&mut rasterizer, &kept, 700);
    let kept_pieces = of_kind(&kept_middle, SubtitleBitmapKind::Glyph);
    assert_eq!(
        (kept_pieces[0].color(), kept_pieces[1].color()),
        (GREEN, BLUE),
        "fmod(-150, 360) is outside (90, 270)"
    );
}

#[test]
fn a_rectangular_clip_and_the_karaoke_split_apply_together() {
    let cues = cues_of(&format!(
        r"{{\an7\pos(100,100)\bord0\fs80{GREEN_BLUE}\clip(150,0,400,720)\kf20\kf100}}HHHH"
    ));
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let middle = render_at(&mut rasterizer, &cues, 700);
    let pieces = of_kind(&middle, SubtitleBitmapKind::Glyph);
    assert!(pieces.len() >= 2, "{}", pieces.len());
    for piece in &pieces {
        assert!(piece.origin().x >= 150 && piece.origin().x + piece.width() as i32 <= 400);
    }
    let colors: Vec<_> = pieces.iter().map(|piece| piece.color()).collect();
    assert!(
        colors.contains(&GREEN) && colors.contains(&BLUE),
        "{colors:?}"
    );
    assert_eq!(rasterizer.statistics().rasterized_cues, 1);
}

#[test]
fn an_inverse_clip_cuts_every_piece_around_the_hole_and_the_split_still_applies() {
    let cues = cues_of(&format!(
        r"{{\an7\pos(100,100)\bord0\fs100{GREEN_BLUE}\iclip(150,135,190,160)\kf20\kf100}}HHHH"
    ));
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let middle = render_at(&mut rasterizer, &cues, 700);
    let pieces = of_kind(&middle, SubtitleBitmapKind::Glyph);
    assert!(pieces.len() > 2, "the hole adds pieces: {}", pieces.len());
    for piece in &pieces {
        let (x0, y0) = (piece.origin().x, piece.origin().y);
        let (x1, y1) = (x0 + piece.width() as i32, y0 + piece.height() as i32);
        let inside_hole = x0 < 190 && x1 > 150 && y0 < 160 && y1 > 135;
        assert!(
            !inside_hole,
            "piece ({x0},{y0})-({x1},{y1}) overlaps the hole"
        );
    }
    let colors: Vec<_> = pieces.iter().map(|piece| piece.color()).collect();
    assert!(
        colors.contains(&GREEN) && colors.contains(&BLUE),
        "{colors:?}"
    );
}

#[test]
fn karaoke_in_a_wrapped_line_splits_each_visual_line_on_its_own_vertical() {
    // Узкие поля заставляют перенести строку; слог охватывает конец первой и начало второй.
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE],
        &[&format!(
            "Dialogue: 0,0:00:00.00,0:00:10.00,Default,,300,300,0,,{{\\an8\\bord0\\fs70{GREEN_BLUE}\\kf20\\kf200}}wide wide wide wide wide wide wide wide"
        )],
    );
    let cues = load_ass_cues(&file, fresh_source());
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let middle = render_at(&mut rasterizer, &cues, 1200);
    let colors = glyph_colors(&middle);
    assert!(
        colors.contains(&GREEN) && colors.contains(&BLUE),
        "{colors:?}"
    );
    let ys: std::collections::BTreeSet<_> = of_kind(&middle, SubtitleBitmapKind::Glyph)
        .iter()
        .map(|bitmap| bitmap.origin().y / 40)
        .collect();
    assert!(ys.len() >= 2, "the text really wrapped: {ys:?}");
}
