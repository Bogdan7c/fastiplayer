//! Статическая геометрия слоя 2 через публичный `SubtitleRasterizer::render`: повороты, наклон,
//! `\org`. Файл `.ass` на входе, картинки на холсте на выходе; проверяется то, что видно на
//! экране (где стоят чернила и какой они формы), а не промежуточные матрицы.

use super::ass::{fresh_source, glyph_ink, render_ass, single};
use super::*;
use crate::overlay::OverlayPlacementGroup;
use crate::test_ass::{DEFAULT_STYLE_LINE, ass_file, dialogue, load_ass_cues};

/// Ширина и высота рамки чернил.
fn ink_size(ink: (i32, i32, i32, i32)) -> (i32, i32) {
    (ink.2 - ink.0, ink.3 - ink.1)
}

/// Центр рамки чернил.
fn ink_center(ink: (i32, i32, i32, i32)) -> (f64, f64) {
    (
        f64::from(ink.0 + ink.2) / 2.0,
        f64::from(ink.1 + ink.3) / 2.0,
    )
}

/// Чернила одной реплики с тегами `tags`, расположенной `\an5\pos(640,360)`, без обводки.
fn ink_with(tags: &str, text: &str) -> (i32, i32, i32, i32) {
    let overlay = render_ass(
        &mut rasterizer(ROOMY_LIMITS),
        &single(&format!(r"{{\an5\pos(640,360)\bord0{tags}}}{text}")),
    );
    glyph_ink(&overlay)
}

#[test]
fn frz_90_turns_a_wide_word_on_its_side_around_the_anchor() {
    let flat = ink_with("", "HHHHHH");
    let (flat_width, flat_height) = ink_size(flat);
    assert!(flat_width > 2 * flat_height, "{flat:?}");

    let turned = ink_with(r"\frz90", "HHHHHH");
    let (turned_width, turned_height) = ink_size(turned);
    // Ширина и высота поменялись местами (с запасом на округление краёв).
    assert!((turned_width - flat_height).abs() <= 3, "{turned:?}");
    assert!((turned_height - flat_width).abs() <= 3, "{turned:?}");
    // Центр слова остался в опорной точке \an5\pos.
    let (cx, cy) = ink_center(turned);
    let (flat_cx, flat_cy) = ink_center(flat);
    assert!(
        (cx - flat_cx).abs() <= 3.0 && (cy - flat_cy).abs() <= 3.0,
        "{cx},{cy}"
    );
}

#[test]
fn frz_180_mirrors_the_picture_through_the_anchor_point() {
    // Буква с хвостом: чернила несимметричны относительно якоря, поэтому после поворота на
    // 180 градусов центр чернил отражается через точку \pos, а размер остаётся прежним.
    let flat = ink_with("", "Lg");
    let turned = ink_with(r"\frz180", "Lg");
    assert_eq!(ink_size(turned), ink_size(flat));
    let ((cx, cy), (flat_cx, flat_cy)) = (ink_center(turned), ink_center(flat));
    assert!((cx + flat_cx - 1280.0).abs() <= 3.0, "x: {cx} + {flat_cx}");
    assert!((cy + flat_cy - 720.0).abs() <= 3.0, "y: {cy} + {flat_cy}");
}

#[test]
fn org_moves_the_centre_of_rotation() {
    // Поворот на 180 градусов вокруг точки (840, 360): центр слова отражается через неё
    // (был 640, станет 1040), высота остаётся на месте.
    let ink = ink_with(r"\org(840,360)\frz180", "HHHH");
    let (cx, cy) = ink_center(ink);
    assert!((cx - 1040.0).abs() <= 3.0, "centre x {cx}");
    assert!((cy - 360.0).abs() <= 3.0, "centre y {cy}");
}

#[test]
fn the_first_org_wins() {
    let first_only = ink_with(r"\org(840,360)\frz180", "HHHH");
    let both = ink_with(r"\org(840,360)\org(100,100)\frz180", "HHHH");
    assert_eq!(first_only, both);
}

