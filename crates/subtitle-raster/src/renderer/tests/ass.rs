//! Функциональные тесты ASS через публичный `SubtitleRasterizer::render`: файл `.ass` на входе,
//! картинки на холсте на выходе.

use std::sync::atomic::{AtomicU64, Ordering};

use subtitle_core::SubtitleColor;

use super::*;
use crate::ass::{AssRenderOptions, AssStorageSize, AssStyleSource, AssTextScale};
use crate::test_ass::{AssTestCue, DEFAULT_STYLE_LINE, ass_file, dialogue, load_ass_cues};

/// Стиль «Alt»: красный текст, контур 3.
const ALT_STYLE_LINE: &str = "Style: Alt,Noto Sans,48,&H000000FF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,3,0,2,40,40,40,1";

/// Рисует ASS-реплики на холсте с заданными настройками и стилем пользователя.
pub(super) fn render_ass_with(
    rasterizer: &mut SubtitleRasterizer,
    cues: &[AssTestCue],
    options: AssRenderOptions,
    appearance: &SubtitleAppearance,
    canvas: CanvasSize,
) -> SubtitleOverlay {
    let list: Vec<RenderableCue<'_>> = cues.iter().map(AssTestCue::renderable).collect();
    rasterizer
        .render(&RenderRequest {
            moment: subtitle_core::SubtitleTime::ZERO,
            cues: &list,
            canvas,
            appearance,
            text_language: crate::fonts::TextLanguagePreference::unknown(),
            ass: options,
        })
        .clone()
}

/// Рисует ASS-реплики стилем автора на холсте 1280x720.
pub(super) fn render_ass(
    rasterizer: &mut SubtitleRasterizer,
    cues: &[AssTestCue],
) -> SubtitleOverlay {
    render_ass_with(
        rasterizer,
        cues,
        AssRenderOptions::default(),
        &SubtitleAppearance::youtube_like(),
        canvas(),
    )
}

/// Следующий свежий идентификатор источника: ключ реплики обязан соответствовать содержимому
/// (контракт `CueKey`), поэтому разные тексты одного теста получают разные источники.
pub(super) fn fresh_source() -> u64 {
    static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);
    NEXT_SOURCE.fetch_add(1, Ordering::Relaxed)
}

/// Файл с одним событием стиля `Default` (PlayRes 1280x720) и его реплики.
pub(super) fn single(text: &str) -> Vec<AssTestCue> {
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE],
        &[&dialogue(0, text)],
    );
    load_ass_cues(&file, fresh_source())
}

/// Рамка закрашенных пикселей букв (без обводки и тени).
pub(super) fn glyph_ink(overlay: &SubtitleOverlay) -> (i32, i32, i32, i32) {
    ink_bounds(of_kind(overlay, SubtitleBitmapKind::Glyph)).expect("glyph ink")
}

fn rasterizer_for_ass() -> SubtitleRasterizer {
    rasterizer(ROOMY_LIMITS)
}

#[test]
fn main_text_stands_near_the_bottom_margin_and_is_centered() {
    let overlay = render_ass(&mut rasterizer_for_ass(), &single(r"{\bord0}HHHH"));
    let ink = glyph_ink(&overlay);
    // Поле 40 px: низ строки (с запасом на спуск шрифта) стоит на 720 - 40.
    assert!(ink.3 <= 680 && ink.3 >= 680 - 20, "ink bottom {}", ink.3);
    let center = f64::from(ink.0 + ink.2) / 2.0;
    assert!((center - 640.0).abs() <= 3.0, "text center {center}");
}

