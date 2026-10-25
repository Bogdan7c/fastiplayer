//! Один текущий кадр у видеослоя: ключ, который уже не «текущий», всё равно рисует СВОЮ
//! картинку (замороженный кадр, повтор прошлого кадра, scrub, кадры прежнего декодера), а не
//! чужую или устаревшую.

use render_shell_api::{
    FrameMaterializationOutcome, RenderFrameFailureKind, RenderFrameOutcome, RenderShell,
};

use super::{
    FrameExtras, assert_gray, connect_software_decoder, materialize_key, offscreen_or_skip, pixel,
    present,
};
use crate::test_support::fake_decoder::{LookupMode, SplitPicture};

const TARGET: [u32; 2] = [64, 32];

fn picture(handle: u64, left_luma: u8, right_luma: u8) -> SplitPicture {
    SplitPicture {
        handle,
        size: TARGET,
        left_luma,
        right_luma,
    }
}

fn show(
    renderer: &mut crate::Gles2Renderer,
    key: &render_shell_api::RenderableFrameKey,
) -> RenderFrameOutcome {
    present(renderer, TARGET, Some(key), FrameExtras::default())
}

/// Более старый ключ при более новом кадре на слое рисует свою картинку, а не картинку нового
/// кадра; возврат к новому ключу — снова его картинка.
#[test]
fn an_older_key_draws_its_own_picture_while_a_newer_frame_is_on_the_layer() {
    let Some(mut renderer) = offscreen_or_skip(
        "an_older_key_draws_its_own_picture_while_a_newer_frame_is_on_the_layer",
        TARGET,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (older, _older_lease) = materialize_key(&mut renderer, &decoder, picture(1, 180, 60));
    let (newer, _newer_lease) = materialize_key(&mut renderer, &decoder, picture(2, 30, 220));

    assert!(matches!(
        show(&mut renderer, &older),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(pixel(&renderer, 16, 16), 180, "older key, left half");
    assert_gray(pixel(&renderer, 48, 16), 60, "older key, right half");
    assert_eq!(renderer.frame_counters().video_frames_reshown, 1);

    assert!(matches!(
        show(&mut renderer, &newer),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(pixel(&renderer, 16, 16), 30, "newer key, left half");
    assert_gray(pixel(&renderer, 48, 16), 220, "newer key, right half");
    assert_eq!(renderer.frame_counters().video_frames_reshown, 2);

    // Тот же ключ подряд — картинка уже на слое: заново ничего не возвращается.
    assert!(matches!(
        show(&mut renderer, &newer),
        RenderFrameOutcome::Presented(_)
    ));
    assert_eq!(renderer.frame_counters().video_frames_reshown, 2);
}

/// Кадры прежнего декодера и нового с ОДНИМ номером ресурса не путаются: новый ключ рисует
/// картинку нового декодера, а замороженный ключ старого — свою.
#[test]
fn keys_of_a_replaced_decoder_never_mix_with_the_new_decoder_pictures() {
    let Some(mut renderer) = offscreen_or_skip(
        "keys_of_a_replaced_decoder_never_mix_with_the_new_decoder_pictures",
        TARGET,
    ) else {
        return;
    };
    let old_decoder = connect_software_decoder(&mut renderer);
    let (old_key, _old_lease) = materialize_key(&mut renderer, &old_decoder, picture(1, 180, 60));
    assert!(matches!(
        show(&mut renderer, &old_key),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(pixel(&renderer, 16, 16), 180, "old decoder frame");

    // Смена декодера: ключи кадров начинаются заново — номер ресурса 1 снова, другая картинка.
    let new_decoder = connect_software_decoder(&mut renderer);
    let (new_key, _new_lease) = materialize_key(&mut renderer, &new_decoder, picture(1, 30, 220));
    assert!(matches!(
        show(&mut renderer, &new_key),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(
        pixel(&renderer, 16, 16),
        30,
        "the new decoder frame, not the stale picture of the old one",
    );
    assert_gray(
        pixel(&renderer, 48, 16),
        220,
        "new decoder frame, right half",
    );

    // Замороженный кадр старого декодера по-прежнему рисует свою картинку.
    assert!(matches!(
        show(&mut renderer, &old_key),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(
        pixel(&renderer, 16, 16),
        180,
        "frozen frame of the old decoder",
    );
    assert_gray(
        pixel(&renderer, 48, 16),
        60,
        "frozen frame of the old decoder",
    );

    // А следующий кадр нового декодера с тем же номером не принимается за картинку старого.
    assert!(matches!(
        show(&mut renderer, &new_key),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(pixel(&renderer, 16, 16), 30, "the new decoder again");
}

/// Не удалось вернуть картинку ключа — отказ кадра с причиной, а не молча чужая картинка;
/// прежний кадр цели остаётся как был.
#[test]
fn a_key_that_cannot_be_shown_again_fails_loudly_instead_of_drawing_a_stale_picture() {
    let Some(mut renderer) = offscreen_or_skip(
        "a_key_that_cannot_be_shown_again_fails_loudly_instead_of_drawing_a_stale_picture",
        TARGET,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (older, _older_lease) = materialize_key(&mut renderer, &decoder, picture(1, 180, 60));
    let (newer, _newer_lease) = materialize_key(&mut renderer, &decoder, picture(2, 30, 220));
    assert!(matches!(
        show(&mut renderer, &newer),
        RenderFrameOutcome::Presented(_)
    ));

    decoder.state.set_mode(LookupMode::Missing);
    let outcome = show(&mut renderer, &older);

    match outcome {
        RenderFrameOutcome::Failed(failure) => {
            assert_eq!(failure.kind, RenderFrameFailureKind::VideoRender);
            assert!(
                failure.message.contains("cannot be shown again"),
                "{}",
                failure.message
            );
        }
        other => panic!("a key that cannot be restored must fail, got {other:?}"),
    }
    assert_gray(
        pixel(&renderer, 16, 16),
        30,
        "the target keeps the last shown frame, not the stale picture",
    );

    // Провайдер снова отвечает — тот же ключ рисуется.
    decoder.state.set_mode(LookupMode::Ready);
    assert!(matches!(
        show(&mut renderer, &older),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(
        pixel(&renderer, 16, 16),
        180,
        "the key after the provider recovered",
    );
}

/// Все слоты кольца заняты загрузками без показа: возврат картинки старого ключа закрывает их
/// fence-ом и получает слот, а не падает.
#[test]
fn bringing_a_picture_back_survives_a_ring_full_of_unretired_uploads() {
    let Some(mut renderer) = offscreen_or_skip(
        "bringing_a_picture_back_survives_a_ring_full_of_unretired_uploads",
        TARGET,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (first, _first_lease) = materialize_key(&mut renderer, &decoder, picture(1, 180, 60));
    let (_second, _second_lease) = materialize_key(&mut renderer, &decoder, picture(2, 30, 220));
    let (_third, _third_lease) = materialize_key(&mut renderer, &decoder, picture(3, 90, 90));
    // Три загрузки без единого показа заняли кольцо: следующая получает `Busy`.
    let fourth_lease = decoder.lease(picture(4, 10, 10));
    assert!(
        renderer
            .video_frames()
            .materialize(&fourth_lease)
            .lookup_was_busy(),
        "the ring must be full for this scenario"
    );

    let outcome = show(&mut renderer, &first);

    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "{outcome:?}"
    );
    assert_gray(pixel(&renderer, 16, 16), 180, "the first key restored");
    assert_gray(pixel(&renderer, 48, 16), 60, "the first key restored");
    assert_eq!(renderer.frame_counters().video_frames_reshown, 1);
}

/// Подготовка нового кадра не влияет на картинку, которую рисуют старые ключи, пока кадр не
/// стал текущим по ключу: `Busy` оставляет прежний кадр на слое.
#[test]
fn a_busy_materialization_leaves_the_current_picture_untouched() {
    let Some(mut renderer) = offscreen_or_skip(
        "a_busy_materialization_leaves_the_current_picture_untouched",
        TARGET,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (current, _lease) = materialize_key(&mut renderer, &decoder, picture(1, 180, 60));
    decoder.state.set_mode(LookupMode::Busy);
    let next_lease = decoder.lease(picture(2, 30, 220));
    assert!(matches!(
        renderer.video_frames().materialize(&next_lease).outcome,
        FrameMaterializationOutcome::Busy
    ));

    assert!(matches!(
        show(&mut renderer, &current),
        RenderFrameOutcome::Presented(_)
    ));
    decoder.state.set_mode(LookupMode::Ready);

    assert_gray(pixel(&renderer, 16, 16), 180, "previous frame after Busy");
    assert_eq!(
        renderer.frame_counters().video_frames_reshown,
        0,
        "no picture had to be brought back"
    );
}
