//! Эфир HLS по номеркам кончается (мост 16d1, решения 325–327, протокол 1.11): настоящий процесс.
//!
//! Страница отдаёт эфир: корневой плейлист без `EXT-X-ENDLIST`, на обновлениях — тот же
//! плейлист, дописанный до конца ролика, с `ENDLIST`. Ожидается: плеер доигрывает ролик до
//! конца, его шкала становится записью (журнал «Live timeline became a recording»), а
//! странице 1.11 после окон эфира уходит окно записи с краем — длиной всего ролика. На коде
//! до 16d1 шкала оставалась эфиром, и странице окна записи не было вовсе.

use super::hls_page::{
    FRAGMENT_SECONDS, HlsPage, PageResource, announce_hls_generation, bridge_address,
    final_summary, fragmented_movie, open_attached_session_speaking,
};
use super::*;

/// `hello` экстеншена 1.11: окно записи такая страница понимает.
const HELLO_1_11: &str = r#"{"type":"hello","protocol":{"major":1,"minor":11},"software_version":"0.1.0","capabilities":[]}"#;
/// Номерок корневого плейлиста.
const ROOT_HANDLE: u32 = 0;
/// Номерок init-сегмента.
const INIT_HANDLE: u32 = 1;
/// Номерок первого медиасегмента (дальше подряд).
const FIRST_SEGMENT_HANDLE: u32 = 100;
/// Сколько сегментов в эфире при открытии; остальные появятся вместе с `ENDLIST`.
const SEGMENTS_ON_AIR: usize = 6;
/// Кадров ролика фикстуры (10 сегментов по 10 кадров) минус кадры у краёв.
const FRAMES_AT_LEAST: u64 = 90;
/// Допуск края записи: граница кадра (25 к/с).
const EDGE_SLACK_US: u64 = 40_000;

/// Плейлист эфира fMP4: первые `segments` сегментов ролика; `ended` — эфир кончился.
fn live_playlist(segments: usize, ended: bool) -> Vec<u8> {
    let mut text = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-MAP:URI=\"{}\"\n",
        bridge_address(INIT_HANDLE)
    );
    for index in 0..segments {
        text.push_str(&format!(
            "#EXTINF:{FRAGMENT_SECONDS:.3},\n{}\n",
            bridge_address(FIRST_SEGMENT_HANDLE + index as u32)
        ));
    }
    if ended {
        text.push_str("#EXT-X-ENDLIST\n");
    }
    text.into_bytes()
}

/// Страница эфира, который кончается на первом обновлении плейлиста.
fn ending_live_page() -> (HlsPage, usize) {
    let movie = fragmented_movie();
    let total_segments = movie.fragments.len();
    let mut resources = vec![
        (
            ROOT_HANDLE,
            PageResource::Snapshots(vec![
                live_playlist(SEGMENTS_ON_AIR, false),
                live_playlist(total_segments, true),
            ]),
        ),
        (INIT_HANDLE, PageResource::Body(movie.init.clone())),
    ];
    for (index, fragment) in movie.fragments.iter().enumerate() {
        resources.push((
            FIRST_SEGMENT_HANDLE + index as u32,
            PageResource::Body(fragment.clone()),
        ));
    }
    (HlsPage::new(resources), total_segments)
}

#[test]
fn an_hls_live_stream_that_got_endlist_plays_to_its_end_and_tells_the_page_it_is_a_recording() {
    let Some(process) = SessionProcessUnderTest::start("hls-live-endlist", VideoMode::Headless)
    else {
        return;
    };
    let (mut page, total_segments) = ending_live_page();
    let mut socket = open_attached_session_speaking(&process, HELLO_1_11);
    announce_hls_generation(&mut socket, ROOT_HANDLE);

    let messages = page.serve_until_ended(&mut socket);

    process
        .bus
        .wait_for_log_line("Live timeline became a recording");
    let first_live = messages
        .iter()
        .position(|message| matches!(message, PlayerSessionMessage::LiveWindow { .. }))
        .expect("the page got a live window while the stream was on air");
    let recording_ends: Vec<(usize, u64)> = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| match message {
            PlayerSessionMessage::RecordingWindow { end_us, .. } => {
                Some((index, end_us.as_micros()))
            }
            _ => None,
        })
        .collect();
    let (first_recording, recording_end_us) = *recording_ends
        .first()
        .expect("the page got a recording window after the stream ended");
    assert!(
        first_recording > first_live,
        "the recording window follows the live window"
    );
    // Край записи — весь ролик, окончательный сразу (решение 325).
    let whole_clip_us = (total_segments as f64 * FRAGMENT_SECONDS * 1_000_000.0) as u64;
    assert!(
        recording_end_us.abs_diff(whole_clip_us) <= EDGE_SLACK_US,
        "the recording ends at {recording_end_us} µs, the clip at {whole_clip_us} µs"
    );
    assert!(
        recording_ends
            .iter()
            .all(|(_, end_us)| *end_us == recording_end_us),
        "the end of the recording did not move: {recording_ends:?}"
    );
    // Эфир в запись — без возврата: после окна записи окон эфира нет.
    assert!(
        !messages[first_recording..]
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::LiveWindow { .. })),
        "a live window came after the recording window"
    );

    send_tab_gone_and_expect_close(&mut socket);
    let summary = final_summary(&process.bus);
    assert!(
        summary.presented_frames >= FRAMES_AT_LEAST,
        "{}",
        summary.line
    );
}
