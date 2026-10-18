//! Поток пакетов встроенных дорожек -> кэш реплик владельца: маршрутизация по media,
//! дубликаты после seek, пределы, ошибки и то, что реплика реально появляется на кадре.

use media_core::subtitle_codec;

use super::embedded_support::*;
use super::*;

fn two_srt_owner(media: MediaInstanceId) -> (SubtitleOwner, DemuxedFixture) {
    let fixture = demux_fixture("two-srt-tracks.mkv");
    let owner = embedded_owner(&enabled_config(), media, &fixture);
    (owner, fixture)
}

#[test]
fn real_packets_fill_every_track_cache_and_the_selected_cue_becomes_visible_on_a_frame() {
    let media = instance(2);
    let (mut owner, fixture) = two_srt_owner(media);
    let english = fixture.subtitle_row(0).id;
    let russian = fixture.subtitle_row(1).id;
    // До прихода пакетов показывать нечего, хотя дорожка уже выбрана автовыбором.
    assert_eq!(visible_texts(&mut owner, media, 1_000), 0);

    let update = owner.deliver_feed_messages(fixture.feed_messages(media));

    assert_eq!(
        update,
        EmbeddedUpdate::Changed,
        "the shown track received cues"
    );
    assert_eq!(cached_cues(&owner, english), 2);
    assert_eq!(
        cached_cues(&owner, russian),
        2,
        "cues of the unselected track are cached too"
    );
    // Новые реплики сразу видны на кадре: показанный ранее «пустой» кадр не залип.
    assert!(
        visible_texts(&mut owner, media, 1_000) > 0,
        "cue [0.5 s, 1.5 s) is visible at 1.0 s"
    );
    assert_eq!(
        visible_texts(&mut owner, media, 1_700),
        0,
        "no cue between the two"
    );
    assert!(visible_texts(&mut owner, media, 2_100) > 0);
    assert_eq!(visible_texts(&mut owner, media, 3_000), 0);
}

#[test]
fn a_cue_arriving_while_its_time_is_on_screen_replaces_the_blank_frame() {
    // Пауза внутри будущей реплики: кадр уже растеризован как «пусто», затем приходит пакет.
    let media = instance(2);
    let (mut owner, fixture) = two_srt_owner(media);
    let english = fixture.subtitle_row(0).id;
    assert_eq!(visible_texts(&mut owner, media, 1_000), 0);
    let calls_before = owner.presentation_stats().rasterizer_calls;

    let hello: Vec<_> = fixture
        .subtitle_packets
        .iter()
        .filter(|packet| packet.track_id == english)
        .take(1)
        .map(|packet| packet_message(media, packet))
        .collect();
    owner.deliver_feed_messages(hello);

    assert!(visible_texts(&mut owner, media, 1_000) > 0);
    assert_eq!(
        owner.presentation_stats().rasterizer_calls,
        calls_before + 1,
        "the new cue triggers exactly one rasterization"
    );
    // Дальше на том же кадре растеризации нет.
    assert!(visible_texts(&mut owner, media, 1_000) > 0);
    assert_eq!(
        owner.presentation_stats().rasterizer_calls,
        calls_before + 1
    );
}

#[test]
fn replaying_the_same_packets_after_seek_back_adds_no_duplicate_cues() {
    let media = instance(2);
    let (mut owner, fixture) = two_srt_owner(media);
    let english = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(fixture.feed_messages(media));
    let counters_before = owner.embedded_feed_counters();
    assert_eq!(counters_before.cues_added, 4);

    // Seek назад: демуксер читает те же пакеты ещё раз.
    let update = owner.deliver_feed_messages(fixture.feed_messages(media));

    assert_eq!(update, EmbeddedUpdate::Unchanged, "nothing new to show");
    assert_eq!(cached_cues(&owner, english), 2);
    let counters = owner.embedded_feed_counters();
    assert_eq!(counters.cues_added, 4);
    assert_eq!(counters.duplicate_cues, 4);
    // Прочитанная ранее реплика показывается из кэша.
    assert!(visible_texts(&mut owner, media, 2_100) > 0);
}

#[test]
fn messages_of_a_previous_media_are_ignored_and_future_media_waits_for_its_installation() {
    let fixture = demux_fixture("two-srt-tracks.mkv");
    let english = fixture.subtitle_row(0).id;
    let mut owner = embedded_owner(&enabled_config(), instance(5), &fixture);

    // Старое media (меньший идентификатор): пакеты выбрасываются и считаются.
    owner.deliver_feed_messages(fixture.feed_messages(instance(4)));
    assert_eq!(cached_cues(&owner, english), 0);
    assert_eq!(owner.embedded_feed_counters().stale_messages, 4);

    // Будущее media: труба обогнала установку. Пакеты ждут и доставляются после begin_media.
    owner.deliver_feed_messages(fixture.feed_messages(instance(6)));
    assert_eq!(cached_cues(&owner, english), 0, "not the current media yet");
    owner.begin_media(instance(6), SubtitleMediaSource::NotLocal);
    owner.sync_embedded_tracks(Some(instance(6)), &fixture.tracks);
    assert_eq!(
        cached_cues(&owner, english),
        2,
        "parked packets reach the new media after its tracks are known"
    );
    assert_eq!(owner.embedded_feed_counters().stale_messages, 4);
}