#[test]
fn pos_places_the_text_by_alignment_anchor() {
    let mut rasterizer = rasterizer_for_ass();
    let ink_of = |rasterizer: &mut SubtitleRasterizer, tags: &str| {
        let overlay = render_ass(
            rasterizer,
            &single(&format!(r"{{\bord0{tags}\pos(500,300)}}HHHH")),
        );
        glyph_ink(&overlay)
    };
    // \an7: верхний левый угол текста в точке.
    let top_left = ink_of(&mut rasterizer, r"\an7");
    assert!((500..=512).contains(&top_left.0), "an7 left {}", top_left.0);
    assert!((300..=325).contains(&top_left.1), "an7 top {}", top_left.1);
    // \an9: верхний правый угол в точке.
    let top_right = ink_of(&mut rasterizer, r"\an9");
    assert!(
        (488..=500).contains(&top_right.2),
        "an9 right {}",
        top_right.2
    );
    assert!(
        (300..=325).contains(&top_right.1),
        "an9 top {}",
        top_right.1
    );
    // \an1: нижний левый угол: низ текста над точкой на высоту спуска.
    let bottom_left = ink_of(&mut rasterizer, r"\an1");
    assert!(
        (500..=512).contains(&bottom_left.0),
        "an1 left {}",
        bottom_left.0
    );
    assert!(
        (280..=300).contains(&bottom_left.3),
        "an1 bottom {}",
        bottom_left.3
    );
    // \an5: центр блока в точке.
    let middle = ink_of(&mut rasterizer, r"\an5");
    let center_x = f64::from(middle.0 + middle.2) / 2.0;
    let center_y = f64::from(middle.1 + middle.3) / 2.0;
    assert!((center_x - 500.0).abs() <= 6.0, "an5 x {center_x}");
    assert!((center_y - 300.0).abs() <= 14.0, "an5 y {center_y}");
    // Одно и то же выравнивание, другая точка: тот же рисунок, сдвинутый ровно на разницу.
    let moved = {
        let overlay = render_ass(&mut rasterizer, &single(r"{\bord0\an7\pos(700,400)}HHHH"));
        glyph_ink(&overlay)
    };
    assert_eq!((moved.0 - top_left.0, moved.1 - top_left.1), (200, 100));
    assert_eq!(moved.2 - moved.0, top_left.2 - top_left.0);
}

#[test]
fn two_bottom_events_do_not_overlap_and_a_positioned_one_is_not_shifted() {
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE],
        &[
            &dialogue(0, r"{\bord0}First line"),
            &dialogue(0, r"{\bord0}Second line"),
            &dialogue(0, r"{\bord0\an2\pos(640,700)}Pinned"),
        ],
    );
    let cues = load_ass_cues(&file, fresh_source());
    let mut rasterizer = rasterizer_for_ass();
    let both = render_ass(&mut rasterizer, &cues[..2]);
    let glyphs = of_kind(&both, SubtitleBitmapKind::Glyph);
    assert_eq!(glyphs.len(), 2, "one glyph run per event");
    let first = ink_bounds([glyphs[0]]).expect("first");
    let second = ink_bounds([glyphs[1]]).expect("second");
    // Первая стоит внизу, вторая выше и не налезает на первую.
    assert!(
        second.3 <= first.1,
        "second {second:?} overlaps first {first:?}"
    );

    // С событием `\pos` оно не двигается: рисуется там же, где в одиночку.
    let alone = render_ass(&mut rasterizer, &cues[2..]);
    let with_others = render_ass(&mut rasterizer, &cues);
    let pinned_alone = glyph_ink(&alone);
    let pinned_ink: Vec<_> = of_kind(&with_others, SubtitleBitmapKind::Glyph)
        .into_iter()
        .map(|bitmap| ink_bounds([bitmap]).expect("ink"))
        .collect();
    assert!(
        pinned_ink.contains(&pinned_alone),
        "positioned event moved: {pinned_ink:?} vs {pinned_alone:?}"
    );
}

#[test]
fn higher_layer_is_drawn_after_lower_regardless_of_request_order() {
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE],
        &[
            &dialogue(5, r"{\bord0\an5\pos(400,300)\c&H0000FF&}Top"),
            &dialogue(1, r"{\bord0\an5\pos(400,300)\c&HFF0000&}Bottom"),
        ],
    );
    let cues = load_ass_cues(&file, fresh_source());
    let overlay = render_ass(&mut rasterizer_for_ass(), &cues);
    let colors: Vec<SubtitleColor> = of_kind(&overlay, SubtitleBitmapKind::Glyph)
        .iter()
        .map(|bitmap| bitmap.color())
        .collect();
    // \c задаёт BGR: &H0000FF& — красный, &HFF0000& — синий. Слой 1 (синий) идёт раньше слоя 5.
    assert_eq!(
        colors,
        vec![
            SubtitleColor::opaque(0, 0, 255),
            SubtitleColor::opaque(255, 0, 0)
        ]
    );
}

