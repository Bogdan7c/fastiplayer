//! Встроенный ASS: заголовок из `codec_private` + пакеты настоящего MKV -> события ASS с
//! найденным стилем и тегами; запасные пути (нет заголовка, заголовок отвергнут) и правила
//! `ReadOrder`. Проверяется то, что видно снаружи: содержимое кэша, пиксели кадра, счётчики.

use player_core::SubtitleFeedMessage;
use subtitle_core::{AssOverride, AssTextItem, SubtitleContent};

use super::ass_sidecar::{SAMPLE_ASS, displayed_events};
use super::embedded_support::*;
use super::track_selection::{movie_with, settle};
use super::*;

/// MKV с одной ASS-дорожкой: PlayRes 64x64, стиль `Default,Noto Sans Hebrew,12`,
/// реплика `Styled {\b1}line{\b0}` на 0.5–1.5 с.
fn ass_fixture_owner(media: MediaInstanceId) -> (SubtitleOwner, DemuxedFixture) {
    let fixture = demux_fixture("ass-with-font.mkv");
    let owner = embedded_owner(&enabled_config(), media, &fixture);
    (owner, fixture)
}

/// Настоящий порядок трубы: `MediaStarted` с заголовком, затем пакеты.
fn feed_in_real_order(
    fixture: &DemuxedFixture,
    media: MediaInstanceId,
) -> Vec<SubtitleFeedMessage> {
    let mut messages = vec![fixture.media_started(media)];
    messages.extend(fixture.feed_messages(media));
    messages
}

/// Идентичность источника реплик показываемой дорожки.
fn displayed_source_id(owner: &SubtitleOwner) -> subtitle_raster::CueSourceId {
    let media = owner.media.as_ref().expect("media");
    let id = media.display_track_id().expect("displayed track");
    match &media.tracks.get(id).expect("entry").load {
        TrackLoadState::Ready(track) => track.source_id(),
        _ => panic!("displayed track must be ready"),
    }
}

#[test]
fn real_mkv_header_and_packets_give_ass_events_with_style_and_overrides_that_render() {
    let media = instance(2);
    let (mut owner, fixture) = ass_fixture_owner(media);

    owner.deliver_feed_messages(feed_in_real_order(&fixture, media));

    let events = displayed_events(&owner);
    assert_eq!(events.len(), 1, "one dialogue line in the fixture");
    let SubtitleContent::Ass(cue) = events[0].content() else {
        panic!(
            "with a header the event must be ASS, got {:?}",
            events[0].content()
        );
    };
    assert_eq!(
        cue.style().name,
        "Default",
        "style found by name in the header"
    );
    assert_eq!(cue.style().font_name, "Noto Sans Hebrew");
    assert!((cue.style().font_size - 12.0).abs() < f64::EPSILON);
    assert_eq!(cue.script.info().play_res.width, 64);
    assert_eq!(cue.script.info().play_res.height, 64);
    assert!(
        cue.text.iter().any(|item| matches!(
            item,
            AssTextItem::Overrides(tags) if tags.iter().any(|tag| matches!(tag, AssOverride::Bold(_)))
        )),
        "the \\b override of the event is present"
    );
    assert!(
        visible_texts(&mut owner, media, 1_000) > 0,
        "pixels at 1.0 s"
    );
    assert_eq!(visible_texts(&mut owner, media, 2_000), 0);
    assert_eq!(
        owner.presentation_stats().skipped_unsupported_cues,
        0,
        "an ASS cue is rendered, not counted as unsupported"
    );
    assert_eq!(owner.embedded_feed_counters().ass_headers_rejected, 0);
}

#[test]
fn without_a_header_the_track_falls_back_to_plain_text_that_still_shows() {
    let media = instance(2);
    let (mut owner, fixture) = ass_fixture_owner(media);

    owner.deliver_feed_messages(fixture.feed_messages(media));

    let events = displayed_events(&owner);
    assert_eq!(events.len(), 1);
    match events[0].content() {
        SubtitleContent::Text(cue) => {
            assert_eq!(
                cue.text.plain_text(),
                "Styled line",
                "tags are stripped by the text path"
            );
        }
        other => panic!("without a header the event must be plain text, got {other:?}"),
    }
    assert!(visible_texts(&mut owner, media, 1_000) > 0);
    assert_eq!(owner.embedded_feed_counters().ass_headers_rejected, 0);
}

