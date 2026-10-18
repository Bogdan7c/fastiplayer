//! Поэтапное появление слов до пикселей: WebVTT-реплика с таймстемпом внутри текста рядом с
//! «видео» -> владелец -> настоящий рисовальщик -> размещение -> настоящий GPU-проход -> readback.
//! Слово, чьё время ещё не наступило, не рисуется и не занимает места: строка растёт по ходу
//! показа. Тесты пропускаются (с сообщением), если на машине нет GPU adapter-а.

use super::ass_gpu_support::*;
use super::gpu_chain::Harness;
use super::*;

macro_rules! gpu_or_skip {
    () => {
        match Harness::with_size(WINDOW_WIDTH, WINDOW_HEIGHT) {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the word reveal test");
                return;
            }
        }
    };
}

/// Светлый пиксель текста: красный канал «видео» (64) и затемнённой плашки ниже порога.
fn is_light(pixel: [u8; 4]) -> bool {
    pixel[0] > 150
}

/// Реплика с 0 по 5 с; слово «WORLD» появляется на 2-й секунде.
const REVEAL_VTT: &str = "WEBVTT\n\n00:00.000 --> 00:05.000\nHELLO <00:02.000>WORLD\n";

/// Кадр окна в момент `pts_millis`: владелец -> размещение -> GPU -> пиксели.
fn frame_at(
    owner: &mut SubtitleOwner,
    harness: &mut Harness,
    media: MediaInstanceId,
    pts_millis: u64,
) -> Vec<u8> {
    render_placed(owner, harness, media, full_window_picture(), pts_millis)
}

#[test]
fn unrevealed_word_is_absent_from_the_frame_and_the_line_grows_when_it_appears() {
    let mut harness = gpu_or_skip!();
    let media = instance(1);
    let (directory, video) = super::track_selection::movie_with(&[("Movie.vtt", REVEAL_VTT)]);
    let mut owner = owner_with_config(&enabled_config());
    begin_and_wait(&mut owner, media, &video);
    super::track_selection::settle(&mut owner);
    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Ready);

    let before = frame_at(&mut owner, &mut harness, media, 1_000);
    let just_before = frame_at(&mut owner, &mut harness, media, 1_999);
    let after = frame_at(&mut owner, &mut harness, media, 3_000);
    let before_text = bounds_of(&before, WINDOW_WIDTH, is_light).expect("HELLO is drawn");
    let after_text = bounds_of(&after, WINDOW_WIDTH, is_light).expect("HELLO WORLD is drawn");

    assert_eq!(
        before, just_before,
        "the picture does not change until the word appears"
    );
    assert!(
        after_text.max_x - after_text.min_x > (before_text.max_x - before_text.min_x) * 3 / 2,
        "two words are much wider than one: {before_text:?} -> {after_text:?}"
    );
    // Текст по центру: при появлении слова строка растёт в обе стороны.
    assert!(
        after_text.min_x < before_text.min_x,
        "{before_text:?} -> {after_text:?}"
    );
    assert!(
        after_text.max_x > before_text.max_x,
        "{before_text:?} -> {after_text:?}"
    );

    // Перемотка назад скрывает слово снова: кадр совпадает с тем, что был до появления.
    let rewound = frame_at(&mut owner, &mut harness, media, 1_000);
    assert_eq!(rewound, before);
    drop(directory);
}
