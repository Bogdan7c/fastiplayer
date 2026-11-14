//! HLS-эфир под мостом потерял нить окна (мост 16d3, решение 333): настоящий процесс.
//!
//! Страница отдаёт эфир. Первые обновления — прежнее окно, потом — окно «после сна»:
//! номера сегментов ушли далеко вперёд, ни одного прежнего сегмента в нём нет (так было в
//! цикле 5 сна на T480s). Последнее обновление закрывает эфир `ENDLIST`, чтобы ролик
//! доигрался до конца. Ожидается: сеанс не кончается «Stream error», поколение не
//! отвергнуто, кадры после разрыва дошли до рендера, позиция не откатывалась. На коде до
//! 16d3 движок падал с «live refresh continuity failed» и поколение отвергалось.

use super::hls_page::{
    FRAGMENT_FRAMES, FRAGMENT_SECONDS, HELLO_1_2, HlsPage, PageResource, announce_hls_generation,
    bridge_address, final_summary, fragmented_movie, open_attached_session_speaking,
};
use super::*;

/// Номерок корневого плейлиста.
const ROOT_HANDLE: u32 = 0;
/// Номерок init-сегмента.
const INIT_HANDLE: u32 = 1;
/// Номерки сегментов прежнего окна (дальше подряд).
const OLD_WINDOW_FIRST_HANDLE: u32 = 100;
/// Номерки сегментов окна «после сна»: другие номерки, как у новых сегментов сайта.
const AFTER_SLEEP_FIRST_HANDLE: u32 = 200;
/// Media sequence окна «после сна»: далеко за прежним окном.
const AFTER_SLEEP_FIRST_SEQUENCE: usize = 1_000;
/// Сколько сегментов ролика в каждом окне.
const WINDOW_SEGMENTS: usize = 5;
/// Сколько раз страница отдаёт прежнее окно, прежде чем «проснуться».
const OLD_WINDOW_REFRESHES: usize = 2;

/// Плейлист эфира fMP4 из сегментов ролика `fragments` с номерками от `first_handle`.
fn live_playlist(
    media_sequence: usize,
    first_handle: u32,
    fragments: std::ops::Range<usize>,
    ended: bool,
) -> Vec<u8> {
    let mut text = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{media_sequence}\n\
         #EXT-X-MAP:URI=\"{}\"\n",
        bridge_address(INIT_HANDLE)
    );
    for offset in 0..fragments.len() {
        text.push_str(&format!(
            "#EXTINF:{FRAGMENT_SECONDS:.3},\n{}\n",
            bridge_address(first_handle + offset as u32)
        ));
    }
    if ended {
        text.push_str("#EXT-X-ENDLIST\n");
    }
    text.into_bytes()
}

/// Страница эфира, окно которого за время «сна» ушло целиком вперёд.
fn sleeping_live_page() -> HlsPage {
    let movie = fragmented_movie();
    assert!(movie.fragments.len() >= 2 * WINDOW_SEGMENTS);
    let old_window = 0..WINDOW_SEGMENTS;
    let after_sleep = WINDOW_SEGMENTS..2 * WINDOW_SEGMENTS;
    let mut snapshots = vec![
        live_playlist(0, OLD_WINDOW_FIRST_HANDLE, old_window.clone(), false);
        OLD_WINDOW_REFRESHES
    ];
    snapshots.push(live_playlist(
        AFTER_SLEEP_FIRST_SEQUENCE,
        AFTER_SLEEP_FIRST_HANDLE,
        after_sleep.clone(),
        false,
    ));
    snapshots.push(live_playlist(
        AFTER_SLEEP_FIRST_SEQUENCE,
        AFTER_SLEEP_FIRST_HANDLE,
        after_sleep.clone(),
        true,
    ));
    let mut resources = vec![
        (ROOT_HANDLE, PageResource::Snapshots(snapshots)),
        (INIT_HANDLE, PageResource::Body(movie.init.clone())),
    ];
    for (offset, index) in old_window.enumerate() {
        resources.push((
            OLD_WINDOW_FIRST_HANDLE + offset as u32,
            PageResource::Body(movie.fragments[index].clone()),
        ));
    }
    for (offset, index) in after_sleep.enumerate() {
        resources.push((
            AFTER_SLEEP_FIRST_HANDLE + offset as u32,
            PageResource::Body(movie.fragments[index].clone()),
        ));
    }
    HlsPage::new(resources)
}

#[test]
fn an_hls_live_stream_whose_window_moved_past_everything_known_keeps_playing_through_the_bridge() {
    let Some(process) = SessionProcessUnderTest::start("hls-live-rejoin", VideoMode::Headless)
    else {
        return;
    };
    let mut page = sleeping_live_page();
    let mut socket = open_attached_session_speaking(&process, HELLO_1_2);
    announce_hls_generation(&mut socket, ROOT_HANDLE);

    let messages = page.serve_until_ended(&mut socket);

    // Поколение не отвергнуто: на коде до 16d3 здесь был `stream_rejected`.
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. })),
        "the live generation was rejected after the lost window"
    );
    let after_sleep_requests: usize = (0..WINDOW_SEGMENTS as u32)
        .map(|offset| page.requests_for(AFTER_SLEEP_FIRST_HANDLE + offset))
        .sum();
    assert!(
        after_sleep_requests > 0,
        "segments after the gap were never fetched"
    );
    process.bus.wait_for_log_line("rejoined near the live edge");

    send_tab_gone_and_expect_close(&mut socket);
    let summary = final_summary(&process.bus);
    // Прежнее окно даёт не больше WINDOW_SEGMENTS × FRAGMENT_FRAMES кадров: всё сверх —
    // кадры окна «после сна», дошедшие до рендера.
    let old_window_frames = WINDOW_SEGMENTS as u64 * FRAGMENT_FRAMES;
    assert!(
        summary.presented_frames >= old_window_frames + FRAGMENT_FRAMES,
        "no frames after the gap: {}",
        summary.line
    );
    // Склейка без прыжка назад: позиция ни разу не откатывалась.
    assert_eq!(summary.position_regressions, 0, "{}", summary.line);
}