#[test]
fn packets_of_a_track_missing_from_the_snapshot_wait_until_the_track_appears() {
    let media = instance(2);
    let fixture = demux_fixture("two-srt-tracks.mkv");
    let english = fixture.subtitle_row(0).id;
    let mut owner = owner_with_config(&enabled_config());
    owner.begin_media(media, SubtitleMediaSource::NotLocal);

    // Пакеты пришли раньше, чем снимок плеера показал дорожки.
    owner.deliver_feed_messages(fixture.feed_messages(media));
    owner.sync_embedded_tracks(Some(media), &fixture.tracks);

    assert_eq!(cached_cues(&owner, english), 2);
}

#[test]
fn switching_media_drops_the_embedded_tracks_and_their_cues() {
    let (mut owner, fixture) = two_srt_owner(instance(2));
    owner.deliver_feed_messages(fixture.feed_messages(instance(2)));
    assert!(!owner.menu_view().tracks.is_empty());

    owner.begin_media(instance(3), SubtitleMediaSource::NotLocal);

    assert!(
        owner.menu_view().tracks.is_empty(),
        "the previous media's tracks are gone"
    );
    assert_eq!(visible_texts(&mut owner, instance(3), 1_000), 0);
    // Пакеты прежнего media после смены уже чужие.
    owner.deliver_feed_messages(fixture.feed_messages(instance(2)));
    assert!(owner.menu_view().tracks.is_empty());
    assert!(owner.embedded_feed_counters().stale_messages >= 4);
}

#[test]
fn cue_cache_is_capped_by_the_per_track_limit_and_the_excess_is_counted() {
    let media = instance(2);
    let (mut owner, fixture) = two_srt_owner(media);
    let english = fixture.subtitle_row(0).id;
    owner.settings.embedded_limits.max_cues_per_track = 2;
    let messages: Vec<_> = (0..5)
        .map(|index| raw_packet(media, english, 1_000 * (index + 1), Some(500), b"line"))
        .collect();

    owner.deliver_feed_messages(messages);

    assert_eq!(cached_cues(&owner, english), 2);
    assert_eq!(owner.embedded_feed_counters().cues_over_capacity, 3);
}

#[test]
fn broken_and_empty_packets_are_skipped_with_counters_and_good_ones_still_show() {
    let media = instance(2);
    let fixture = demux_fixture("ass-with-font.mkv");
    let ass = fixture.subtitle_row(0).id;
    assert_eq!(fixture.subtitle_row(0).codec_id, subtitle_codec::ASS);
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);

    owner.deliver_feed_messages(vec![
        // Слишком мало полей ASS-строки.
        raw_packet(media, ass, 1_000, Some(500), b"0,0,Default"),
        // Корректная строка без видимого текста.
        raw_packet(media, ass, 3_000, Some(500), b"1,0,Default,,0,0,0,,"),
        // Нормальная реплика.
        raw_packet(media, ass, 5_000, Some(500), b"2,0,Default,,0,0,0,,Fine"),
    ]);

    let counters = owner.embedded_feed_counters();
    assert_eq!(counters.rejected_packets, 1);
    assert_eq!(counters.empty_packets, 1);
    assert_eq!(counters.cues_added, 1);
    assert_eq!(cached_cues(&owner, ass), 1);
    assert!(visible_texts(&mut owner, media, 5_100) > 0);
}

#[test]
fn packet_without_duration_is_shown_for_the_configured_open_duration() {
    let media = instance(2);
    let (mut owner, fixture) = two_srt_owner(media);
    let english = fixture.subtitle_row(0).id;
    let open_ms = u64::try_from(
        owner
            .settings
            .embedded_limits
            .cue_max_open_duration
            .as_millis(),
    )
    .expect("duration fits");

    owner.deliver_feed_messages(vec![raw_packet(
        media,
        english,
        10_000,
        None,
        b"Open ended",
    )]);

    assert!(visible_texts(&mut owner, media, 10_000 + open_ms - 1) > 0);
    assert_eq!(visible_texts(&mut owner, media, 10_000 + open_ms), 0);
}

#[test]
fn bitmap_track_packets_are_not_cached() {
    let media = instance(2);
    let mut fixture = demux_fixture("two-srt-tracks.mkv");
    let pgs_id = media_core::TrackId::new(90);
    let mut pgs_row = fixture.subtitle_row(0).clone();
    pgs_row.id = pgs_id;
    // VobSub: картинки, которые пока не показываем (PGS показывается, см. `embedded_pgs`).
    pgs_row.codec_id = subtitle_codec::VOBSUB.to_owned();
    fixture.tracks.push(pgs_row);
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);

    owner.deliver_feed_messages(vec![raw_packet(
        media,
        pgs_id,
        1_000,
        Some(500),
        &[1, 2, 3],
    )]);

    assert_eq!(owner.embedded_feed_counters().unsupported_track_packets, 1);
    assert_eq!(owner.embedded_feed_counters().cues_added, 0);
}