#[test]
fn an_oversized_header_falls_back_to_text_counts_the_rejection_and_playback_goes_on() {
    let media = instance(2);
    let config = SubtitlesConfig {
        max_ass_header_kb: fastiplayer_config::MIN_MAX_ASS_HEADER_KB,
        ..enabled_config()
    };
    let fixture = demux_fixture("ass-with-font.mkv");
    let mut owner = embedded_owner(&config, media, &fixture);
    let ass_track = fixture.subtitle_row(0).id;
    let mut huge_header = SAMPLE_ASS.to_owned();
    huge_header.push_str(&"; padding\n".repeat(10_000));
    let started = SubtitleFeedMessage::MediaStarted(player_core::SubtitleFeedMediaStart {
        media_instance_id: media,
        attachments: media_core::MediaAttachments::default(),
        track_headers: vec![player_core::SubtitleTrackHeader {
            track_id: ass_track,
            codec_private: bytes::Bytes::from(huge_header),
        }],
    });
    let mut messages = vec![started];
    messages.extend(fixture.feed_messages(media));

    owner.deliver_feed_messages(messages);

    assert_eq!(owner.embedded_feed_counters().ass_headers_rejected, 1);
    let events = displayed_events(&owner);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].content(), SubtitleContent::Text(_)));
    assert!(
        visible_texts(&mut owner, media, 1_000) > 0,
        "text still shows"
    );
}

#[test]
fn replayed_packets_after_a_seek_do_not_add_events_and_are_counted_as_read_order_repeats() {
    let media = instance(2);
    let (mut owner, fixture) = ass_fixture_owner(media);
    let ass_track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(feed_in_real_order(&fixture, media));

    // После seek назад демуксер перечитывает те же пакеты.
    owner.deliver_feed_messages(fixture.feed_messages(media));

    assert_eq!(cached_cues(&owner, ass_track), 1, "still one event");
    assert_eq!(owner.embedded_feed_counters().cues_added, 1);
    assert_eq!(owner.embedded_feed_counters().duplicate_read_orders, 1);
}

#[test]
fn libass_rule_a_seen_read_order_is_ignored_even_when_the_line_differs() {
    let media = instance(2);
    let (mut owner, fixture) = ass_fixture_owner(media);
    let ass_track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(vec![fixture.media_started(media)]);

    owner.deliver_feed_messages(vec![
        raw_packet(
            media,
            ass_track,
            1_000,
            Some(1_000),
            b"7,0,Default,,0,0,0,,First",
        ),
        // Тот же ReadOrder, другой текст и время (ошибка муксера): libass считает повтором.
        raw_packet(
            media,
            ass_track,
            5_000,
            Some(1_000),
            b"7,0,Default,,0,0,0,,Other",
        ),
        raw_packet(
            media,
            ass_track,
            7_000,
            Some(1_000),
            b"8,0,Default,,0,0,0,,Next",
        ),
    ]);

    assert_eq!(cached_cues(&owner, ass_track), 2);
    assert_eq!(owner.embedded_feed_counters().duplicate_read_orders, 1);
    assert!(visible_texts(&mut owner, media, 1_500) > 0);
    assert_eq!(
        visible_texts(&mut owner, media, 5_500),
        0,
        "the repeat was dropped"
    );
    assert!(visible_texts(&mut owner, media, 7_500) > 0);
}

