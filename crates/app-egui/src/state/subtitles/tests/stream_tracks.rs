//! Дорожки потока (HLS-рендиции) у владельца: меню, автовыбор, жизненный цикл ленты, сбои,
//! история live и память выбора. Транспорт подставной, всё остальное — настоящие владелец,
//! лента `media-source-open` и разбор WebVTT.

use std::time::Duration;

use fastiplayer_config::SubtitleStreamConfig;
use media_source_open::stream_subtitles::StreamSubtitleTrackId;

use super::super::menu_view::SubtitleTrackAvailability;
use super::super::stream_position::StreamTimelineKind;
use super::embedded_support::wait_until;
use super::stream_support::*;
use super::track_selection::overlay_present;
use super::web_support::{click_menu_row, menu_texts};
use super::*;
use crate::state::notifications::{NotificationCenter, OpenProgress};
use crate::subtitle_message::STREAM_SUBTITLES_UNAVAILABLE_MESSAGE;
use crate::ui::animation::UiMotion;

/// Сегмент `k` (10 с): одна реплика [k*10 с + 0,5 с; k*10 с + 2,5 с) с текстом `cue k`.
fn ten_second_segments(count: u32) -> Vec<Vec<CueSpec>> {
    (0..count)
        .map(|key| {
            vec![CueSpec::new(
                key * 10_000 + 500,
                key * 10_000 + 2_500,
                &format!("cue {key}"),
            )]
        })
        .collect()
}

fn english_and_russian() -> Vec<media_source_open::stream_subtitles::StreamSubtitleTrackInfo> {
    vec![
        stream_track_info(0, Some("en"), "English", StreamFlags::AUTO),
        stream_track_info(1, Some("ru"), "Russian", StreamFlags::AUTO),
    ]
}

fn running_feed_track_name(owner: &SubtitleOwner) -> Option<String> {
    let track_id = owner.running_stream_feed_track()?;
    let view = owner.menu_view();
    let (index, track) = view
        .tracks
        .iter()
        .enumerate()
        .find(|(_, track)| track.id == track_id)?;
    Some(crate::subtitle_message::track_text(
        &track.label,
        crate::subtitle_message::TrackNumber::from_menu_index(index),
    ))
}

fn toast_texts(center: &mut NotificationCenter) -> Vec<String> {
    center
        .frame(
            OpenProgress::Idle,
            UiMotion::Standard,
            std::time::Instant::now(),
        )
        .toasts
        .iter()
        .map(|toast| toast.message.to_string())
        .collect()
}

#[test]
fn menu_lists_every_rendition_and_no_feed_runs_until_one_is_shown() {
    let transport = FakeStreamTransport::new(
        vec![
            stream_track_info(0, Some("en"), "English", StreamFlags::AUTO),
            stream_track_info(
                1,
                Some("ru"),
                "Russian",
                StreamFlags {
                    forced: true,
                    default: false,
                    autoselect: true,
                },
            ),
            stream_track_info(2, Some("fr"), "Francais", StreamFlags::AUTO),
            stream_track_info(3, None, "Commentary", StreamFlags::AUTO),
        ],
        10,
        ten_second_segments(3),
        true,
    );
    let owner = stream_owner(
        &SubtitlesConfig::default(),
        instance(1),
        stream_source(&transport),
        Duration::ZERO,
    );

    let texts: Vec<String> = menu_texts(&owner)
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    assert_eq!(
        texts,
        [
            "English (stream)",
            "Russian — forced (stream)",
            "French \"Francais\" (stream)",
            "Commentary (stream)",
        ]
    );
    assert!(owner.menu_view().has_tracks, "the CC button is available");
    assert_eq!(owner.running_stream_feed_track(), None);
    assert_eq!(
        transport.fetch_calls(),
        0,
        "nothing is downloaded while subtitles are off"
    );
    assert!(transport.indexed_tracks().is_empty());
}

#[test]
fn selecting_a_rendition_starts_its_feed_and_cues_reach_the_screen_at_their_time() {
    let transport =
        FakeStreamTransport::new(english_and_russian(), 10, ten_second_segments(3), true);
    let media = instance(1);
    let mut owner = stream_owner(
        &SubtitlesConfig::default(),
        media,
        stream_source(&transport),
        Duration::from_secs(1),
    );

    click_menu_row(&mut owner, "English (stream)");
    assert_eq!(
        running_feed_track_name(&owner).as_deref(),
        Some("English (stream)")
    );
    wait_for_cues(&mut owner, 1);

    assert!(
        overlay_present(&mut owner, media, 1_000),
        "cue 0 is on screen at 1 s"
    );
    assert_eq!(displayed_active_texts(&owner, 1_000), ["cue 0"]);
    assert!(
        !overlay_present(&mut owner, media, 4_000),
        "gone after the cue ends"
    );
    assert_eq!(
        transport.indexed_tracks(),
        [StreamSubtitleTrackId::new(0)],
        "only the shown rendition is requested"
    );
}

