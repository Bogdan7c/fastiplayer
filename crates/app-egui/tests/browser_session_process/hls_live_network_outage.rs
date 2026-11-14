//! HLS-эфир под мостом переживает обрыв сети (мост 16d3, решение 334): настоящий процесс.
//!
//! Эфир идёт по часам стены. Посреди просмотра плейлист несколько секунд не скачивается:
//! страница отвечает «сетевая ошибка» (так бывает, пока после пробуждения поднимается
//! Wi-Fi). Это дольше трёх быстрых попыток транспорта (~0,3 с), на которых эфир раньше
//! падал. Ожидается: поколение не отвергнуто, после возвращения сети плейлист снова
//! обновляется и плеер играет дальше — позиция уходит за то место, где кончились данные
//! на момент обрыва.

use std::rc::Rc;

use super::hls_live_window::{live_page, live_playlist_now, no_rejections, playing, serve_for};
use super::hls_page::{
    HELLO_1_2, PageResource, announce_hls_generation, open_attached_session_speaking,
};
use super::*;

/// Номерок live-корня (как у `hls_live_window`).
const ROOT_HANDLE: u32 = 0;
/// Сколько эфир играет до обрыва.
const BEFORE_OUTAGE: Duration = Duration::from_secs(4);
/// Сколько длится обрыв: много дольше быстрых попыток транспорта, но короче окна эфира
/// (15 сегментов по 0,4 с = 6 с), чтобы проверять именно ожидание сети, а не склейку.
const OUTAGE: Duration = Duration::from_secs(4);
/// Сколько слушаем после возвращения сети.
const AFTER_OUTAGE: Duration = Duration::from_secs(6);
/// Насколько позиция обязана уйти дальше прежнего максимума: больше, чем запас скачанного.
const PROGRESS_AFTER_OUTAGE_US: u64 = 2_000_000;

#[test]
fn an_hls_live_playlist_outage_through_the_bridge_is_waited_out() {
    let Some(process) = SessionProcessUnderTest::start("hls-live-outage", VideoMode::Headless)
    else {
        return;
    };
    let mut page = live_page();
    let on_air_since = Instant::now();
    let mut socket = open_attached_session_speaking(&process, HELLO_1_2);
    announce_hls_generation(&mut socket, ROOT_HANDLE);

    let before = serve_for(
        &mut page,
        &mut socket,
        "live playback before the outage",
        BEFORE_OUTAGE,
    );
    let reached_before = playing(&before)
        .into_iter()
        .max()
        .expect("the live stream played");

    page.replace(ROOT_HANDLE, PageResource::NetworkError);
    let refreshes_before_outage = page.requests_for(ROOT_HANDLE);
    let during = serve_for(&mut page, &mut socket, "the network outage", OUTAGE);
    let failed_refreshes = page.requests_for(ROOT_HANDLE) - refreshes_before_outage;

    page.replace(
        ROOT_HANDLE,
        PageResource::Generated(Rc::new(move || live_playlist_now(on_air_since))),
    );
    let after = serve_for(
        &mut page,
        &mut socket,
        "live playback after the outage",
        AFTER_OUTAGE,
    );

    for heard in [&before, &during, &after] {
        no_rejections(heard);
    }
    // Обрыв был: плейлист повторяли, а не сдались после первой серии попыток.
    assert!(
        failed_refreshes > 3,
        "the playlist was not retried during the outage: {failed_refreshes} requests"
    );
    let reached_after = playing(&after)
        .into_iter()
        .max()
        .expect("playback after the outage");
    assert!(
        reached_after >= reached_before + PROGRESS_AFTER_OUTAGE_US,
        "playback did not go on after the network came back: {reached_before} -> {reached_after} µs"
    );
    send_tab_gone_and_expect_close(&mut socket);
}
