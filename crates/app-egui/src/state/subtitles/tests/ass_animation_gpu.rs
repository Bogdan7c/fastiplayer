//! Анимация ASS до пикселей: сценарий рядом с «видео» -> настоящий поток-рисовальщик ->
//! размещение -> настоящий GPU-проход -> readback. Тесты пропускаются (с сообщением), если на
//! машине нет GPU adapter-а.

use super::ass_animation_support::*;
use super::ass_gpu_support::*;
use super::gpu_chain::Harness;
use super::*;

macro_rules! gpu_or_skip {
    () => {
        match Harness::with_size(WINDOW_WIDTH, WINDOW_HEIGHT) {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the ASS animation test");
                return;
            }
        }
    };
}

/// PlayRes сценариев этих тестов равен окну: координаты тегов = пиксели кадра.
const PLAY_RES: (u32, u32) = (WINDOW_WIDTH, WINDOW_HEIGHT);

/// Событие [1 с; 3 с) с текстом `text`.
fn two_second_event(text: &str) -> (String, String, String) {
    (
        "0:00:01.00".to_owned(),
        "0:00:03.00".to_owned(),
        text.to_owned(),
    )
}

/// Суммарное отличие кадра с субтитрами от кадра без них: «сколько субтитра видно».
fn visible_amount(with_subtitles: &[u8], video_only: &[u8]) -> u64 {
    with_subtitles
        .iter()
        .zip(video_only)
        .map(|(shown, background)| u64::from(shown.abs_diff(*background)))
        .sum()
}

#[test]
fn move_places_the_subtitle_between_its_endpoints_in_the_frame_at_the_middle_of_the_move() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let script = animated_script(
        PLAY_RES,
        &[two_second_event(
            "{\\an5\\move(100,180,540,180)\\c&H0000FF&}MOVE",
        )],
    );
    let (mut owner, _dir) = owner_with_script(media, &script);
    let rect = full_window_picture();

    // Начало, середина (1 с из 2 с) и конец интервала движения. Кадры идут подряд с шагом
    // 40 мс, как в воспроизведении: часы показа должны наблюдать настоящий шаг кадров.
    let mut frames_by_time = std::collections::BTreeMap::new();
    for pts_ms in (1_000..=2_960_u64).step_by(40) {
        let pixels = render_placed(&mut owner, &mut harness, media, rect, pts_ms);
        frames_by_time.insert(pts_ms, pixels);
    }
    let (start, middle, end) = (
        &frames_by_time[&1_000],
        &frames_by_time[&2_000],
        &frames_by_time[&2_960],
    );

    let start_x = bounds_of(start, WINDOW_WIDTH, is_red)
        .expect("subtitle at start")
        .center_x();
    let middle_x = bounds_of(middle, WINDOW_WIDTH, is_red)
        .expect("subtitle at middle")
        .center_x();
    let end_x = bounds_of(end, WINDOW_WIDTH, is_red)
        .expect("subtitle at end")
        .center_x();
    assert!((start_x - 100.0).abs() <= 6.0, "start x {start_x}");
    assert!((end_x - 540.0).abs() <= 12.0, "end x {end_x}");
    assert!(
        (middle_x - 320.0).abs() <= 8.0,
        "middle x {middle_x}: halfway between 100 and 540"
    );
    assert!(
        middle_x - start_x > 100.0 && end_x - middle_x > 100.0,
        "{start_x} {middle_x} {end_x}"
    );
    assert_ne!(middle, start);
    assert_ne!(middle, end);
}

#[test]
fn fade_in_makes_the_subtitle_fainter_at_the_start_than_in_the_middle() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let script = animated_script(
        PLAY_RES,
        &[two_second_event(
            "{\\an5\\pos(320,180)\\fad(1000,0)\\c&H0000FF&}FADE",
        )],
    );
    let (mut owner, _dir) = owner_with_script(media, &script);
    let rect = full_window_picture();
    let (_, video_only) = harness.frame(rect, None);

    let beginning = render_placed(&mut owner, &mut harness, media, rect, 1_000);
    let early = render_placed(&mut owner, &mut harness, media, rect, 1_200);
    let middle = render_placed(&mut owner, &mut harness, media, rect, 2_000);

    assert_eq!(
        beginning, video_only,
        "fully transparent at the start: nothing is drawn"
    );
    let early_amount = visible_amount(&early, &video_only);
    let middle_amount = visible_amount(&middle, &video_only);
    assert!(early_amount > 0, "the fade has started");
    assert!(
        early_amount * 2 < middle_amount,
        "fade start {early_amount} must be much fainter than the middle {middle_amount}"
    );
    assert!(
        count_of(&middle, is_red) > 50,
        "full colour after the fade-in"
    );
}