#[test]
fn proportional_play_res_gives_the_same_geometry_on_the_canvas() {
    // 384x288 и 1920x1080 с пропорциональными числами (кадр 1280x720: по вертикали 2,5 и 0,667).
    let small = ass_file(
        (384, 288),
        true,
        &[
            "Style: Default,Noto Sans,16,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,8,8,8,1",
        ],
        &["Dialogue: 0,0:00:00.00,0:00:10.00,Default,,0,0,0,,Proportional text"],
    );
    let large = ass_file(
        (1920, 1080),
        true,
        &[
            "Style: Default,Noto Sans,60,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,3.75,0,2,12,12,30,1",
        ],
        &["Dialogue: 0,0:00:00.00,0:00:10.00,Default,,0,0,0,,Proportional text"],
    );
    let mut rasterizer = rasterizer_for_ass();
    let from_small = render_ass(&mut rasterizer, &load_ass_cues(&small, fresh_source()));
    let from_large = render_ass(&mut rasterizer, &load_ass_cues(&large, fresh_source()));
    let (a, b) = (glyph_ink(&from_small), glyph_ink(&from_large));
    for (x, y) in [(a.0, b.0), (a.1, b.1), (a.2, b.2), (a.3, b.3)] {
        assert!((x - y).abs() <= 2, "geometry differs: {a:?} vs {b:?}");
    }
    let outline = |overlay: &SubtitleOverlay| {
        ink_bounds(of_kind(overlay, SubtitleBitmapKind::Outline)).expect("outline")
    };
    let (oa, ob) = (outline(&from_small), outline(&from_large));
    assert!(
        (oa.0 - ob.0).abs() <= 2 && (oa.3 - ob.3).abs() <= 2,
        "{oa:?} vs {ob:?}"
    );
}

#[test]
fn color_and_alpha_tags_change_layer_colors() {
    let overlay = render_ass(
        &mut rasterizer_for_ass(),
        &single(r"{\1c&H0000FF&\3c&H00FF00&\4c&HFF0000&\shad4\1a&H80&}HH"),
    );
    let color_of = |kind| of_kind(&overlay, kind)[0].color();
    // \1c задаёт красный, \1a&H80& — прозрачность 0x80, то есть непрозрачность 0x7F.
    assert_eq!(
        color_of(SubtitleBitmapKind::Glyph),
        SubtitleColor {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 0x7f
        }
    );
    assert_eq!(
        color_of(SubtitleBitmapKind::Outline),
        SubtitleColor::opaque(0, 255, 0)
    );
    assert_eq!(
        color_of(SubtitleBitmapKind::Shadow),
        SubtitleColor::opaque(0, 0, 255)
    );
}

#[test]
fn reset_tag_returns_to_the_named_style() {
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE, ALT_STYLE_LINE],
        &[&dialogue(0, r"{\bord0\c&H00FF00&}A{\rAlt}B{\r}C")],
    );
    let overlay = render_ass(
        &mut rasterizer_for_ass(),
        &load_ass_cues(&file, fresh_source()),
    );
    let colors: Vec<SubtitleColor> = of_kind(&overlay, SubtitleBitmapKind::Glyph)
        .iter()
        .map(|bitmap| bitmap.color())
        .collect();
    // A — зелёная (тег), B — красная (стиль Alt, а не зелёная), C — белая (стиль события).
    // У Alt контур 3 из стиля, но \bord0 был до сброса: контур вернулся (виден как отдельный слой).
    assert_eq!(
        colors,
        vec![
            SubtitleColor::opaque(0, 255, 0),
            SubtitleColor::opaque(255, 0, 0),
            SubtitleColor::opaque(255, 255, 255)
        ]
    );
    assert!(
        !of_kind(&overlay, SubtitleBitmapKind::Outline).is_empty(),
        "reset restores the style outline"
    );
}

#[test]
fn soft_break_breaks_only_in_no_wrap_mode() {
    let mut rasterizer = rasterizer_for_ass();
    let one_line = glyph_ink(&render_ass(
        &mut rasterizer,
        &single(r"{\bord0}Hello World"),
    ));
    let spaced = glyph_ink(&render_ass(
        &mut rasterizer,
        &single(r"{\bord0}Hello\nWorld"),
    ));
    let broken = glyph_ink(&render_ass(
        &mut rasterizer,
        &single(r"{\bord0\q2}Hello\nWorld"),
    ));
    let height = |ink: (i32, i32, i32, i32)| ink.3 - ink.1;
    // `\n` в обычном режиме — пробел: одна строка той же высоты.
    assert!(
        (height(spaced) - height(one_line)).abs() <= 1,
        "{spaced:?} {one_line:?}"
    );
    // В режиме `\q2` — перевод строки: две строки.
    assert!(
        height(broken) > height(one_line) * 3 / 2,
        "{broken:?} {one_line:?}"
    );
    // `\N` переводит строку в любом режиме.
    let hard = glyph_ink(&render_ass(
        &mut rasterizer,
        &single(r"{\bord0}Hello\NWorld"),
    ));
    assert!(height(hard) > height(one_line) * 3 / 2);
}

