//! Следующий кадр по просьбе рендера, пока таблица HDR → SDR строится в фоне (решение 38).
//!
//! Приложение в паузе спит; готовую таблицу замечает только `draw`. Рендер обязан сам сказать
//! «нужен ещё кадр» ровно на время фоновой сборки и не дольше.

use std::time::{Duration, Instant};

use render_shell_api::{
    FollowUpFrame, FrameMaterializationOutcome, RenderFrameOutcome, RenderShell,
};

use super::{FrameExtras, connect_software_decoder, offscreen_or_skip, pixel, present};

const TARGET: [u32; 2] = [64, 32];

/// Живое видео HDR: смена уровней → «нужен кадр» до готовности таблицы, потом «не нужен»;
/// картинка после готовности другая (новая таблица реально встала на экран).
#[test]
fn a_new_hdr_table_asks_for_frames_until_it_is_on_screen() {
    let test_name = "a_new_hdr_table_asks_for_frames_until_it_is_on_screen";
    let Some(mut renderer) = offscreen_or_skip(test_name, TARGET) else {
        return;
    };
    if !renderer.capabilities().supports_hdr_to_sdr {
        eprintln!("skip {test_name}: driver has no highp for HDR");
        return;
    }
    assert_eq!(
        renderer.needs_follow_up_frame(),
        FollowUpFrame::NotNeeded,
        "nothing is built before the first HDR frame"
    );
    let decoder = connect_software_decoder(&mut renderer);
    let lease = decoder.lease_hdr(1, TARGET);
    let FrameMaterializationOutcome::Ready(key) =
        renderer.video_frames().materialize(&lease).outcome
    else {
        panic!("the HDR frame must be ready");
    };

    // Первая таблица строится синхронно: после кадра ждать нечего.
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    assert_eq!(renderer.needs_follow_up_frame(), FollowUpFrame::NotNeeded);
    let before = pixel(&renderer, 10, 16);

    // Другие уровни HDR → нужна новая таблица, фон строит её; на экране пока старая.
    let mut settings = renderer.live_settings();
    settings.hdr_to_sdr.sdr_reference_white_nits = 203.0;
    renderer
        .live_settings_adapter()
        .commit_live_settings(&settings)
        .expect("HDR levels are live-editable");
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    assert_eq!(
        renderer.needs_follow_up_frame(),
        FollowUpFrame::Soon,
        "the new table is still being built in the background"
    );

    // Приложение перерисовывается, пока рендер просит; ограничение по времени — защита от
    // зависания теста, а не часть поведения.
    let deadline = Instant::now() + Duration::from_secs(60);
    while renderer.needs_follow_up_frame() == FollowUpFrame::Soon {
        assert!(
            Instant::now() < deadline,
            "the HDR table never became ready"
        );
        std::thread::sleep(Duration::from_millis(2));
        assert!(matches!(
            present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
            RenderFrameOutcome::Presented(_)
        ));
    }
    let after = pixel(&renderer, 10, 16);
    assert!(
        (0..3).any(|channel| before[channel].abs_diff(after[channel]) > 2),
        "the new table must reach the screen: {before:?} -> {after:?}"
    );

    // Устоялось: повторный кадр больше ничего не просит.
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    assert_eq!(renderer.needs_follow_up_frame(), FollowUpFrame::NotNeeded);
}

/// Таблица ждёт новую сборку, но видео пропало (закрыли файл): ждать некого, рендер не держит
/// приложение в бесконечной перерисовке.
#[test]
fn a_pending_hdr_table_stops_asking_when_the_video_is_gone() {
    let test_name = "a_pending_hdr_table_stops_asking_when_the_video_is_gone";
    let Some(mut renderer) = offscreen_or_skip(test_name, TARGET) else {
        return;
    };
    if !renderer.capabilities().supports_hdr_to_sdr {
        eprintln!("skip {test_name}: driver has no highp for HDR");
        return;
    }
    let decoder = connect_software_decoder(&mut renderer);
    let lease = decoder.lease_hdr(1, TARGET);
    let FrameMaterializationOutcome::Ready(key) =
        renderer.video_frames().materialize(&lease).outcome
    else {
        panic!("the HDR frame must be ready");
    };
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    let mut settings = renderer.live_settings();
    settings.hdr_to_sdr.sdr_reference_white_nits = 203.0;
    renderer
        .live_settings_adapter()
        .commit_live_settings(&settings)
        .expect("HDR levels are live-editable");
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    assert_eq!(renderer.needs_follow_up_frame(), FollowUpFrame::Soon);

    assert!(matches!(
        present(&mut renderer, TARGET, None, FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));

    assert_eq!(
        renderer.needs_follow_up_frame(),
        FollowUpFrame::NotNeeded,
        "no video on screen, nobody waits for the table"
    );
}
