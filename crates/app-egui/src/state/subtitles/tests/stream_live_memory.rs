//! История живой трансляции и память выбора дорожек потока у владельца: вытеснение старых реплик
//! пересборкой таймлайна (с гистерезисом), отсутствие вытеснения у VOD, применение настроек и
//! запоминание/восстановление выбора. Транспорт подставной, остальное настоящее.

use std::time::Duration;

use fastiplayer_config::SubtitleStreamConfig;
use playlist_state::{RememberedStreamSubtitle, RememberedSubtitleChoice};
use subtitle_core::SubtitleSyncOffset;

use super::super::stream_position::StreamTimelineKind;
use super::stream_support::*;
use super::web_support::click_menu_row;
use super::*;
use crate::state::subtitles::SubtitleRestoreRequest;

fn english_and_russian() -> Vec<media_source_open::stream_subtitles::StreamSubtitleTrackInfo> {
    vec![
        stream_track_info(0, Some("en"), "English", StreamFlags::AUTO),
        stream_track_info(1, Some("ru"), "Russian", StreamFlags::AUTO),
    ]
}

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

#[test]
fn live_history_is_dropped_by_rebuilding_the_timeline_but_the_current_cue_stays() {
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
    let media = instance(1);
    let mut owner = stream_owner(&config, media, stream_source(&transport), Duration::ZERO);
    observe_position(&mut owner, media, Duration::ZERO, StreamTimelineKind::Live);
    click_menu_row(&mut owner, "English (stream)");
    wait_for_cues(&mut owner, 100);
    assert_eq!(
        displayed_cue_count(&owner),
        100,
        "nothing is old at the start"
    );
    let source_before = displayed_source_id(&owner);

    // Позиция ушла к 99 с: всё, что закончилось раньше 69 с, вытесняется одной пересборкой.
    observe_position(
        &mut owner,
        media,
        Duration::from_secs(99),
        StreamTimelineKind::Live,
    );

    assert_eq!(displayed_cue_count(&owner), 31);
    assert_eq!(owner.stream_feed_counters().cues_evicted, 69);
    assert_ne!(
        displayed_source_id(&owner),
        source_before,
        "a rebuilt timeline has a new source"
    );
    assert_eq!(
        displayed_active_texts(&owner, 99_500),
        ["live 99"],
        "the current cue survived"
    );
    assert!(
        displayed_active_texts(&owner, 10_500).is_empty(),
        "old cues are gone"
    );

    // Гистерезис: ещё 6 закончившихся реплик (меньше пакета в 16) пересборки не вызывают.
    let source_after_first = displayed_source_id(&owner);
    observe_position(
        &mut owner,
        media,
        Duration::from_secs(105),
        StreamTimelineKind::Live,
    );
    assert_eq!(displayed_cue_count(&owner), 31);
    assert_eq!(
        displayed_source_id(&owner),
        source_after_first,
        "no rebuild for a few dead cues"
    );
}