#[test]
fn unknown_tag_is_ignored_and_the_text_is_still_drawn() {
    let mut rasterizer = rasterizer_for_ass();
    let plain = render_ass(&mut rasterizer, &single("Hello"));
    let unknown = render_ass(&mut rasterizer, &single(r"{\zzz5\qqq(1,2)}Hello"));
    let ink = ink_bounds(unknown.bitmaps());
    assert!(ink.is_some(), "text must be drawn despite the unknown tag");
    assert_eq!(ink, ink_bounds(plain.bitmaps()));
}

#[test]
fn user_style_replaces_author_look_but_keeps_author_position() {
    let author_look = r"{\bord0\an7\pos(200,150)\c&H0000FF&\fs80}HHHH";
    let cues = single(author_look);
    let mut rasterizer = rasterizer_for_ass();
    let appearance = SubtitleAppearance::outline_and_shadow();
    let user_options = AssRenderOptions {
        style_source: AssStyleSource::User,
        ..AssRenderOptions::default()
    };
    let authored = render_ass(&mut rasterizer, &cues);
    let forced = render_ass_with(&mut rasterizer, &cues, user_options, &appearance, canvas());

    // Цвет и размер — пользовательские: белый текст, обводка пользовательского стиля и мельче
    // авторских 80.
    let glyph = of_kind(&forced, SubtitleBitmapKind::Glyph)[0];
    assert_eq!(glyph.color(), appearance.text_color);
    assert!(!of_kind(&forced, SubtitleBitmapKind::Outline).is_empty());
    let (author_ink, forced_ink) = (glyph_ink(&authored), glyph_ink(&forced));
    assert!(forced_ink.3 - forced_ink.1 < author_ink.3 - author_ink.1);
    // Положение — авторское: левый верх у точки `\pos(200,150)`.
    assert!((200..=212).contains(&forced_ink.0), "left {}", forced_ink.0);
    assert!((150..=175).contains(&forced_ink.1), "top {}", forced_ink.1);
    // У авторского размера 80 боковой зазор буквы больше, но обе версии стоят у той же точки.
    assert!(
        (200..=212).contains(&author_ink.0),
        "author left {}",
        author_ink.0
    );
}

#[test]
fn user_style_with_a_plate_draws_the_plate_under_ass_text() {
    let cues = single(r"{\an7\pos(200,150)}Plate");
    let options = AssRenderOptions {
        style_source: AssStyleSource::User,
        ..AssRenderOptions::default()
    };
    let overlay = render_ass_with(
        &mut rasterizer_for_ass(),
        &cues,
        options,
        &SubtitleAppearance::youtube_like(),
        canvas(),
    );
    assert_eq!(overlay.bitmaps()[0].kind(), SubtitleBitmapKind::Background);
    assert!(of_kind(&overlay, SubtitleBitmapKind::Outline).is_empty());
}

#[test]
fn text_scale_enlarges_main_events_but_not_positioned_ones() {
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE],
        &[
            &dialogue(0, r"{\bord0}Main text"),
            &dialogue(0, r"{\bord0\an7\pos(100,100)}Pinned text"),
        ],
    );
    let cues = load_ass_cues(&file, fresh_source());
    let mut rasterizer = rasterizer_for_ass();
    let scaled_options = AssRenderOptions {
        text_scale: AssTextScale::new(1.5).expect("scale"),
        ..AssRenderOptions::default()
    };
    let appearance = SubtitleAppearance::youtube_like();
    let normal = render_ass(&mut rasterizer, &cues);
    let scaled = render_ass_with(
        &mut rasterizer,
        &cues,
        scaled_options,
        &appearance,
        canvas(),
    );
    let heights = |overlay: &SubtitleOverlay| -> Vec<i32> {
        of_kind(overlay, SubtitleBitmapKind::Glyph)
            .into_iter()
            .map(|bitmap| {
                let ink = ink_bounds([bitmap]).expect("ink");
                ink.3 - ink.1
            })
            .collect()
    };
    let (before, after) = (heights(&normal), heights(&scaled));
    assert_eq!(before.len(), 2);
    // Главная реплика (первая) выросла примерно в 1,5 раза, `\pos`-реплика не изменилась.
    let growth = f64::from(after[0]) / f64::from(before[0]);
    assert!((growth - 1.5).abs() < 0.06, "main growth {growth}");
    assert_eq!(after[1], before[1]);
}