#[test]
fn tag_only_and_empty_events_are_dropped_without_an_error() {
    let media = instance(2);
    let (mut owner, fixture) = ass_fixture_owner(media);
    let ass_track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(vec![fixture.media_started(media)]);

    owner.deliver_feed_messages(vec![
        raw_packet(
            media,
            ass_track,
            1_000,
            Some(500),
            b"1,0,Default,,0,0,0,,{\\b1}",
        ),
        raw_packet(media, ass_track, 2_000, Some(500), b"2,0,Default,,0,0,0,,"),
        raw_packet(
            media,
            ass_track,
            3_000,
            Some(500),
            b"3,0,Default,,0,0,0,,Visible",
        ),
    ]);

    assert_eq!(cached_cues(&owner, ass_track), 1);
    assert_eq!(owner.embedded_feed_counters().empty_packets, 2);
    assert_eq!(owner.embedded_feed_counters().rejected_packets, 0);
}

#[test]
fn a_malformed_ass_packet_is_counted_and_the_next_one_still_works() {
    let media = instance(2);
    let (mut owner, fixture) = ass_fixture_owner(media);
    let ass_track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(vec![fixture.media_started(media)]);

    owner.deliver_feed_messages(vec![
        raw_packet(
            media,
            ass_track,
            1_000,
            Some(500),
            b"garbage without fields",
        ),
        raw_packet(
            media,
            ass_track,
            2_000,
            Some(500),
            b"4,0,Default,,0,0,0,,Fine",
        ),
    ]);

    assert_eq!(owner.embedded_feed_counters().rejected_packets, 1);
    assert_eq!(cached_cues(&owner, ass_track), 1);
}

#[test]
fn messages_arriving_before_the_media_and_the_track_list_still_end_up_as_ass() {
    // Труба обогнала приложение: сообщения пришли раньше `begin_media` и списка дорожек.
    let media = instance(3);
    let fixture = demux_fixture("ass-with-font.mkv");
    let mut owner = owner_with_config(&enabled_config());
    owner.deliver_feed_messages(feed_in_real_order(&fixture, media));

    owner.begin_media(media, SubtitleMediaSource::NotLocal);
    owner.sync_embedded_tracks(Some(media), &fixture.tracks);

    let events = displayed_events(&owner);
    assert_eq!(events.len(), 1);
    assert!(
        matches!(events[0].content(), SubtitleContent::Ass(_)),
        "deferred packets were parsed with the header, not as plain text"
    );
}

#[test]
fn a_replaced_header_starts_the_track_cache_over_under_a_new_source_id() {
    let media = instance(2);
    let (mut owner, fixture) = ass_fixture_owner(media);
    let ass_track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(feed_in_real_order(&fixture, media));
    let source_before = displayed_source_id(&owner);
    assert_eq!(cached_cues(&owner, ass_track), 1);

    owner.deliver_feed_messages(vec![fixture.media_started(media)]);

    assert_ne!(
        displayed_source_id(&owner),
        source_before,
        "CueKey must not outlive the script"
    );
    assert_eq!(
        cached_cues(&owner, ass_track),
        0,
        "cues of the old script are gone"
    );
}

#[test]
fn ass_events_of_a_new_media_never_reuse_the_previous_media_scripts() {
    let first = instance(2);
    let (mut owner, fixture) = ass_fixture_owner(first);
    owner.deliver_feed_messages(feed_in_real_order(&fixture, first));
    let ass_track = fixture.subtitle_row(0).id;

    let second = instance(3);
    owner.begin_media(second, SubtitleMediaSource::NotLocal);
    owner.sync_embedded_tracks(Some(second), &fixture.tracks);
    // Пакеты второго media без `MediaStarted` (его сообщение потеряно): запасной текст.
    owner.deliver_feed_messages(fixture.feed_messages(second));

    let events = displayed_events(&owner);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].content(), SubtitleContent::Text(_)));
    assert_eq!(cached_cues(&owner, ass_track), 1);
}