#[test]
fn vod_history_is_never_evicted() {
    let cues: Vec<CueSpec> = (0..100)
        .map(|second| {
            CueSpec::new(
                second * 1_000,
                second * 1_000 + 900,
                &format!("vod {second}"),
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
    let media = instance(1);
    let mut owner = stream_owner(&config, media, stream_source(&transport), Duration::ZERO);
    click_menu_row(&mut owner, "English (stream)");
    wait_for_cues(&mut owner, 100);

    observe_position(
        &mut owner,
        media,
        Duration::from_secs(99),
        StreamTimelineKind::Vod,
    );

    assert_eq!(
        displayed_cue_count(&owner),
        100,
        "the feed never resends handled VOD segments"
    );
    assert_eq!(owner.stream_feed_counters().cues_evicted, 0);
}

#[test]
fn the_user_choice_is_remembered_as_language_name_and_forced() {
    let transport =
        FakeStreamTransport::new(english_and_russian(), 10, ten_second_segments(1), true);
    let mut owner = stream_owner(
        &SubtitlesConfig::default(),
        instance(1),
        stream_source(&transport),
        Duration::ZERO,
    );

    click_menu_row(&mut owner, "Russian (stream)");

    let changes = owner
        .take_memory_changes()
        .expect("the click is a user choice");
    let expected =
        RememberedStreamSubtitle::new(Some("ru".to_owned()), "Russian".to_owned(), false)
            .expect("valid");
    assert_eq!(
        changes.choice,
        Some(RememberedSubtitleChoice::Stream(expected))
    );
}

#[test]
fn a_remembered_stream_choice_selects_the_same_rendition_on_the_next_open() {
    let transport =
        FakeStreamTransport::new(english_and_russian(), 10, ten_second_segments(2), true);
    let remembered =
        RememberedStreamSubtitle::new(Some("ru".to_owned()), "Russian".to_owned(), false)
            .expect("valid");
    let mut owner = owner_with_config(&SubtitlesConfig {
        preferred_languages: "en".to_owned(),
        ..SubtitlesConfig::default()
    });

    owner.begin_media_restoring(
        instance(1),
        SubtitleMediaSource::Stream(stream_source(&transport)),
        SubtitleRestoreRequest {
            choice: Some(RememberedSubtitleChoice::Stream(remembered)),
            sync_offset: SubtitleSyncOffset::ZERO,
        },
    );

    assert_eq!(
        running_feed_track_name(&owner).as_deref(),
        Some("Russian (stream)"),
        "memory beats the preferred language, and the remembered rendition is fed"
    );
    assert!(
        owner.take_memory_changes().is_none(),
        "restoring is not a user action"
    );
}

#[test]
fn a_remembered_rendition_the_stream_no_longer_has_falls_back_to_auto_select() {
    let transport =
        FakeStreamTransport::new(english_and_russian(), 10, ten_second_segments(2), true);
    let gone =
        RememberedStreamSubtitle::new(Some("ru".to_owned()), "Russian forced".to_owned(), true)
            .expect("valid");
    let mut owner = owner_with_config(&enabled_config());

    owner.begin_media_restoring(
        instance(1),
        SubtitleMediaSource::Stream(stream_source(&transport)),
        SubtitleRestoreRequest {
            choice: Some(RememberedSubtitleChoice::Stream(gone)),
            sync_offset: SubtitleSyncOffset::ZERO,
        },
    );

    assert_eq!(
        running_feed_track_name(&owner).as_deref(),
        Some("English (stream)")
    );
}

#[test]
fn applied_config_changes_the_live_history_limit_for_the_running_owner() {
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
    let media = instance(1);
    // История по умолчанию (10 минут): к 99 с вытеснять нечего.
    let mut owner = stream_owner(
        &SubtitlesConfig::default(),
        media,
        stream_source(&transport),
        Duration::ZERO,
    );
    observe_position(&mut owner, media, Duration::ZERO, StreamTimelineKind::Live);
    click_menu_row(&mut owner, "English (stream)");
    wait_for_cues(&mut owner, 100);
    observe_position(
        &mut owner,
        media,
        Duration::from_secs(99),
        StreamTimelineKind::Live,
    );
    assert_eq!(
        displayed_cue_count(&owner),
        100,
        "ten minutes of history keep everything"
    );

    // Пользователь сократил историю до 30 с (committed config): следующая проверка вытесняет.
    owner.apply_config(&SubtitlesConfig {
        stream: SubtitleStreamConfig {
            live_retention_s: 30,
            ..SubtitleStreamConfig::default()
        },
        ..SubtitlesConfig::default()
    });
    observe_position(
        &mut owner,
        media,
        Duration::from_secs(110),
        StreamTimelineKind::Live,
    );

    assert!(
        displayed_cue_count(&owner) < 100,
        "the shorter history is applied"
    );
    assert_eq!(displayed_active_texts(&owner, 99_500), ["live 99"]);
}

#[test]
fn a_large_sync_offset_makes_the_feed_load_the_segment_the_lookup_needs() {
    let transport =
        FakeStreamTransport::new(english_and_russian(), 10, ten_second_segments(40), true);
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
    // Игрок на 310 с, сдвиг +300 с: кадр на 310 с ищет реплики на 10 с (сегмент 1), а
    // сегменты вокруг позиции плеера (31) ленте не нужны.
    let mut owner = owner_with_config(&config);
    owner.begin_media_restoring(
        media,
        SubtitleMediaSource::Stream(stream_source(&transport)),
        SubtitleRestoreRequest {
            choice: None,
            sync_offset: SubtitleSyncOffset::from_millis(300_000),
        },
    );
    click_menu_row(&mut owner, "English (stream)");
    observe_position(
        &mut owner,
        media,
        Duration::from_secs(310),
        StreamTimelineKind::Vod,
    );

    super::embedded_support::wait_until(&mut owner, "the shifted segment", |owner| {
        !displayed_active_texts(owner, 310_800).is_empty()
    });

    assert_eq!(
        displayed_active_texts(&owner, 310_800),
        ["cue 1"],
        "lookup time 10.8 s"
    );
    assert!(
        displayed_active_texts(&owner, 400_000).is_empty()
            && owner.stream_feed_counters().cues_added <= 2,
        "the segments around the player position (31) were not needed"
    );
}

#[test]
fn runtime_sync_nudges_wake_the_feed_and_it_fetches_the_segment_the_new_lookup_needs() {
    let transport =
        FakeStreamTransport::new(english_and_russian(), 10, ten_second_segments(40), true);
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
    let position = Duration::from_secs(100);
    let mut owner = stream_owner(&config, media, stream_source(&transport), position);
    click_menu_row(&mut owner, "English (stream)");
    observe_position(&mut owner, media, position, StreamTimelineKind::Vod);
    // Лента загрузила окно вокруг 100 с (сегмент 10) и теперь простаивает.
    super::embedded_support::wait_until(&mut owner, "the first window", |owner| {
        !displayed_active_texts(owner, 100_800).is_empty()
    });
    std::thread::sleep(Duration::from_millis(300));
    let fetched_before = transport.fetch_calls();

    // Пользователь жмёт X 300 раз: субтитры позже на 30 с, кадр на 100 с ищет реплики на 70 с.
    // Seek-а нет: позиция плеера та же.
    for _ in 0..300 {
        owner.nudge_sync(crate::state::subtitles::SyncNudge::Later);
    }
    observe_position(&mut owner, media, position, StreamTimelineKind::Vod);

    // Без прыжка ленту разбудил бы только таймаут ожидания (секунды): ждём гораздо меньше.
    let deadline = std::time::Instant::now() + Duration::from_millis(1_500);
    while displayed_active_texts(&owner, 100_800) != ["cue 7"] {
        assert!(
            std::time::Instant::now() < deadline,
            "the feed was not woken by the offset change"
        );
        owner.poll_jobs();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        transport.fetch_calls() > fetched_before,
        "the new segment was requested"
    );
}

/// Два сегмента по 100 с: в первом `first` реплик по секунде, во втором `second` реплик с 100 с.
fn two_segments_of_cues(first: u32, second: u32) -> Vec<Vec<CueSpec>> {
    let early = (0..first)
        .map(|k| CueSpec::new(k * 1_000, k * 1_000 + 900, &format!("early {k}")))
        .collect();
    let late = (0..second)
        .map(|k| {
            CueSpec::new(
                100_000 + k * 1_000,
                100_000 + k * 1_000 + 900,
                &format!("late {k}"),
            )
        })
        .collect();
    vec![early, late]
}

#[test]
fn live_track_at_the_cue_limit_evicts_ended_cues_instead_of_dropping_new_ones() {
    let transport = FakeStreamTransport::new(
        english_and_russian(),
        100,
        two_segments_of_cues(10, 5),
        true,
    );
    let media = instance(1);
    let mut owner = stream_owner(
        &SubtitlesConfig::default(),
        media,
        stream_source(&transport),
        Duration::ZERO,
    );
    // Малый потолок: первый сегмент заполняет его целиком (мёртвых < пакета гистерезиса 16).
    owner.settings.embedded_limits.max_cues_per_track = 10;
    observe_position(&mut owner, media, Duration::ZERO, StreamTimelineKind::Live);
    click_menu_row(&mut owner, "English (stream)");
    wait_for_cues(&mut owner, 10);
    assert_eq!(displayed_cue_count(&owner), 10);

    // Плеер ушёл к краю: все ранние реплики закончились, приходит второй сегмент.
    observe_position(
        &mut owner,
        media,
        Duration::from_secs(100),
        StreamTimelineKind::Live,
    );
    wait_for_cues(&mut owner, 15);

    assert_eq!(displayed_cue_count(&owner), 5, "old ended cues made room");
    assert_eq!(displayed_active_texts(&owner, 100_500), ["late 0"]);
    assert_eq!(displayed_active_texts(&owner, 104_500), ["late 4"]);
    assert_eq!(owner.stream_feed_counters().cues_evicted, 10);
    assert_eq!(
        owner.stream_feed_counters().cues_over_capacity,
        0,
        "nothing new was dropped"
    );
}

#[test]
fn vod_track_at_the_cue_limit_still_skips_new_cues() {
    let transport = FakeStreamTransport::new(
        english_and_russian(),
        100,
        two_segments_of_cues(10, 5),
        true,
    );
    let media = instance(1);
    let mut owner = stream_owner(
        &SubtitlesConfig::default(),
        media,
        stream_source(&transport),
        Duration::ZERO,
    );
    owner.settings.embedded_limits.max_cues_per_track = 10;
    click_menu_row(&mut owner, "English (stream)");
    wait_for_cues(&mut owner, 10);
    observe_position(
        &mut owner,
        media,
        Duration::from_secs(100),
        StreamTimelineKind::Vod,
    );
    super::embedded_support::wait_until(&mut owner, "the over-limit cues", |owner| {
        owner.stream_feed_counters().cues_over_capacity >= 5
    });

    assert_eq!(displayed_cue_count(&owner), 10, "VOD history is kept");
    assert_eq!(owner.stream_feed_counters().cues_evicted, 0);
}

#[test]
fn network_reconnect_wait_reaches_the_feed_limits_and_survives_a_subtitle_config_change() {
    let mut owner = owner_with_config(&enabled_config());
    assert_eq!(
        owner.settings.stream_limits.feed.outage_budget,
        Duration::from_secs(60)
    );

    owner.apply_network_reconnect_wait(Duration::from_secs(5));
    assert_eq!(
        owner.settings.stream_limits.feed.outage_budget,
        Duration::from_secs(5)
    );

    // Правка секции [subtitles] не должна откатить бюджет сети к значению при старте.
    let mut changed = enabled_config();
    changed.stream.ahead_s += 1;
    owner.apply_config(&changed);
    assert_eq!(
        owner.settings.stream_limits.feed.outage_budget,
        Duration::from_secs(5)
    );
    assert_eq!(
        owner.settings.stream_limits.feed.ahead,
        Duration::from_secs(u64::from(changed.stream.ahead_s))
    );
}