#[test]
fn storage_size_changes_border_scale_only_when_scaled_border_is_off() {
    let off = ass_file(
        (384, 288),
        false,
        &[
            "Style: Default,Noto Sans,16,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,8,8,8,1",
        ],
        &["Dialogue: 0,0:00:00.00,0:00:10.00,Default,,0,0,0,,Border"],
    );
    let cues = load_ass_cues(&off, fresh_source());
    let mut rasterizer = rasterizer_for_ass();
    let appearance = SubtitleAppearance::youtube_like();
    let mut outline_width = |storage| {
        let options = AssRenderOptions {
            storage_size: storage,
            ..AssRenderOptions::default()
        };
        let overlay = render_ass_with(&mut rasterizer, &cues, options, &appearance, canvas());
        let outline = ink_bounds(of_kind(&overlay, SubtitleBitmapKind::Outline)).expect("outline");
        let glyph = glyph_ink(&overlay);
        glyph.0 - outline.0
    };
    // Без размера хранения обводка растёт как PlayRes (x2,5 = 5 px); при хранении 640x360
    // (масштаб кадра 2) — 4 px.
    let unknown = outline_width(AssStorageSize::Unknown);
    let known = outline_width(AssStorageSize::from_pixels(640, 360));
    assert!(unknown > known, "unknown {unknown}, known {known}");
}

#[test]
fn user_plate_follows_the_stretched_text_when_the_pixel_aspect_differs() {
    let cues = single(r"{\an7\pos(200,150)}Plate");
    let options = AssRenderOptions {
        style_source: AssStyleSource::User,
        // Видео 4:3 на кадре 16:9: текст растягивается по горизонтали на 4/3.
        storage_size: AssStorageSize::from_pixels(1024, 768),
        ..AssRenderOptions::default()
    };
    let overlay = render_ass_with(
        &mut rasterizer_for_ass(),
        &cues,
        options,
        &SubtitleAppearance::youtube_like(),
        canvas(),
    );
    let plate = ink_bounds(of_kind(&overlay, SubtitleBitmapKind::Background)).expect("plate");
    let glyphs = glyph_ink(&overlay);
    // Отступ плашки 10 px с каждой стороны от букв — и при растянутом тексте.
    assert!(
        (glyphs.0 - plate.0 - 10).abs() <= 2,
        "left {glyphs:?} {plate:?}"
    );
    assert!(
        (plate.2 - glyphs.2 - 10).abs() <= 2,
        "right {glyphs:?} {plate:?}"
    );
}

/// Стиль `Zero`: размер шрифта 0. Известное отличие от libass: libass рисует такие буквы
/// размером, который выбирает FreeType при нулевом запросе (em около unitsPerEm/64 px), а
/// растеризатор их не рисует. Рисунки `\p` в этом стиле рисуются как в libass (решение
/// владельца 17.10.2026).
const ZERO_SIZE_STYLE_LINE: &str = "Style: Zero,Noto Sans,0,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,0,0,2,40,40,40,1";

fn zero_style_overlay(text: &str) -> SubtitleOverlay {
    let line = format!("Dialogue: 0,0:00:00.00,0:00:10.00,Zero,,0,0,0,,{text}");
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE, ZERO_SIZE_STYLE_LINE],
        &[&line],
    );
    render_ass(
        &mut rasterizer_for_ass(),
        &load_ass_cues(&file, fresh_source()),
    )
}

#[test]
fn zero_font_size_runs_draw_nothing_but_the_rest_of_the_cue_still_renders() {
    // «AAA» в нулевом размере не рисуется, `\fs48` возвращает буквы.
    let mixed = zero_style_overlay(r"AAA{\fs48}HHHH");
    let only_sized = zero_style_overlay(r"{\fs48}HHHH");
    assert!(
        !of_kind(&mixed, SubtitleBitmapKind::Glyph).is_empty(),
        "остальной текст реплики рисуется"
    );
    assert_eq!(glyph_ink(&mixed), glyph_ink(&only_sized));
}

#[test]
fn a_cue_made_only_of_zero_size_text_renders_nothing_without_failing() {
    let overlay = zero_style_overlay("AAA");
    assert!(of_kind(&overlay, SubtitleBitmapKind::Glyph).is_empty());
}

#[test]
fn drawing_in_a_zero_font_size_style_is_drawn_while_letters_next_to_it_are_not() {
    // Квадрат 100x100 PlayRes (экран 1280x720 = PlayRes 1280x720, масштаб 1) в нулевом
    // размере шрифта: рисунок рисуется, «AAA» после него — нет.
    let overlay = zero_style_overlay(r"{\an7\pos(100,100)\p1}m 0 0 l 100 0 100 100 0 100{\p0}AAA");
    let drawn = of_kind(&overlay, SubtitleBitmapKind::Glyph);
    assert_eq!(drawn.len(), 1, "только рисунок, без букв");
    let ink = glyph_ink(&overlay);
    // Край на пиксель мягче из-за квантования матрицы растеризатора libass (см. ass_drawing).
    assert_eq!(ink, (99, 99, 201, 201));
}
