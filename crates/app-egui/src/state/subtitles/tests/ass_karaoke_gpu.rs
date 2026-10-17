//! Караоке `\kf` до пикселей (сессия 10b): сценарий рядом с «видео» -> настоящий поток-рисовальщик
//! -> размещение -> настоящий GPU-проход -> readback. Проверяется то, что видит зритель: в
//! середине слога заливка в КАДРЕ частичная — основной цвет слева от вертикальной границы,
//! вторичный справа, и граница идёт вправо вместе со временем. Тесты пропускаются (с
//! сообщением), если на машине нет GPU adapter-а.

use super::ass_animation_support::*;
use super::ass_gpu_support::*;
use super::gpu_chain::Harness;
use super::*;

macro_rules! gpu_or_skip {
    () => {
        match Harness::with_size(WINDOW_WIDTH, WINDOW_HEIGHT) {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the ASS karaoke test");
                return;
            }
        }
    };
}

/// PlayRes сценариев равен окну: координаты тегов = пиксели кадра.
const PLAY_RES: (u32, u32) = (WINDOW_WIDTH, WINDOW_HEIGHT);
/// Событие идёт с 1-й по 3-ю секунду: караоке считает время от начала события.
const EVENT_START_MILLIS: u64 = 1_000;

/// Красный основной и зелёный вторичный цвет: ни того, ни другого нет в видео фикстуры.
/// Слог `WWWWWW` длится секунду и начинается через 200 мс после начала события.
const KARAOKE_TEXT: &str =
    "{\\an5\\pos(320,180)\\fs80\\bord0\\1c&H0000FF&\\2c&H00FF00&\\kf20\\kf100}WWWWWW";

/// Кадр окна с караоке-событием в момент `elapsed_millis` от начала события.
fn karaoke_frame(harness: &mut Harness, media_number: u64, elapsed_millis: u64) -> Vec<u8> {
    let media = instance(media_number);
    let script = animated_script(
        PLAY_RES,
        &[(
            "0:00:01.00".to_owned(),
            "0:00:03.00".to_owned(),
            KARAOKE_TEXT.to_owned(),
        )],
    );
    let (mut owner, _directory) = owner_with_script(media, &script);
    render_placed(
        &mut owner,
        harness,
        media,
        full_window_picture(),
        EVENT_START_MILLIS + elapsed_millis,
    )
}

#[test]
fn kf_fill_is_partial_in_the_middle_of_the_syllable_in_the_frame() {
    let mut harness = gpu_or_skip!();
    // До слога (100 мс) все буквы вторичного цвета, после (1300 мс) — основного.
    let unsung = karaoke_frame(&mut harness, 21, 100);
    let sung = karaoke_frame(&mut harness, 22, 1_300);
    let text_in_green = bounds_of(&unsung, WINDOW_WIDTH, is_green).expect("unsung letters");
    let text_in_red = bounds_of(&sung, WINDOW_WIDTH, is_red).expect("sung letters");
    assert_eq!(count_of(&unsung, is_red), 0, "nothing is sung yet");
    assert_eq!(count_of(&sung, is_green), 0, "everything is sung");
    assert!(
        text_in_green.max_x - text_in_green.min_x > 200,
        "{text_in_green:?}"
    );

    // Середина слога (700 мс из 200..1200): граница посередине слова.
    let middle = karaoke_frame(&mut harness, 23, 700);
    let red = bounds_of(&middle, WINDOW_WIDTH, is_red).expect("sung part in the frame");
    let green = bounds_of(&middle, WINDOW_WIDTH, is_green).expect("unsung part in the frame");
    assert!(
        count_of(&middle, is_red) > 300 && count_of(&middle, is_green) > 300,
        "both colours are really there"
    );
    // Основной цвет целиком слева от вторичного: граница вертикальная (край в пиксель).
    assert!(
        red.max_x <= green.min_x + 2,
        "red {red:?} must end where green {green:?} begins"
    );
    // Слово покрыто целиком: слева начинается красный, справа кончается зелёный.
    assert!(
        red.min_x.abs_diff(text_in_red.min_x) <= 2,
        "{red:?} {text_in_red:?}"
    );
    assert!(
        green.max_x.abs_diff(text_in_green.max_x) <= 2,
        "{green:?} {text_in_green:?}"
    );
    let boundary = (red.max_x + green.min_x) / 2;
    assert!(
        boundary > text_in_red.min_x + 40 && boundary < text_in_red.max_x - 40,
        "boundary {boundary} is inside the letters {text_in_red:?}"
    );

    // Позже в слоге граница правее.
    let later = karaoke_frame(&mut harness, 24, 1_000);
    let later_red = bounds_of(&later, WINDOW_WIDTH, is_red).expect("sung part");
    assert!(later_red.max_x > red.max_x + 20, "{later_red:?} vs {red:?}");
}