#[test]
fn a_forced_rendition_is_shown_while_subtitles_are_off() {
    let transport = FakeStreamTransport::new(
        vec![
            stream_track_info(0, Some("en"), "English", StreamFlags::AUTO),
            stream_track_info(
                1,
                Some("ru"),
                "Russian forced",
                StreamFlags {
                    forced: true,
                    default: false,
                    autoselect: true,
                },
            ),
        ],
        10,
        ten_second_segments(2),
        true,
    );
    let media = instance(1);
    let config = SubtitlesConfig {
        enabled: false,
        preferred_languages: "ru".to_owned(),
        ..SubtitlesConfig::default()
    };
    let mut owner = stream_owner(
        &config,
        media,
        stream_source(&transport),
        Duration::from_secs(1),
    );

    assert_eq!(
        running_feed_track_name(&owner).as_deref(),
        Some("Russian \"Russian forced\" — forced (stream)"),
        "the forced rendition of the preferred language is fed even with CC off"
    );
    wait_for_cues(&mut owner, 1);
    assert_eq!(displayed_active_texts(&owner, 1_000), ["cue 0"]);
    assert_eq!(
        owner.menu_view().visibility,
        super::super::intents::SubtitleVisibility::Hidden,
        "CC itself stays off"
    );
}

#[test]
fn auto_select_honours_default_and_autoselect_flags_of_the_manifest() {
    let manual_only = FakeStreamTransport::new(
        vec![
            stream_track_info(
                0,
                Some("en"),
                "English",
                StreamFlags {
                    forced: false,
                    default: false,
                    autoselect: false,
                },
            ),
            stream_track_info(
                1,
                Some("de"),
                "Deutsch",
                StreamFlags {
                    forced: false,
                    default: false,
                    autoselect: false,
                },
            ),
        ],
        10,
        ten_second_segments(1),
        true,
    );
    let owner = stream_owner(
        &enabled_config(),
        instance(1),
        stream_source(&manual_only),
        Duration::ZERO,
    );
    assert_eq!(
        owner.running_stream_feed_track(),
        None,
        "AUTOSELECT=NO renditions are never picked without the user"
    );

    let with_default = FakeStreamTransport::new(
        vec![
            stream_track_info(0, Some("en"), "English", StreamFlags::AUTO),
            stream_track_info(
                1,
                Some("de"),
                "Deutsch",
                StreamFlags {
                    forced: false,
                    default: true,
                    autoselect: true,
                },
            ),
        ],
        10,
        ten_second_segments(1),
        true,
    );
    let owner = stream_owner(
        &enabled_config(),
        instance(1),
        stream_source(&with_default),
        Duration::ZERO,
    );
    assert_eq!(
        running_feed_track_name(&owner).as_deref(),
        Some("German \"Deutsch\" (stream)"),
        "DEFAULT=YES wins among renditions with no preferred language"
    );

    // Ручной выбор разрешён и для AUTOSELECT=NO.
    let mut manual = stream_owner(
        &enabled_config(),
        instance(2),
        stream_source(&manual_only),
        Duration::ZERO,
    );
    click_menu_row(&mut manual, "German \"Deutsch\" (stream)");
    assert!(manual.running_stream_feed_track().is_some());
}

#[test]
fn switching_the_rendition_cancels_the_old_feed_and_starts_the_new_one() {
    // Калитка закрыта: лента первой дорожки зависла в загрузке сегмента.
    let transport =
        FakeStreamTransport::new(english_and_russian(), 10, ten_second_segments(3), false);
    let mut owner = stream_owner(
        &SubtitlesConfig::default(),
        instance(1),
        stream_source(&transport),
        Duration::from_secs(1),
    );
    click_menu_row(&mut owner, "English (stream)");
    wait_until(&mut owner, "the first feed to enter the segment", |_| {
        transport.fetch_calls() >= 1
    });
    assert!(!transport.cancellation_seen());

    click_menu_row(&mut owner, "Russian (stream)");

    assert_eq!(
        running_feed_track_name(&owner).as_deref(),
        Some("Russian (stream)")
    );
    wait_until(
        &mut owner,
        "the old feed to observe the cancellation",
        |_| transport.cancellation_seen(),
    );
    transport.open_gate();
    wait_for_cues(&mut owner, 1);
    assert_eq!(
        transport.indexed_tracks(),
        [StreamSubtitleTrackId::new(0), StreamSubtitleTrackId::new(1)]
    );
    let counters = owner.stream_feed_counters();
    assert_eq!(
        counters.cues_added, 3,
        "the new feed delivered its three segments"
    );
    assert_eq!(
        counters.duplicate_cues, 0,
        "the cancelled feed delivered nothing"
    );
}

