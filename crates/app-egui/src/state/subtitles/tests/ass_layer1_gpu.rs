//! ASS слоя 1 до пикселей: настоящий MKV с ASS-дорожкой -> demux -> feed -> владелец ->
//! растеризатор -> размещение -> настоящий GPU-проход -> readback. Тесты пропускаются (с
//! сообщением), если на машине нет GPU adapter-а.

use super::ass_gpu_support::*;
use super::gpu_chain::Harness;
use super::*;

macro_rules! gpu_or_skip {
    () => {
        match Harness::with_size(WINDOW_WIDTH, WINDOW_HEIGHT) {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the ASS layer-1 test");
                return;
            }
        }
    };
}

/// PlayRes фикстуры.
const PLAY_RES_WIDTH: f32 = 640.0;
const PLAY_RES_HEIGHT: f32 = 360.0;

/// Допуск на геометрию знака в пикселях окна: центр красных пикселей ≈ точка `\pos` + смещение
/// строки текста относительно центра `\an5`.
const SIGN_TOLERANCE_PX: f32 = 6.0;

#[test]
fn sign_and_stacked_bottom_lines_reach_the_gpu_frame_at_the_scaled_places() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let mut owner = layer1_owner(&enabled_config(), media);
    let rect = full_window_picture();
    let (_, video_only) = harness.frame(rect, None);

    let pixels = render_placed(&mut owner, &mut harness, media, rect, INSIDE_EVENTS_MILLIS);

    // Знак: центр красных пикселей около точки \pos(160,60), масштабированной в картинку.
    let sign = bounds_of(&pixels, WINDOW_WIDTH, is_red).expect("red sign pixels reach the frame");
    let expected_x = rect.x + 160.0 / PLAY_RES_WIDTH * rect.width;
    let expected_y = rect.y + 60.0 / PLAY_RES_HEIGHT * rect.height;
    assert!(
        (sign.center_x() - expected_x).abs() <= SIGN_TOLERANCE_PX,
        "sign x {} vs expected {expected_x}",
        sign.center_x()
    );
    assert!(
        (sign.center_y() - expected_y).abs() <= SIGN_TOLERANCE_PX,
        "sign y {} vs expected {expected_y}",
        sign.center_y()
    );
    // Две нижние реплики видны, лежат в нижней половине и не перекрываются по строкам.
    let first = bounds_of(&pixels, WINDOW_WIDTH, is_white).expect("first bottom line");
    let second = bounds_of(&pixels, WINDOW_WIDTH, is_yellow).expect("second bottom line");
    let picture_middle = rect.y + rect.height / 2.0;
    for (name, line) in [("first", first), ("second", second)] {
        assert!(
            line.min_y as f32 > picture_middle,
            "{name} line is in the lower half: {line:?}"
        );
        assert!(
            (line.center_x() - (rect.x + rect.width / 2.0)).abs() <= 4.0,
            "{name} line is centred horizontally: {line:?}"
        );
    }
    let disjoint = first.max_y < second.min_y || second.max_y < first.min_y;
    assert!(
        disjoint,
        "simultaneous bottom lines must stack, not overlap: {first:?} vs {second:?}"
    );
    // Вне окна реплик кадр чистый.
    let after = render_placed(&mut owner, &mut harness, media, rect, OUTSIDE_EVENTS_MILLIS);
    assert_eq!(after, video_only);
}

#[test]
fn nothing_is_drawn_on_the_black_bars_of_a_letterboxed_picture() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let mut owner = layer1_owner(&enabled_config(), media);
    let rect = letterboxed_window_picture();
    let (_, video_only) = harness.frame(rect, None);

    let pixels = render_placed(&mut owner, &mut harness, media, rect, INSIDE_EVENTS_MILLIS);

    assert_ne!(pixels, video_only, "the subtitles are drawn");
    let picture_top = rect.y.round() as u32;
    let picture_bottom = (rect.y + rect.height).round() as u32;
    assert!(picture_top > 5, "the test needs real bars");
    for y in (0..picture_top).chain(picture_bottom..WINDOW_HEIGHT) {
        assert!(
            !row_has_non_black(&pixels, y),
            "row {y} is a bar and must stay black"
        );
    }
    // Положение знака считается от КАРТИНКИ, а не от окна.
    let sign = bounds_of(&pixels, WINDOW_WIDTH, is_red).expect("sign");
    let expected_y = rect.y + 60.0 / PLAY_RES_HEIGHT * rect.height;
    assert!((sign.center_y() - expected_y).abs() <= SIGN_TOLERANCE_PX);
}