/// Меняется «мой стиль для ASS» или размер текста — картинка меняется сразу, и на паузе.
#[test]
fn toggling_user_style_or_text_size_re_renders_the_ass_overlay_on_pause() {
    let (_dir, video) = movie_with(&[("Movie.ass", SAMPLE_ASS)]);
    let mut owner = owner_with_config(&enabled_config());
    begin_and_wait(&mut owner, instance(1), &video);
    settle(&mut owner);
    let snapshot = |owner: &mut SubtitleOwner| {
        let rect = full_rect(1280, 720);
        let overlay = owner
            .overlay_for_frame(
                current(instance(1), GENERATION),
                &frame(1_500, GENERATION, rect),
            )
            .expect("the cue is visible at 1.5 s");
        (
            overlay.revision(),
            overlay.composite_rgba_over_transparent(),
            overlay.content_bounds().expect("bounds"),
        )
    };
    let (author_revision, author_pixels, author_bounds) = snapshot(&mut owner);
    let (same_revision, ..) = snapshot(&mut owner);
    assert_eq!(same_revision, author_revision, "a paused frame is reused");

    // Флажок «мой стиль для ASS»: пользовательская подложка вместо обводки автора.
    owner.apply_config(&SubtitlesConfig {
        ass_force_user_style: true,
        ..enabled_config()
    });
    let (forced_revision, forced_pixels, _) = snapshot(&mut owner);
    assert_ne!(
        forced_revision, author_revision,
        "the flag re-renders at once"
    );
    assert_ne!(
        forced_pixels, author_pixels,
        "and the picture really differs"
    );

    // Размер текста: масштаб ASS без флажка (флажок снят обратно).
    owner.apply_config(&enabled_config());
    let (_, restored_pixels, _) = snapshot(&mut owner);
    assert_eq!(restored_pixels, author_pixels, "back to the author's look");
    owner.apply_config(&SubtitlesConfig {
        text_size_percent: 200,
        ..enabled_config()
    });
    let (scaled_revision, _, scaled_bounds) = snapshot(&mut owner);
    assert_ne!(
        scaled_revision, author_revision,
        "text size re-renders at once"
    );
    assert!(
        scaled_bounds.width > author_bounds.width,
        "200 % text is wider: {} vs {}",
        scaled_bounds.width,
        author_bounds.width
    );
}

/// Режим позиции ASS доходит до владельца из конфига и применяется сразу, без перерасчёта картинки.
#[test]
fn ass_position_mode_reaches_the_owner_and_follows_config_changes() {
    use super::super::owner_placement::AssLayerPositioning;

    let (_dir, video) = movie_with(&[("Movie.ass", SAMPLE_ASS)]);
    let mut owner = owner_with_config(&enabled_config());
    begin_and_wait(&mut owner, instance(1), &video);
    settle(&mut owner);
    assert_eq!(
        owner.ass_layer_positioning(),
        AssLayerPositioning::LiftMainLinesAboveControls
    );
    assert_eq!(
        owner.shown_ass_layer_positioning(),
        None,
        "nothing is shown yet, so no ASS layer to place"
    );
    let rect = full_rect(1280, 720);
    owner
        .overlay_for_frame(
            current(instance(1), GENERATION),
            &frame(1_500, GENERATION, rect),
        )
        .expect("cue visible");
    assert_eq!(
        owner.shown_ass_layer_positioning(),
        Some(AssLayerPositioning::LiftMainLinesAboveControls)
    );
    let revision_before = owner
        .overlay_for_frame(
            current(instance(1), GENERATION),
            &frame(1_500, GENERATION, rect),
        )
        .map(|overlay| overlay.revision());

    owner.apply_config(&SubtitlesConfig {
        ass_position_mode: fastiplayer_config::AssPositionMode::Author,
        ..enabled_config()
    });
    assert_eq!(
        owner.ass_layer_positioning(),
        AssLayerPositioning::KeepAuthorPlacement
    );

    owner.apply_config(&SubtitlesConfig {
        ass_position_mode: fastiplayer_config::AssPositionMode::WholeLayer,
        ..enabled_config()
    });

    assert_eq!(
        owner.ass_layer_positioning(),
        AssLayerPositioning::MoveWholeLayerInsidePicture
    );
    let revision_after = owner
        .overlay_for_frame(
            current(instance(1), GENERATION),
            &frame(1_500, GENERATION, rect),
        )
        .map(|overlay| overlay.revision());
    assert_eq!(
        revision_after, revision_before,
        "placement mode is not a raster input"
    );
}

