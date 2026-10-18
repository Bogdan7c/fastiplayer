//! Потоковые субтитры до пикселей: подставной транспорт -> настоящая лента -> владелец ->
//! настоящий рисовальщик -> настоящий GPU-проход -> readback. Тесты пропускаются (с сообщением),
//! если на машине нет GPU adapter-а, как остальные тесты цепочки субтитров.

use std::time::Duration;

use fastiplayer_config::SubtitleStreamConfig;

use super::super::stream_position::StreamTimelineKind;
use super::ass_gpu_support::*;
use super::embedded_support::wait_until;
use super::gpu_chain::Harness;
use super::stream_support::*;
use super::web_support::click_menu_row;
use super::*;

macro_rules! gpu_or_skip {
    () => {
        match Harness::with_size(WINDOW_WIDTH, WINDOW_HEIGHT) {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the stream subtitle test");
                return;
            }
        }
    };
}

/// Светлый пиксель текста: красный канал «видео» и затемнённой плашки ниже порога.
fn is_light(pixel: [u8; 4]) -> bool {
    pixel[0] > 150
}

fn frame_at(
    owner: &mut SubtitleOwner,
    harness: &mut Harness,
    media: MediaInstanceId,
    pts_millis: u64,
) -> Vec<u8> {
    render_placed(owner, harness, media, full_window_picture(), pts_millis)
}

fn has_ink(pixels: &[u8]) -> bool {
    bounds_of(pixels, WINDOW_WIDTH, is_light).is_some()
}

fn english_and_russian() -> Vec<media_source_open::stream_subtitles::StreamSubtitleTrackInfo> {
    vec![
        stream_track_info(0, Some("en"), "English", StreamFlags::AUTO),
        stream_track_info(1, Some("ru"), "Russian", StreamFlags::AUTO),
    ]
}

#[test]
fn stream_cue_is_drawn_only_after_its_segment_arrives_and_only_while_active() {
    let mut harness = gpu_or_skip!();
    let media = instance(1);
    // Сегмент 0 (0..10 с): реплика [1,5 с; 3,0 с). Калитка закрыта: «сеть» пока молчит.
    let transport = FakeStreamTransport::new(
        english_and_russian(),
        10,
        vec![vec![CueSpec::new(1_500, 3_000, "Hello stream")]],
        false,
    );
    let mut owner = stream_owner(
        &SubtitlesConfig::default(),
        media,
        stream_source(&transport),
        Duration::from_millis(2_000),
    );
    let before_selection = frame_at(&mut owner, &mut harness, media, 2_000);
    assert!(!has_ink(&before_selection), "subtitles are off: no ink");

    click_menu_row(&mut owner, "English (stream)");
    wait_until(&mut owner, "the feed to ask for the segment", |_| {
        transport.fetch_calls() >= 1
    });
    let before_segment = frame_at(&mut owner, &mut harness, media, 2_000);
    assert!(
        !has_ink(&before_segment),
        "the segment has not arrived yet: nothing to draw, playback is not blocked"
    );

    transport.open_gate();
    wait_for_cues(&mut owner, 1);
    let during_cue = frame_at(&mut owner, &mut harness, media, 2_000);
    let after_cue = frame_at(&mut owner, &mut harness, media, 3_500);
    let before_cue = frame_at(&mut owner, &mut harness, media, 1_000);

    assert!(has_ink(&during_cue), "the arrived cue is drawn at its time");
    assert!(!has_ink(&after_cue), "and gone after its end");
    assert!(!has_ink(&before_cue), "and not drawn before its start");
}

#[test]
fn after_an_unavailable_track_the_reselect_brings_the_cue_to_the_screen() {
    let mut harness = gpu_or_skip!();
    let media = instance(1);
    let transport = FakeStreamTransport::new(
        english_and_russian(),
        10,
        vec![vec![CueSpec::new(1_500, 3_000, "Retry me")]],
        true,
    );
    transport.set_failing(true);
    let config = SubtitlesConfig {
        stream: SubtitleStreamConfig {
            failure_threshold: 1,
            ..SubtitleStreamConfig::default()
        },
        ..SubtitlesConfig::default()
    };
    let mut owner = stream_owner(
        &config,
        media,
        stream_source(&transport),
        Duration::from_millis(2_000),
    );

    click_menu_row(&mut owner, "English (stream)");
    let notices = collect_notices_for(&mut owner, Duration::from_millis(500));
    let failed = frame_at(&mut owner, &mut harness, media, 2_000);
    transport.set_failing(false);
    click_menu_row(&mut owner, "English (stream)");
    wait_for_cues(&mut owner, 1);
    let retried = frame_at(&mut owner, &mut harness, media, 2_000);

    assert_eq!(notices.len(), 1, "one notice for the failure");
    assert!(!has_ink(&failed), "the video plays without subtitles");
    assert!(has_ink(&retried), "the retry draws the cue");
}

#[test]
fn live_history_rebuild_keeps_the_current_cue_on_the_screen() {
    let mut harness = gpu_or_skip!();
    let media = instance(1);
    let cues: Vec<CueSpec> = (0..100)
        .map(|second| {
            CueSpec::new(
                second * 1_000,
                second * 1_000 + 900,
                &format!("live {second}"),
            )
        })
        .collect();
    let transport = FakeStreamTransport::new(english_and_russian(), 100, vec![cues], true);
    let config = SubtitlesConfig {
        stream: SubtitleStreamConfig {
            live_retention_s: 30,
            ..SubtitleStreamConfig::default()
        },
        ..SubtitlesConfig::default()
    };
    let mut owner = stream_owner(&config, media, stream_source(&transport), Duration::ZERO);
    observe_position(&mut owner, media, Duration::ZERO, StreamTimelineKind::Live);
    click_menu_row(&mut owner, "English (stream)");
    wait_for_cues(&mut owner, 100);
    let early = frame_at(&mut owner, &mut harness, media, 10_300);
    assert!(
        has_ink(&early),
        "an early cue is drawn before the history moves on"
    );

    observe_position(
        &mut owner,
        media,
        Duration::from_secs(99),
        StreamTimelineKind::Live,
    );
    let current = frame_at(&mut owner, &mut harness, media, 99_300);
    let evicted = frame_at(&mut owner, &mut harness, media, 10_300);

    assert_eq!(displayed_cue_count(&owner), 31, "the old cues were dropped");
    assert!(
        has_ink(&current),
        "the cue at the live position is still drawn after the rebuild"
    );
    assert!(!has_ink(&evicted), "the dropped cue is not drawn any more");
}