#[test]
fn author_colours_are_kept_and_text_size_scales_dialogue_but_not_the_positioned_sign() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let rect = full_window_picture();
    let mut owner = layer1_owner(&enabled_config(), media);
    let normal = render_placed(&mut owner, &mut harness, media, rect, INSIDE_EVENTS_MILLIS);
    assert!(count_of(&normal, is_red) > 50, "author's red sign colour");
    assert!(count_of(&normal, is_white) > 50, "author's white dialogue");
    assert!(
        count_of(&normal, is_yellow) > 50,
        "author's yellow dialogue"
    );
    assert_eq!(
        count_of(&normal, is_green),
        0,
        "the user colour is not applied"
    );

    owner.apply_config(&SubtitlesConfig {
        text_size_percent: 150,
        ..enabled_config()
    });
    let big = render_placed(&mut owner, &mut harness, media, rect, INSIDE_EVENTS_MILLIS);

    let normal_line = bounds_of(&normal, WINDOW_WIDTH, is_white).expect("line");
    let big_line = bounds_of(&big, WINDOW_WIDTH, is_white).expect("scaled line");
    assert!(
        big_line.height() * 10 >= normal_line.height() * 13,
        "150 % text is taller: {} vs {}",
        big_line.height(),
        normal_line.height()
    );
    assert_eq!(
        bounds_of(&big, WINDOW_WIDTH, is_red),
        bounds_of(&normal, WINDOW_WIDTH, is_red),
        "the \\pos sign keeps its size and place"
    );
    assert_eq!(
        count_of(&big, is_red),
        count_of(&normal, is_red),
        "the sign is pixel-identical in size"
    );
}

#[test]
fn force_user_style_paints_with_the_user_colour_and_keeps_the_sign_at_its_pos() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let rect = full_window_picture();
    let mut owner = layer1_owner(&enabled_config(), media);
    let author = render_placed(&mut owner, &mut harness, media, rect, INSIDE_EVENTS_MILLIS);
    let author_sign = bounds_of(&author, WINDOW_WIDTH, is_red).expect("author sign");

    owner.apply_config(&SubtitlesConfig {
        ass_force_user_style: true,
        text_color: "#00FF00".to_owned(),
        ..enabled_config()
    });
    let forced = render_placed(&mut owner, &mut harness, media, rect, INSIDE_EVENTS_MILLIS);

    assert_eq!(count_of(&forced, is_red), 0, "author's red is replaced");
    assert_eq!(count_of(&forced, is_white), 0, "author's white is replaced");
    assert_eq!(
        count_of(&forced, is_yellow),
        0,
        "author's yellow is replaced"
    );
    assert!(
        count_of(&forced, is_green) > 100,
        "user colour paints the text"
    );
    // Зелёные пиксели знака стоят на месте красных: \pos сохранён. Знак - самое верхнее зелёное.
    let top_rows_green = bounds_of(
        &forced[..(WINDOW_WIDTH * 4 * 100) as usize],
        WINDOW_WIDTH,
        is_green,
    )
    .expect("green sign in the top part of the picture");
    assert!(
        (top_rows_green.center_x() - author_sign.center_x()).abs() <= 3.0
            && (top_rows_green.center_y() - author_sign.center_y()).abs() <= 3.0,
        "forced sign {top_rows_green:?} vs author sign {author_sign:?}"
    );
}

/// Сценарий с одним красным знаком; все размеры - доли PlayRes, поэтому при любом PlayRes
/// геометрия в кадре должна совпасть.
fn proportional_script(play_res_width: u32, play_res_height: u32) -> String {
    let scale = |fraction: f64, base: u32| fraction * f64::from(base);
    format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {play_res_width}\nPlayResY: {play_res_height}\n\n\
[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
Style: Default,Arial,{size},&H000000FF,&H000000FF,&H00000000,&H00000000,1,0,0,0,100,100,0,0,1,0,0,5,0,0,0,1\n\n\
[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,{{\\pos({x},{y})}}SIGN\n",
        size = scale(0.1, play_res_height),
        x = scale(0.25, play_res_width),
        y = scale(0.25, play_res_height),
    )
}

#[test]
fn play_res_does_not_change_the_relative_geometry_in_the_frame() {
    let mut harness = gpu_or_skip!();
    let rect = full_window_picture();
    let mut bounds_by_script = Vec::new();
    for (play_res_width, play_res_height) in [(384, 288), (1920, 1080)] {
        let script = proportional_script(play_res_width, play_res_height);
        let (_dir, video) = super::track_selection::movie_with(&[("Movie.ass", &script)]);
        let mut owner = owner_with_config(&enabled_config());
        begin_and_wait(&mut owner, instance(1), &video);
        super::track_selection::settle(&mut owner);
        let pixels = render_placed(
            &mut owner,
            &mut harness,
            instance(1),
            rect,
            INSIDE_EVENTS_MILLIS,
        );
        bounds_by_script.push(
            bounds_of(&pixels, WINDOW_WIDTH, is_red)
                .unwrap_or_else(|| panic!("sign for PlayRes {play_res_width}x{play_res_height}")),
        );
    }
    let (small, large) = (bounds_by_script[0], bounds_by_script[1]);
    for (name, left, right) in [
        ("min_x", small.min_x, large.min_x),
        ("min_y", small.min_y, large.min_y),
        ("max_x", small.max_x, large.max_x),
        ("max_y", small.max_y, large.max_y),
    ] {
        assert!(
            left.abs_diff(right) <= 2,
            "{name}: {left} (384x288) vs {right} (1920x1080); {small:?} vs {large:?}"
        );
    }
}
