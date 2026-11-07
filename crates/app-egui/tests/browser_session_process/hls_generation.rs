//! Сквозные тесты 10a1: HLS-поколение браузерного режима до кадров в рендере.
//!
//! Настоящий бинарь, настоящий плеер (без окна и в окне), звук — тихие часы. Поддельный
//! экстеншен говорит на протоколе 1.1: объявляет `hls_source_added{root}` и раздаёт
//! каждый ресурс по номерку (`fetch_resource` → рамки вида 2 + `resource_fetch_finished`).
//! Доказательство — итог плеера: показанные кадры, без откатов позиции, со звуком.

use super::hls_page::{
    FRAGMENT_FRAMES, HELLO_1_1, HlsPage, PageResource, announce_hls_generation, bridge_address,
    final_summary, fmp4_vod_playlist, fragmented_movie, hls_fixture,
    open_attached_session_speaking,
};
use super::*;

/// Кадров в двух TS/fMP4-сегментах фикстур приложения (10 к/с × 2 с).
const TWO_SEGMENT_FRAMES: u64 = 20;

/// Плейлист VOD из двух сегментов по номеркам (как после переписчика экстеншена).
fn two_segment_playlist(map: Option<u32>, first: u32, second: u32, version: u8) -> Vec<u8> {
    let map_line = map.map_or(String::new(), |init| {
        format!("#EXT-X-MAP:URI=\"{}\"\n", bridge_address(init))
    });
    format!(
        "#EXTM3U\n#EXT-X-VERSION:{version}\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-PLAYLIST-TYPE:VOD\n{map_line}#EXTINF:1.000000,\n{}\n#EXTINF:1.000000,\n{}\n\
         #EXT-X-ENDLIST\n",
        bridge_address(first),
        bridge_address(second),
    )
    .into_bytes()
}

/// Плеер показал от `at_least` до `expected` кадров без откатов и со звуком.
fn assert_played_between(summary: &super::hls_page::PlaybackSummary, at_least: u64, expected: u64) {
    assert!(
        (at_least..=expected).contains(&summary.presented_frames),
        "expected {at_least}..={expected} presented frames: {}",
        summary.line
    );
    assert_eq!(summary.position_regressions, 0, "{}", summary.line);
    assert!(summary.audio_played_ms > 0, "{}", summary.line);
}

/// Плеер сыграл примерно `expected` кадров без откатов и со звуком.
fn assert_played(summary: &super::hls_page::PlaybackSummary, expected: u64) {
    assert!(
        (expected.saturating_sub(3)..=expected).contains(&summary.presented_frames),
        "expected about {expected} presented frames: {}",
        summary.line
    );
    assert_eq!(summary.position_regressions, 0, "{}", summary.line);
    assert!(summary.audio_played_ms > 0, "{}", summary.line);
}

/// VOD MPEG-TS: корень и оба сегмента пришли по номеркам, плеер показал кадры до конца.
///
/// Окна у этого теста нет: фикстура 16×16, а видеовыход окна такую картинку не
/// показывает вовсе (то же у fMP4 16×16; 64×64 в окне — ниже). Без окна TS даёт не все
/// 20 кадров: в PMT нет размеров картинки, плеер переизбирает видеодекодер на ходу и
/// продолжает со следующего ключевого кадра (`ContinueForwardToKeyframe`), то есть
/// второй сегмент показывается целиком, а первый — частично. Это поведение ядра для
/// любого TS-HLS, не моста.
#[test]
fn a_ts_vod_generation_is_fetched_by_handles_and_played_to_the_end() {
    let Some(process) = SessionProcessUnderTest::start("hls-ts-vod", VideoMode::Headless) else {
        return;
    };
    let mut socket = open_attached_session_speaking(&process, HELLO_1_1);
    announce_hls_generation(&mut socket, 0);
    let mut page = HlsPage::new([
        (0, PageResource::Body(two_segment_playlist(None, 1, 2, 3))),
        (1, PageResource::Body(hls_fixture("ts-0.ts"))),
        (2, PageResource::Body(hls_fixture("ts-1.ts"))),
    ]);
    let messages = page.serve_until_ended(&mut socket);

    assert_eq!(
        page.requests.first().map(|request| request.resource),
        Some(0)
    );
    for handle in [1, 2] {
        assert!(
            page.requests_for(handle) >= 1,
            "segment {handle} was never asked"
        );
    }
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::BufferState { .. })),
        "an HLS generation never gets buffer_state"
    );
    send_tab_gone_and_expect_close(&mut socket);
    process
        .bus
        .wait_for_log_line("Playback drain finished class=played_to_end");
    let resources = process
        .bus
        .wait_for_log_line("Browser HLS resource requests");
    assert_eq!(
        log_number(&resources, "resource_requests_sent"),
        page.requests.len() as u64,
        "{resources}"
    );
    // Второй сегмент — весь (10 кадров) и хотя бы что-то из первого.
    assert_played_between(
        &final_summary(&process.bus),
        TWO_SEGMENT_FRAMES / 2 + 1,
        TWO_SEGMENT_FRAMES,
    );
    process
        .bus
        .wait_for_log_line("Browser session process finished");
}

