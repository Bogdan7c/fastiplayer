//! Перемотка зрителя у эфира, ставшего записью (мост 16d1): настоящий процесс с окном.
//!
//! Поколение открылось эфиром, а потом страница назвала конечную длительность (как hls.js,
//! Shaka и dash.js в конце эфира). Шкала плеера стала записью, и перемотка зрителя должна
//! работать по всей записи:
//! - за скачанное — просьба странице, потом кадры и игра у цели;
//! - после ухода вкладки (досмотр) — внутрь скачанного садится. На коде до 16d1 окно
//!   помнило «эфир» с открытия и отказывало: «only the downloaded part can be played».
//!
//! Нужен `kwin_wayland` для окна (иначе тесты SKIPPED).

use super::generation_switch_clicks::{
    CLIP_US, FRAME_US, is_ended, is_playing_from, page_seek_targets, viewer_seek_by,
    wait_until_controllable, window_position_us,
};
use super::live_timeline_changes::{
    END_OF_STREAM, LIVE_DURATION, RECORDING_DURATION, RECORDING_LENGTH_US, six_seconds_announced_as,
};
use super::next_episode::wait_for_mpris_length_near;
use super::page_variants::player_seek_started;
use super::seek_waits_for_page::{OneGenerationPage, TARGET_US};
use super::session_ends::{send_all, wait_until_playing_past};
use super::*;

/// Номер сообщения о длительности среди записанных сообщений начала поколения.
const DURATION_MESSAGE_INDEX: usize = 1;
/// Допуск цели, которую плеер называет странице: граница кадра.
const TARGET_SLACK_US: u64 = FRAME_US;
/// Перемотка MPRIS относительная: к цели прибавляется ход между чтением позиции и командой.
const RELATIVE_SEEK_DRIFT_US: u64 = 300_000;
/// Сколько секунд досмотра дать, прежде чем перематывать назад.
const PLAYED_BEFORE_SEEK_BACK_US: u64 = 3_000_000;
/// Перемотка назад во время досмотра — на 2 с, внутрь скачанного.
const SEEK_BACK_BY_US: i64 = -2_000_000;
/// Сколько ждать, прежде чем смотреть, где стоит окно после перемотки.
const AFTER_SEEK: Duration = Duration::from_millis(800);
/// Строка журнала отказа досмотра «только скачанное».
const DOWNLOADED_ONLY_REFUSAL: &str = "Viewer seek outside of what was downloaded refused";

/// Эфир, ставший записью: длина в MPRIS — признак, что окно уже видит запись.
fn wait_until_the_stream_is_a_recording(bus: &PrivateBusWithBrowserSession) {
    bus.wait_for_log_line("Live timeline became a recording");
    wait_for_mpris_length_near(bus, RECORDING_LENGTH_US);
    wait_until_controllable(bus);
}

#[test]
fn a_viewer_seek_past_the_download_of_an_ended_live_stream_is_asked_from_the_page_and_plays() {
    let Some(process) = SessionProcessUnderTest::start("ended-live-viewer-seek", VideoMode::Window)
    else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    // Эфир: скачано начало (видео 0–2,4 с), дальше страница ещё качает.
    let (mut page, mut head) = OneGenerationPage::start();
    head[DURATION_MESSAGE_INDEX] = Message::text(LIVE_DURATION);
    send_all(&mut socket, head);
    wait_until_playing_past(&mut socket, 1_000_000);

    // Эфир кончился: страница назвала длину всей записи (6 с), байты дальше ещё не пришли.
    send_all(&mut socket, vec![Message::text(RECORDING_DURATION)]);
    wait_until_the_stream_is_a_recording(bus);

    // Зритель перематывает за скачанное (у эфира это было бы «за окном»).
    let position_us = window_position_us(bus);
    viewer_seek_by(
        bus,
        i64::try_from(TARGET_US).expect("target") - i64::try_from(position_us).expect("position"),
    );
    let until_seek =
        read_player_messages_until(&mut socket, "the player asks the page to seek", |message| {
            matches!(message, PlayerSessionMessage::Seek { .. })
        });
    let Some(PlayerSessionMessage::Seek { seek_id, target_us }) = until_seek.last() else {
        panic!("the player did not ask the page to seek: {until_seek:?}");
    };
    let asked_us = target_us.as_micros();
    assert!(
        (TARGET_US - TARGET_SLACK_US..=TARGET_US + RELATIVE_SEEK_DRIFT_US).contains(&asked_us),
        "the page was asked for {asked_us} µs instead of {TARGET_US} µs: {:?}",
        page_seek_targets(&until_seek)
    );

    // Страница перематывается и приносит байты цели — плеер играет оттуда до конца.
    socket
        .send(player_seek_started(seek_id.0, asked_us))
        .expect("the page answers the seek");
    send_all(&mut socket, page.bytes_from_the_target());
    let rest = read_player_messages_until(&mut socket, "the end of the recording", is_ended);
    assert!(
        rest.iter()
            .any(|message| is_playing_from(message, TARGET_US - TARGET_SLACK_US)),
        "the player did not play from the target"
    );

    send_tab_gone_and_expect_close(&mut socket);
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    // Кадры от цели до конца записи (4,8–6 с = 30 кадров) дошли до показа.
    let frames_from_the_target = (CLIP_US - TARGET_US) / FRAME_US;
    assert!(
        log_number(&summary, "presented_frames") >= frames_from_the_target,
        "{summary}"
    );
}

#[test]
fn a_seek_back_into_the_download_of_an_ended_live_stream_lands_after_the_tab_is_gone() {
    let Some(process) = SessionProcessUnderTest::start("ended-live-drain-seek", VideoMode::Window)
    else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    // Эфир скачан целиком (6 с), потом страница сделала его записью и закончила поток.
    send_all(&mut socket, six_seconds_announced_as(LIVE_DURATION));
    wait_until_playing_past(&mut socket, 1_000_000);
    send_all(
        &mut socket,
        vec![
            Message::text(RECORDING_DURATION),
            Message::text(END_OF_STREAM),
        ],
    );
    wait_until_the_stream_is_a_recording(bus);

    // Вкладка ушла: окно досматривает скачанное.
    send_tab_gone_and_expect_close(&mut socket);
    let deadline = Instant::now() + WAIT;
    while window_position_us(bus) < PLAYED_BEFORE_SEEK_BACK_US {
        assert!(Instant::now() < deadline, "the drain did not play on");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Назад на 2 с — это скачано, страница для этого не нужна.
    let before_seek_us = window_position_us(bus);
    viewer_seek_by(bus, SEEK_BACK_BY_US);
    std::thread::sleep(AFTER_SEEK);
    let after_seek_us = window_position_us(bus);
    let expected_us = before_seek_us - SEEK_BACK_BY_US.unsigned_abs();
    assert!(
        after_seek_us < before_seek_us
            && after_seek_us <= expected_us + AFTER_SEEK.as_micros() as u64 + TARGET_SLACK_US,
        "the seek back into the download did not land: {before_seek_us} → {after_seek_us} µs"
    );

    let finished =
        bus.wait_for_log_line_without("Playback drain finished", &[DOWNLOADED_ONLY_REFUSAL]);
    assert!(
        finished.contains("class=played_to_end"),
        "the drain ended otherwise than by playing to the end: {finished}"
    );
}