#[test]
fn turning_subtitles_off_cancels_the_feed() {
    let transport =
        FakeStreamTransport::new(english_and_russian(), 10, ten_second_segments(3), false);
    let mut owner = stream_owner(
        &SubtitlesConfig::default(),
        instance(1),
        stream_source(&transport),
        Duration::from_secs(1),
    );
    click_menu_row(&mut owner, "English (stream)");
    wait_until(&mut owner, "the feed to enter the segment", |_| {
        transport.fetch_calls() >= 1
    });

    owner.apply_control_action(crate::ui::player_controls::SubtitleControlAction::ToggleVisibility);

    assert_eq!(owner.running_stream_feed_track(), None);
    wait_until(&mut owner, "the feed to observe the cancellation", |_| {
        transport.cancellation_seen()
    });
}

#[test]
fn media_change_cancels_the_feed_and_old_cues_never_reach_the_new_media() {
    let transport =
        FakeStreamTransport::new(english_and_russian(), 10, ten_second_segments(3), true);
    let first = instance(1);
    let mut owner = stream_owner(
        &SubtitlesConfig::default(),
        first,
        stream_source(&transport),
        Duration::from_secs(1),
    );
    click_menu_row(&mut owner, "English (stream)");
    // Лента отдала сегмент в очередь, но владелец её ещё не читал.
    wait_until_unpolled(|| transport.fetch_calls() >= 1);
    std::thread::sleep(Duration::from_millis(100));

    let second = instance(2);
    owner.begin_media(second, SubtitleMediaSource::NotLocal);
    for _ in 0..50 {
        owner.poll_jobs();
        std::thread::sleep(Duration::from_millis(2));
    }

    assert_eq!(owner.running_stream_feed_track(), None);
    assert_eq!(
        displayed_cue_count(&owner),
        0,
        "the new media has no stale cues"
    );
    assert_eq!(owner.stream_feed_counters().cues_added, 0);
    assert!(!overlay_present(&mut owner, second, 1_000));
}