#[test]
fn frx_and_fry_shrink_the_far_side_in_perspective() {
    let flat = ink_with("", "HHHHHH");
    // Поворот вокруг вертикальной оси сужает слово по ширине (cos 60 = 0.5) и не меняет центр.
    let narrowed = ink_with(r"\fry60", "HHHHHH");
    assert!(
        ink_size(narrowed).0 < ink_size(flat).0 * 6 / 10,
        "{narrowed:?}"
    );
    assert!(
        ink_size(narrowed).0 > ink_size(flat).0 * 4 / 10,
        "{narrowed:?}"
    );
    // Поворот вокруг горизонтальной оси сплющивает по высоте.
    let squashed = ink_with(r"\frx60", "HHHHHH");
    assert!(
        ink_size(squashed).1 < ink_size(flat).1 * 6 / 10,
        "{squashed:?}"
    );
}

#[test]
fn fax_slants_letters_and_widens_the_word_by_fax_times_height() {
    let flat = ink_with("", "HHHH");
    let slanted = ink_with(r"\fax0.5", "HHHH");
    let height = ink_size(flat).1;
    let widening = ink_size(slanted).0 - ink_size(flat).0;
    // Верх сдвигается на fax * высоту относительно базовой линии.
    assert!(
        (f64::from(widening) - 0.5 * f64::from(height)).abs() <= 4.0,
        "widening {widening}, height {height}"
    );
    assert_eq!(ink_size(slanted).1, height, "высота не меняется");
}

#[test]
fn fay_lifts_later_letters_so_the_baseline_runs_uphill() {
    let flat = ink_with(r"\bord0", "HHHHHH");
    let sheared = ink_with(r"\fay-0.3", "HHHHHH");
    let (flat_width, flat_height) = ink_size(flat);
    // Базовая линия поднимается на 0.3 ширины слова: высота рамки растёт примерно на это.
    let growth = ink_size(sheared).1 - flat_height;
    assert!(
        (f64::from(growth) - 0.3 * f64::from(flat_width)).abs() <= 0.08 * f64::from(flat_width),
        "growth {growth}, width {flat_width}"
    );
}

#[test]
fn glyphs_behind_the_camera_vanish_without_a_panic() {
    // Слово далеко от центра поворота: при \frx80 оно оказывается за камерой.
    let near = single(r"{\an5\pos(640,150)\org(640,40)\frx80\bord0}Near");
    let far = single(r"{\an5\pos(640,560)\org(640,40)\frx80\bord0}Far");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    assert!(!render_ass(&mut rasterizer, &near).is_empty());
    let overlay = render_ass(&mut rasterizer, &far);
    assert!(overlay.is_empty(), "дальняя надпись за камерой не рисуется");
    assert_eq!(rasterizer.statistics().failed_cues, 0, "это не сбой");
}

#[test]
fn style_angle_rotates_without_tags_and_frz_without_argument_returns_to_it() {
    const ANGLE_STYLE: &str = "Style: Default,Noto Sans,48,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,90,1,0,0,2,40,40,40,1";
    let file = ass_file(
        (1280, 720),
        true,
        &[ANGLE_STYLE],
        &[
            &dialogue(0, r"{\an5\pos(640,360)}HHHHHH"),
            &dialogue(1, r"{\an5\pos(640,360)\frz0}HHHHHH"),
            &dialogue(2, r"{\an5\pos(640,360)\frz0\frz}HHHHHH"),
        ],
    );
    let cues = load_ass_cues(&file, fresh_source());
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let ink_of = |index: usize, rasterizer: &mut SubtitleRasterizer| {
        glyph_ink(&render_ass(rasterizer, &cues[index..=index]))
    };
    let styled = ink_size(ink_of(0, &mut rasterizer));
    let overridden = ink_size(ink_of(1, &mut rasterizer));
    let restored = ink_size(ink_of(2, &mut rasterizer));
    assert!(styled.1 > styled.0, "стиль с Angle=90 кладёт слово набок");
    assert!(overridden.0 > overridden.1, "\\frz0 выпрямляет слово");
    assert_eq!(
        restored, styled,
        "\\frz без аргумента возвращает Angle стиля"
    );
}