/// VOD fMP4: init через `EXT-X-MAP` по номерку, десять фрагментов — 100 кадров.
#[test]
fn an_fmp4_vod_generation_with_a_map_init_plays_all_frames() {
    fmp4_vod_plays_all_frames(VideoMode::Headless);
}

/// То же в окне: HLS-поколение идёт путём открытия `AppState` окна, вариант выбран по
/// возможностям плеера окна (их окно называет, когда поднимется).
#[test]
fn an_fmp4_vod_generation_with_a_map_init_plays_all_frames_in_a_window() {
    fmp4_vod_plays_all_frames(VideoMode::Window);
}

fn fmp4_vod_plays_all_frames(mode: VideoMode) {
    let Some(process) = SessionProcessUnderTest::start("hls-fmp4-vod", mode) else {
        return;
    };
    let movie = fragmented_movie();
    let mut resources = vec![
        (
            0,
            PageResource::Body(fmp4_vod_playlist(1, 10, movie.fragments.len())),
        ),
        (1, PageResource::Body(movie.init.clone())),
    ];
    for (index, fragment) in movie.fragments.iter().enumerate() {
        resources.push((10 + index as u32, PageResource::Body(fragment.clone())));
    }
    let mut socket = open_attached_session_speaking(&process, HELLO_1_1);
    announce_hls_generation(&mut socket, 0);
    let mut page = HlsPage::new(resources);
    page.serve_until_ended(&mut socket);

    assert!(
        page.requests_for(1) >= 1,
        "the EXT-X-MAP init was never asked"
    );
    send_tab_gone_and_expect_close(&mut socket);
    assert_played(
        &final_summary(&process.bus),
        FRAGMENT_FRAMES * movie.fragments.len() as u64,
    );
}

/// Вторая fMP4-фикстура приложения (сегменты по 1 с, init отдельным номерком).
#[test]
fn the_fmp4_fixture_of_the_app_plays_through_the_bridge() {
    let Some(process) = SessionProcessUnderTest::start("hls-fmp4-app", VideoMode::Headless) else {
        return;
    };
    let mut socket = open_attached_session_speaking(&process, HELLO_1_1);
    announce_hls_generation(&mut socket, 0);
    let mut page = HlsPage::new([
        (
            0,
            PageResource::Body(two_segment_playlist(Some(3), 1, 2, 7)),
        ),
        (1, PageResource::Body(hls_fixture("fmp4-0.m4s"))),
        (2, PageResource::Body(hls_fixture("fmp4-1.m4s"))),
        (3, PageResource::Body(hls_fixture("fmp4-init.mp4"))),
    ]);
    page.serve_until_ended(&mut socket);
    send_tab_gone_and_expect_close(&mut socket);
    assert_played(&final_summary(&process.bus), TWO_SEGMENT_FRAMES);
}