/// Неизвестный тег не мешает остальному тексту, а теги слоёв 2-3 копятся в статистике дорожки
/// (из неё потом строится матрица «не поддержано»).
#[test]
fn unknown_and_later_layer_tags_are_counted_per_track_and_the_text_still_renders() {
    let media = instance(2);
    let (mut owner, fixture) = ass_fixture_owner(media);
    let ass_track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(vec![fixture.media_started(media)]);

    owner.deliver_feed_messages(vec![raw_packet(
        media,
        ass_track,
        1_000,
        Some(1_000),
        b"9,0,Default,,0,0,0,,{\\k3\\zzz7\\b1}Still shown",
    )]);

    assert_eq!(cached_cues(&owner, ass_track), 1);
    let stats = owner
        .media
        .as_ref()
        .and_then(|media| media.embedded_ass.tag_stats(ass_track))
        .expect("the track has a parsed script");
    // `\k3` и `\b1` разобраны; считается только действительно неизвестный `\zzz7`.
    assert_eq!(stats.malformed(), 0);
    assert_eq!(stats.unknown_total(), 1);
    assert!(
        visible_texts(&mut owner, media, 1_500) > 0,
        "text still renders"
    );
}

#[test]
fn a_rebuilt_track_accepts_replayed_events_instead_of_dropping_them_as_read_order_repeats() {
    let media = instance(2);
    let (mut owner, fixture) = ass_fixture_owner(media);
    let ass_track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(feed_in_real_order(&fixture, media));
    assert_eq!(cached_cues(&owner, ass_track), 1);

    // Дорожка исчезает из снимка плеера и появляется снова: кэш реплик начинается заново.
    owner.sync_embedded_tracks(Some(media), &[]);
    owner.sync_embedded_tracks(Some(media), &fixture.tracks);
    assert_eq!(cached_cues(&owner, ass_track), 0, "новый кэш пуст");
    owner.deliver_feed_messages(fixture.feed_messages(media));

    assert_eq!(
        cached_cues(&owner, ass_track),
        1,
        "события дорожки вернулись"
    );
    assert_eq!(owner.embedded_feed_counters().duplicate_read_orders, 0);
}

#[test]
fn an_invisible_or_empty_event_does_not_burn_its_read_order() {
    let media = instance(2);
    let (mut owner, fixture) = ass_fixture_owner(media);
    let ass_track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(vec![fixture.media_started(media)]);

    owner.deliver_feed_messages(vec![
        // Пустой текст с ReadOrder 5, затем нормальное событие с тем же ReadOrder.
        raw_packet(media, ass_track, 1_000, Some(500), b"5,0,Default,,0,0,0,,"),
        raw_packet(
            media,
            ass_track,
            2_000,
            Some(500),
            b"5,0,Default,,0,0,0,,Shown",
        ),
    ]);

    assert_eq!(cached_cues(&owner, ass_track), 1);
    assert_eq!(owner.embedded_feed_counters().duplicate_read_orders, 0);
}

#[test]
fn ass_events_without_duration_are_invisible_like_in_libass_and_counted() {
    let media = instance(2);
    let (mut owner, fixture) = ass_fixture_owner(media);
    let ass_track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(vec![fixture.media_started(media)]);

    owner.deliver_feed_messages(vec![
        raw_packet(
            media,
            ass_track,
            1_000,
            None,
            b"1,0,Default,,0,0,0,,NoDuration",
        ),
        raw_packet(
            media,
            ass_track,
            2_000,
            Some(0),
            b"2,0,Default,,0,0,0,,Zero",
        ),
        raw_packet(
            media,
            ass_track,
            3_000,
            Some(500),
            b"3,0,Default,,0,0,0,,Fine",
        ),
    ]);

    assert_eq!(
        cached_cues(&owner, ass_track),
        1,
        "только событие с длительностью"
    );
    assert_eq!(owner.embedded_feed_counters().ass_zero_duration_events, 2);
    assert_eq!(owner.embedded_feed_counters().rejected_packets, 0);
    assert_eq!(
        visible_texts(&mut owner, media, 1_200),
        0,
        "5-секундного показа нет"
    );
}