#[test]
fn rotated_event_with_org_does_not_collide_and_is_a_fixed_caption() {
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE],
        &[
            &dialogue(0, r"{\an2\org(640,700)\frz4\bord0}One"),
            &dialogue(0, r"{\an2\org(640,700)\frz4\bord0}One"),
        ],
    );
    let cues = load_ass_cues(&file, fresh_source());
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let both = render_ass(&mut rasterizer, &cues);
    let single_event = render_ass(&mut rasterizer, &cues[..1]);
    // Без поиска свободного места второй экземпляр стоит ровно поверх первого.
    assert_eq!(glyph_ink(&both), glyph_ink(&single_event));
    assert!(
        both.bitmaps()
            .iter()
            .all(|bitmap| bitmap.placement_group() == OverlayPlacementGroup::Fixed)
    );
}

#[test]
fn rotated_event_without_org_still_collides_like_libass() {
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE],
        &[
            &dialogue(0, r"{\frz4\bord0}One"),
            &dialogue(0, r"{\frz4\bord0}One"),
        ],
    );
    let cues = load_ass_cues(&file, fresh_source());
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let both = render_ass(&mut rasterizer, &cues);
    let single_event = render_ass(&mut rasterizer, &cues[..1]);
    // Без \org и \pos коллизии работают: второй экземпляр поднят над первым.
    assert!(glyph_ink(&both).1 < glyph_ink(&single_event).1);
}

#[test]
fn static_rotation_is_rasterised_once_and_then_served_from_the_cache() {
    let cues = single(r"{\an5\pos(640,360)\frx20\fry-20\frz30\fax0.2\org(700,300)}Cached");
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let first = render_ass(&mut rasterizer, &cues);
    assert!(!first.is_empty());
    let rasterized = rasterizer.statistics().rasterized_cues;
    let second = render_ass(&mut rasterizer, &cues);
    assert_eq!(second.change(), OverlayChange::Unchanged);
    assert_eq!(second.revision(), first.revision());
    assert_eq!(rasterizer.statistics().rasterized_cues, rasterized);
}

#[test]
fn box_border_style_rotates_with_the_text() {
    const BOX_STYLE: &str = "Style: Default,Noto Sans,48,&H00FFFFFF,&H000000FF,&H00804000,&H00000000,0,0,0,0,100,100,0,0,3,6,0,2,40,40,40,1";
    let file = ass_file(
        (1280, 720),
        true,
        &[BOX_STYLE],
        &[&dialogue(0, r"{\an5\pos(640,360)\frz90}BoxBoxBox")],
    );
    let cues = load_ass_cues(&file, fresh_source());
    let overlay = render_ass(&mut rasterizer(ROOMY_LIMITS), &cues);
    let boxes = of_kind(&overlay, SubtitleBitmapKind::Outline);
    let (width, height) = ink_size(ink_bounds(boxes).expect("box ink"));
    assert!(
        height > width,
        "плашка повернута вместе с текстом: {width}x{height}"
    );
}

/// Рисует реплику с тегами `tags` и возвращает результат, счётчик пропусков и время работы.
fn render_adversarial(tags: &str) -> (SubtitleOverlay, u64, std::time::Duration) {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let cues = single(&format!(
        r"{{\an5\pos(640,360){tags}}}HHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHH"
    ));
    let started = std::time::Instant::now();
    let overlay = render_ass(&mut rasterizer, &cues);
    let skipped = rasterizer.statistics().skipped_oversized_bitmaps;
    (overlay, skipped, started.elapsed())
}