/// Ждёт условие, не опрашивая владельца (проверка «сообщения старого media не читаются»).
fn wait_until_unpolled(condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn clearing_the_media_cancels_a_hung_feed() {
    let transport =
        FakeStreamTransport::new(english_and_russian(), 10, ten_second_segments(3), false);
    let mut owner = stream_owner(
        &SubtitlesConfig::default(),
        instance(1),
        stream_source(&transport),
        Duration::from_secs(1),
    );
    click_menu_row(&mut owner, "English (stream)");
    wait_until(&mut owner, "the feed to enter the segment", |_| {
        transport.fetch_calls() >= 1
    });

    owner.clear_media();

    wait_until_unpolled(|| transport.cancellation_seen());
}

#[test]
fn unavailable_track_gives_exactly_one_notice_stays_selectable_and_reselect_restarts_the_feed() {
    let transport =
        FakeStreamTransport::new(english_and_russian(), 10, ten_second_segments(3), true);
    transport.set_failing(true);
    let config = SubtitlesConfig {
        stream: SubtitleStreamConfig {
            failure_threshold: 1,
            ..SubtitleStreamConfig::default()
        },
        ..SubtitlesConfig::default()
    };
    let media = instance(1);
    let mut owner = stream_owner(
        &config,
        media,
        stream_source(&transport),
        Duration::from_secs(1),
    );

    click_menu_row(&mut owner, "English (stream)");
    let notices = collect_notices_for(&mut owner, Duration::from_millis(600));

    assert_eq!(
        notices.len(),
        1,
        "exactly one notice for the dead track: {notices:?}"
    );
    let mut center = NotificationCenter::default();
    super::super::app_state_api::deliver_stream_notices(
        &mut center,
        &notices,
        std::time::Instant::now(),
    );
    assert_eq!(
        toast_texts(&mut center),
        ["Subtitles for this stream are unavailable right now."]
    );
    assert_eq!(
        STREAM_SUBTITLES_UNAVAILABLE_MESSAGE,
        "Subtitles for this stream are unavailable right now."
    );
    assert_eq!(
        owner.running_stream_feed_track(),
        None,
        "the dead feed is dropped"
    );
    let row = owner
        .menu_view()
        .tracks
        .into_iter()
        .find(|track| track.is_selected)
        .expect("still selected");
    assert_eq!(row.availability, SubtitleTrackAvailability::Available);
    let calls_after_failure = transport.fetch_calls();
    let quiet = collect_notices_for(&mut owner, Duration::from_millis(300));
    assert!(quiet.is_empty(), "no repeated notices");
    assert_eq!(
        transport.fetch_calls(),
        calls_after_failure,
        "no automatic retry"
    );

    // Сеть вернулась; пользователь снова выбирает дорожку.
    transport.set_failing(false);
    click_menu_row(&mut owner, "English (stream)");
    assert_eq!(
        running_feed_track_name(&owner).as_deref(),
        Some("English (stream)")
    );
    wait_for_cues(&mut owner, 1);

    assert_eq!(displayed_active_texts(&owner, 1_000), ["cue 0"]);
    assert!(transport.fetch_calls() > calls_after_failure);
    assert!(owner.take_stream_notices().is_empty());
}

#[test]
fn skipped_segments_below_the_threshold_are_only_logged() {
    let transport =
        FakeStreamTransport::new(english_and_russian(), 10, ten_second_segments(2), true);
    transport.set_failing(true);
    let mut owner = stream_owner(
        &SubtitlesConfig::default(),
        instance(1),
        stream_source(&transport),
        Duration::from_secs(1),
    );

    click_menu_row(&mut owner, "English (stream)");
    wait_until(&mut owner, "both segments to be skipped", |owner| {
        owner.stream_feed_counters().segments_skipped >= 2
    });
    let notices = collect_notices_for(&mut owner, Duration::from_millis(200));

    assert!(
        notices.is_empty(),
        "default threshold is 3, two skips are silent"
    );
    assert!(
        owner.running_stream_feed_track().is_some(),
        "the feed keeps running"
    );
}

#[test]
fn seek_moves_the_feed_window_to_the_new_position() {
    let transport =
        FakeStreamTransport::new(english_and_russian(), 10, ten_second_segments(30), true);
    let config = SubtitlesConfig {
        stream: SubtitleStreamConfig {
            ahead_s: 5,
            behind_s: 0,
            live_retention_s: 30,
            ..SubtitleStreamConfig::default()
        },
        ..SubtitlesConfig::default()
    };
    let media = instance(1);
    let mut owner = stream_owner(
        &config,
        media,
        stream_source(&transport),
        Duration::from_millis(100),
    );
    click_menu_row(&mut owner, "English (stream)");
    wait_for_cues(&mut owner, 1);
    assert_eq!(displayed_active_texts(&owner, 1_000), ["cue 0"]);
    assert!(
        displayed_active_texts(&owner, 200_800).is_empty(),
        "segment 20 is far away"
    );

    // Seek на 200 с: цель seek будит ленту, она качает сегмент вокруг цели.
    owner.observe_stream_playback(
        current(media, 1),
        &super::super::stream_position::StreamPlaybackObservation {
            position: Duration::from_secs(200),
            seek_target: Some(Duration::from_secs(200)),
            timeline: StreamTimelineKind::Vod,
        },
    );
    wait_for_cues(&mut owner, 2);

    assert_eq!(displayed_active_texts(&owner, 200_800), ["cue 20"]);
}

#[test]
fn stream_unavailable_and_web_download_toasts_coexist_and_each_replaces_its_own() {
    use super::super::app_state_api::{deliver_download_failures, deliver_stream_notices};
    use media_source_open::web_subtitle_fetch::WebSubtitleFetchError;

    let now = std::time::Instant::now();
    let mut center = NotificationCenter::default();
    let stream_notice = [super::super::owner_stream::StreamSubtitleNotice::TrackUnavailable];

    deliver_download_failures(&mut center, &[WebSubtitleFetchError::NotFound], now);
    deliver_stream_notices(&mut center, &stream_notice, now);
    assert_eq!(
        toast_texts(&mut center).len(),
        2,
        "different kinds do not replace each other"
    );

    // Повтор каждого вида заменяет только свою плашку.
    deliver_stream_notices(&mut center, &stream_notice, now);
    deliver_download_failures(&mut center, &[WebSubtitleFetchError::Timeout], now);
    let texts = toast_texts(&mut center);
    assert_eq!(
        texts.len(),
        2,
        "each repeat replaced its own toast: {texts:?}"
    );
    assert!(texts.contains(&STREAM_SUBTITLES_UNAVAILABLE_MESSAGE.to_owned()));
    assert!(
        texts.contains(
            &crate::subtitle_message::subtitle_download_failure_message(
                &WebSubtitleFetchError::Timeout
            )
            .to_string()
        ),
        "the download toast shows the latest failure: {texts:?}"
    );
}