#[test]
fn perspective_blow_up_near_the_camera_plane_is_skipped_quickly_and_counted() {
    // Огромное растяжение + поворот к плоскости камеры раздувают контур глифов до десятков
    // тысяч пикселей. Раньше такой битмап выделялся целиком (сотни мегабайт, секунды работы);
    // теперь глифы шире `max_canvas_side` пропускаются и считаются, реплика не падает.
    let (overlay, skipped, elapsed) = render_adversarial(r"\frx89.9\fscx2000\fscy2000\bord5");
    assert!(skipped > 0, "пропуск должен быть засчитан");
    // Оставшиеся картинки обязаны лежать внутри холста (инвариант выхода).
    assert!(
        overlay
            .bitmaps()
            .iter()
            .all(|bitmap| bitmap.width() <= WIDTH)
    );
    assert!(elapsed.as_secs() < 5, "{elapsed:?}");
}

#[test]
fn ordinary_three_d_text_is_not_counted_as_oversized() {
    let (normal, skipped, _) = render_adversarial(r"\frx45\fry20\bord3");
    assert!(!normal.is_empty());
    assert_eq!(skipped, 0);
}

#[test]
fn bitmap_side_limit_follows_the_configured_canvas_limit() {
    // Тот же текст: при щедром пределе рисуется, при тесном (меньше самого глифа) — пропуск.
    let cues = single(r"{\an5\pos(640,360)\bord0\frx30\fscx9000\fscy300}HHHH");
    let draw = |max_canvas_side: u32| {
        let mut rasterizer = rasterizer_with_limits(RasterLimits {
            cache: ROOMY_LIMITS,
            input: RasterInputLimits {
                max_canvas_side,
                ..ROOMY_INPUT
            },
        });
        let overlay = render_ass(&mut rasterizer, &cues);
        (overlay, rasterizer.statistics().skipped_oversized_bitmaps)
    };
    let (generous, generous_skipped) = draw(16_384);
    assert!(!generous.is_empty());
    assert_eq!(generous_skipped, 0);
    let (tight, tight_skipped) = draw(1_280);
    assert!(tight_skipped > 0);
    assert!(tight.is_empty());
}

/// Чернила одного слова с `\fry` в режиме «мой стиль» при заданном PlayRes (холст всегда
/// 1280x720: PlayRes 640x360 даёт `blur_scale_y = 2`, то есть камеру вдвое дальше).
fn user_style_rotated_ink(play_res: (u32, u32), pos: (u32, u32)) -> (i32, i32, i32, i32) {
    let text = format!(r"{{\an5\pos({},{})\bord0\fry75}}HHHHHH", pos.0, pos.1);
    let file = crate::test_ass::ass_file(
        play_res,
        true,
        &[crate::test_ass::DEFAULT_STYLE_LINE],
        &[&crate::test_ass::dialogue(0, &text)],
    );
    let cues = crate::test_ass::load_ass_cues(&file, fresh_source());
    let options = crate::ass::AssRenderOptions {
        style_source: crate::ass::AssStyleSource::User,
        ..crate::ass::AssRenderOptions::default()
    };
    let overlay = super::ass::render_ass_with(
        &mut rasterizer(ROOMY_LIMITS),
        &cues,
        options,
        &SubtitleAppearance::youtube_like(),
        canvas(),
    );
    glyph_ink(&overlay)
}

#[test]
fn camera_distance_follows_the_frame_scale_in_user_style_mode_too() {
    // libass: `dist = 20000 * blur_scale_y` и при подмене стиля пользователем. Слово одного
    // и того же размера в пикселях (стиль пользователя) и в той же точке холста, но кадр
    // сценария вдвое мельче: камера вдвое дальше, перспектива слабее, чернила другие. Если
    // камера привязана к авторскому размытию (в этом режиме обнулённому), обе картинки
    // совпадут.
    let near_camera = user_style_rotated_ink((1280, 720), (640, 360));
    let far_camera = user_style_rotated_ink((640, 360), (320, 180));
    assert_ne!(near_camera, far_camera);
}
