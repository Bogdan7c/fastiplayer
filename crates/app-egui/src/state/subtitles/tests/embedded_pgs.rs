//! Встроенный PGS у владельца: настоящие пакеты `pgs-basic.mkv` -> труба -> декодер ->
//! хранилище композиций -> шкала дорожки -> (настоящий поток-рисовальщик) -> overlay.
//!
//! Эталон фикстуры (`symphonia-demux/tests/fixtures/subtitles/README.md`): блок 0.5 с — картинка
//! (красная/зелёная половины), 1.5 с — «очистка», 2.0 с — вторая картинка (синяя/белая с
//! альфой), 2.6 с — «очистка». Все блоки без длительности: конец задаёт следующий набор.
//! Пиксели на GPU проверяет `embedded_pgs_gpu`.

use media_core::{Packet, TrackId, subtitle_codec};
use player_core::{SubtitleFeedMessage, SubtitleFeedPacket, SubtitleReadRun};
use subtitle_core::SubtitleTime;

use super::embedded_support::*;
use super::*;
use crate::state::subtitles::render_worker::clock_tracker::{ObservedMode, PlaybackObservation};
use crate::state::subtitles::render_worker::schedule::PlaybackRate;

/// Номера блоков фикстуры по порядку файла.
pub(super) const FIRST_PICTURE: usize = 0;
pub(super) const FIRST_CLEAR: usize = 1;
pub(super) const SECOND_PICTURE: usize = 2;
pub(super) const SECOND_CLEAR: usize = 3;

/// Проход чтения демуксера с номером `index` (0 — первый, каждый seek увеличивает).
pub(super) fn read_run(index: u32) -> SubtitleReadRun {
    (0..index).fold(SubtitleReadRun::INITIAL, |run, _| run.next())
}

/// Сообщение трубы для блока фикстуры: дорожка `track` и проход чтения `run`.
pub(super) fn pgs_message(
    media: MediaInstanceId,
    packet: &Packet,
    track: TrackId,
    run: u32,
) -> SubtitleFeedMessage {
    SubtitleFeedMessage::Packet(SubtitleFeedPacket {
        media_instance_id: media,
        track_id: track,
        presentation_time: packet.pts,
        duration: packet.duration,
        read_run: read_run(run),
        payload: packet.data.clone(),
    })
}

/// Сообщения для выбранных блоков фикстуры одним проходом чтения.
pub(super) fn blocks(
    fixture: &DemuxedFixture,
    media: MediaInstanceId,
    track: TrackId,
    run: u32,
    indices: &[usize],
) -> Vec<SubtitleFeedMessage> {
    indices
        .iter()
        .map(|&index| pgs_message(media, &fixture.subtitle_packets[index], track, run))
        .collect()
}

/// Фикстура с дополнительной PGS-дорожкой (копия первой с другим номером).
pub(super) fn fixture_with_second_pgs_track() -> (DemuxedFixture, TrackId, TrackId) {
    let mut fixture = demux_fixture("pgs-basic.mkv");
    let first = fixture.subtitle_row(0).id;
    let second = TrackId::new(first.get() + 50);
    let mut row = fixture.subtitle_row(0).clone();
    row.id = second;
    fixture.tracks.push(row);
    (fixture, first, second)
}

/// Позиция воспроизведения для политики памяти (как снимок плеера на обычном воспроизведении).
pub(super) fn observe_position(owner: &mut SubtitleOwner, media: MediaInstanceId, ms: u64) {
    owner.observe_playback(
        current(media, GENERATION),
        PlaybackObservation {
            mode: ObservedMode::Playing,
            rate: PlaybackRate::NORMAL,
            estimated_step: None,
            position: Some(Duration::from_millis(ms)),
            read_run: SubtitleReadRun::INITIAL,
        },
    );
}

/// Сколько событий шкалы дорожки активно в `at_ms` (то, что увидит зритель до растеризации).
pub(super) fn active_events(owner: &SubtitleOwner, player_track: TrackId, at_ms: i64) -> usize {
    let media = owner.media.as_ref().expect("media");
    let id = media
        .tracks
        .find_embedded(player_track)
        .expect("track in the list");
    let TrackLoadState::Ready(track) = &media.tracks.get(id).expect("entry").load else {
        panic!("PGS track must be Ready");
    };
    let mut active = Vec::new();
    track
        .timeline()
        .collect_active_at(SubtitleTime::from_millis(at_ms), &mut active);
    active.len()
}

/// Показывает ли презентатор что-нибудь на кадре `at_ms` показываемой дорожки.
pub(super) fn shows_picture(owner: &mut SubtitleOwner, media: MediaInstanceId, at_ms: u64) -> bool {
    owner
        .overlay_for_frame(
            current(media, GENERATION),
            &frame(at_ms, GENERATION, full_rect(256, 144)),
        )
        .is_some()
}

/// Сколько хранится композиций у дорожки (в хранилище, не на шкале).
pub(super) fn stored_compositions(owner: &SubtitleOwner, player_track: TrackId) -> usize {
    let id = owner_track_id(owner, player_track);
    owner
        .media
        .as_ref()
        .and_then(|media| media.embedded_pgs.track(id))
        .map_or(0, |state| state.stored_compositions())
}

pub(super) fn selected_pgs_owner(
    config: &SubtitlesConfig,
) -> (SubtitleOwner, MediaInstanceId, DemuxedFixture) {
    let media = instance(2);
    let fixture = demux_fixture("pgs-basic.mkv");
    let owner = embedded_owner(config, media, &fixture);
    (owner, media, fixture)
}

#[test]
fn pgs_blocks_become_picture_events_closed_by_the_next_display_set() {
    let (mut owner, media, fixture) = selected_pgs_owner(&enabled_config());
    let track = fixture.subtitle_row(0).id;

    owner.deliver_feed_messages(fixture.feed_messages(media));

    // Две картинки; «очистки» событий не дают.
    assert_eq!(cached_cues(&owner, track), 2);
    assert_eq!(active_events(&owner, track, 400), 0, "до первого набора");
    assert_eq!(active_events(&owner, track, 1_000), 1, "первая картинка");
    assert_eq!(
        active_events(&owner, track, 1_700),
        0,
        "после очистки 1.5 с"
    );
    assert_eq!(active_events(&owner, track, 2_300), 1, "вторая картинка");
    assert_eq!(
        active_events(&owner, track, 2_700),
        0,
        "после очистки 2.6 с"
    );
    let counters = owner.media.as_ref().expect("media").embedded_pgs.counters();
    assert_eq!(counters.compositions_added, 4);
    assert_eq!(
        counters.timeline_rebuilds, 0,
        "хвостовые вставки идут без пересборки"
    );
    // Выбранная автоматически дорожка действительно доходит до экрана (до растеризатора).
    assert!(shows_picture(&mut owner, media, 1_000));
    assert!(!shows_picture(&mut owner, media, 1_700));
    assert!(shows_picture(&mut owner, media, 2_300));
}

#[test]
fn successor_arriving_later_closes_the_open_picture_without_blinking() {
    let (mut owner, media, fixture) = selected_pgs_owner(&enabled_config());
    let track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(blocks(&fixture, media, track, 0, &[FIRST_PICTURE]));
    assert!(shows_picture(&mut owner, media, 1_000));
    assert!(
        shows_picture(&mut owner, media, 1_700),
        "пока преемника нет, открытая картинка остаётся на экране"
    );
    let diagnostics = owner.presenter.worker_diagnostics().expect("worker");

    // Преемник («очистка» 1.5 с) приходит позже, отдельной пачкой.
    owner.deliver_feed_messages(blocks(&fixture, media, track, 0, &[FIRST_CLEAR]));

    assert!(
        shows_picture(&mut owner, media, 1_000),
        "в момент до нового конца картинка видна и после укорочения"
    );
    assert!(
        !shows_picture(&mut owner, media, 1_700),
        "после нового конца картинки нет"
    );
    let after = owner.presenter.worker_diagnostics().expect("worker");
    assert_eq!(
        after.track_replaces, diagnostics.track_replaces,
        "укорочение дошло до потока узкой дельтой, а не полной копией дорожки"
    );
    assert!(after.track_updates > diagnostics.track_updates);
    let counters = owner.media.as_ref().expect("media").embedded_pgs.counters();
    assert_eq!(counters.timeline_rebuilds, 0);
}

#[test]
fn after_a_seek_the_open_composition_is_dropped_and_nothing_stale_is_shown() {
    let (mut owner, media, fixture) = selected_pgs_owner(&enabled_config());
    let track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(blocks(&fixture, media, track, 0, &[FIRST_PICTURE]));
    assert!(
        shows_picture(&mut owner, media, 1_200),
        "открытая картинка видна"
    );

    // Seek вперёд: новый проход чтения начинается с блока 2.0 с, конец прежней картинки
    // неизвестен (мог быть в пропущенном участке).
    owner.deliver_feed_messages(blocks(&fixture, media, track, 1, &[SECOND_PICTURE]));

    assert!(
        !shows_picture(&mut owner, media, 1_200),
        "залипшей картинки нет"
    );
    assert!(
        shows_picture(&mut owner, media, 2_300),
        "новая картинка видна"
    );
    let counters = owner.media.as_ref().expect("media").embedded_pgs.counters();
    assert_eq!(counters.discontinuities, 1);
    assert_eq!(
        counters.timeline_rebuilds, 1,
        "открытая картинка снята пересборкой"
    );
}

#[test]
fn seeking_back_shows_closed_pictures_from_the_cache_and_re_read_restores_the_dropped_one() {
    let (mut owner, media, fixture) = selected_pgs_owner(&enabled_config());
    let track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(blocks(
        &fixture,
        media,
        track,
        0,
        &[FIRST_PICTURE, FIRST_CLEAR, SECOND_PICTURE],
    ));
    // Seek (пакет следующего прохода): открытая картинка 2.0 с снята, закрытая первая — в кэше.
    owner.deliver_feed_messages(blocks(&fixture, media, track, 1, &[SECOND_CLEAR]));
    let counters_after_seek = owner.media.as_ref().expect("media").embedded_pgs.counters();

    assert!(
        shows_picture(&mut owner, media, 1_000),
        "первая картинка закрыта 1.5 с и осталась в кэше после seek"
    );
    assert!(
        !shows_picture(&mut owner, media, 2_300),
        "открытая вторая снята"
    );

    // Перечитанные после seek назад блоки второй картинки возвращают её.
    owner.deliver_feed_messages(blocks(
        &fixture,
        media,
        track,
        2,
        &[SECOND_PICTURE, SECOND_CLEAR],
    ));

    assert!(shows_picture(&mut owner, media, 2_300));
    assert!(
        shows_picture(&mut owner, media, 1_000),
        "кэш первой картинки цел"
    );
    assert!(!shows_picture(&mut owner, media, 2_700));
    let counters = owner.media.as_ref().expect("media").embedded_pgs.counters();
    assert_eq!(
        counters.discontinuities,
        counters_after_seek.discontinuities + 1
    );
}

#[test]
fn re_reading_known_blocks_in_the_same_pass_changes_nothing() {
    let (mut owner, media, fixture) = selected_pgs_owner(&enabled_config());
    let track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(fixture.feed_messages(media));
    let before = owner.media.as_ref().expect("media").embedded_pgs.counters();

    owner.deliver_feed_messages(fixture.feed_messages(media));

    let after = owner.media.as_ref().expect("media").embedded_pgs.counters();
    assert_eq!(
        after.duplicate_compositions,
        before.duplicate_compositions + 4
    );
    assert_eq!(after.timeline_rebuilds, 0);
    assert_eq!(cached_cues(&owner, track), 2, "дубликаты не плодят события");
    assert_eq!(active_events(&owner, track, 1_000), 1);
}

#[test]
fn out_of_order_inserts_rebuild_the_timeline_once_per_batch() {
    let (mut owner, media, fixture) = selected_pgs_owner(&enabled_config());
    let track = fixture.subtitle_row(0).id;

    // Одна пачка: блоки приходят не по порядку времени, значит каждая вставка перед хвостом
    // требует пересборки, но пересборка одна.
    owner.deliver_feed_messages(blocks(
        &fixture,
        media,
        track,
        0,
        &[SECOND_PICTURE, FIRST_PICTURE, FIRST_CLEAR],
    ));

    let counters = owner.media.as_ref().expect("media").embedded_pgs.counters();
    assert_eq!(counters.timeline_rebuilds, 1);
    assert_eq!(active_events(&owner, track, 1_000), 1);
    assert_eq!(
        active_events(&owner, track, 1_700),
        0,
        "очистка 1.5 с закрыла первую картинку"
    );
    assert_eq!(active_events(&owner, track, 2_300), 1);
}

#[test]
fn a_removed_and_returned_track_starts_from_an_empty_cache() {
    let (mut owner, media, fixture) = selected_pgs_owner(&enabled_config());
    let track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(fixture.feed_messages(media));
    assert_eq!(stored_compositions(&owner, track), 4);

    owner.sync_embedded_tracks(Some(media), &[]);
    owner.sync_embedded_tracks(Some(media), &fixture.tracks);

    assert_eq!(
        stored_compositions(&owner, track),
        0,
        "старое хранилище забыто"
    );
    assert_eq!(cached_cues(&owner, track), 0);
    owner.deliver_feed_messages(fixture.feed_messages(media));
    assert_eq!(cached_cues(&owner, track), 2, "дорожка читается с нуля");
}

#[test]
fn background_track_keeps_only_its_window_while_the_shown_track_keeps_everything() {
    let config = SubtitlesConfig {
        pgs_background_track_window_s: 0,
        ..enabled_config()
    };
    let media = instance(2);
    let (fixture, shown, background) = fixture_with_second_pgs_track();
    let mut owner = embedded_owner(&config, media, &fixture);
    observe_position(&mut owner, media, 2_300);

    for track in [shown, background] {
        owner.deliver_feed_messages(blocks(
            &fixture,
            media,
            track,
            0,
            &[FIRST_PICTURE, FIRST_CLEAR, SECOND_PICTURE, SECOND_CLEAR],
        ));
    }

    // Показывается первая дорожка (автовыбор); у фоновой остались картинка, активная в позиции
    // 2.3 с, и открытая «очистка».
    assert_eq!(
        active_events(&owner, shown, 1_000),
        1,
        "показываемая хранит всё"
    );
    assert_eq!(stored_compositions(&owner, shown), 4);
    assert_eq!(
        active_events(&owner, background, 1_000),
        0,
        "фоновая забыла старое"
    );
    assert_eq!(
        active_events(&owner, background, 2_300),
        1,
        "активная в позиции осталась"
    );
    assert_eq!(stored_compositions(&owner, background), 2);
    let counters = owner.media.as_ref().expect("media").embedded_pgs.counters();
    assert_eq!(counters.evicted_compositions, 2);
}

#[test]
fn switching_the_shown_track_applies_the_new_roles_and_keeps_the_current_picture() {
    let config = SubtitlesConfig {
        pgs_background_track_window_s: 0,
        ..enabled_config()
    };
    let media = instance(2);
    let (fixture, first, second) = fixture_with_second_pgs_track();
    let mut owner = embedded_owner(&config, media, &fixture);
    observe_position(&mut owner, media, 2_300);
    for track in [first, second] {
        owner.deliver_feed_messages(blocks(
            &fixture,
            media,
            track,
            0,
            &[FIRST_PICTURE, FIRST_CLEAR, SECOND_PICTURE, SECOND_CLEAR],
        ));
    }
    assert_eq!(stored_compositions(&owner, first), 4, "первая показывается");
    assert_eq!(stored_compositions(&owner, second), 2, "вторая в фоне");

    // Пользователь выбирает вторую дорожку.
    let second_menu_id = owner.menu_view().tracks[1].id;
    assert_eq!(
        owner.select_track(second_menu_id),
        SelectTrackOutcome::Selected
    );
    owner.drain_embedded_feed();

    assert_eq!(
        stored_compositions(&owner, first),
        2,
        "бывшая показываемая сжата до хвоста"
    );
    assert_eq!(
        active_events(&owner, first, 2_300),
        1,
        "её текущая картинка цела"
    );
    assert!(
        shows_picture(&mut owner, media, 2_300),
        "новая показываемая дорожка показывает текущую картинку сразу"
    );
}

#[test]
fn shown_track_over_budget_drops_the_pictures_farthest_from_the_position() {
    let media = instance(2);
    let fixture = demux_fixture("pgs-basic.mkv");
    let track = fixture.subtitle_row(0).id;
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);
    observe_position(&mut owner, media, 2_300);
    owner.deliver_feed_messages(blocks(
        &fixture,
        media,
        track,
        0,
        &[FIRST_PICTURE, FIRST_CLEAR, SECOND_PICTURE],
    ));
    let stored_bytes = owner
        .media
        .as_ref()
        .and_then(|media| media.embedded_pgs.track(owner_track_id(&owner, track)))
        .map(|state| state.estimated_bytes())
        .expect("pgs state");
    assert!(stored_bytes > 0);
    // Бюджет на байт меньше занятого: ближайшая вставка выбивает его.
    owner.settings.embedded_limits.pgs = owner
        .settings
        .embedded_limits
        .pgs
        .with_shown_budget_bytes(stored_bytes - 1);

    owner.deliver_feed_messages(blocks(&fixture, media, track, 0, &[SECOND_CLEAR]));

    assert_eq!(
        active_events(&owner, track, 1_000),
        0,
        "самая далёкая от позиции картинка вытеснена"
    );
    assert_eq!(
        active_events(&owner, track, 2_300),
        1,
        "активная в позиции не вытесняется"
    );
    assert!(shows_picture(&mut owner, media, 2_300));
    let counters = owner.media.as_ref().expect("media").embedded_pgs.counters();
    assert!(counters.evicted_compositions >= 1);
    assert_eq!(counters.timeline_rebuilds, 1);
}

#[test]
fn memory_policy_waits_for_a_known_position() {
    let config = SubtitlesConfig {
        pgs_background_track_window_s: 0,
        ..enabled_config()
    };
    let media = instance(2);
    let (fixture, _shown, background) = fixture_with_second_pgs_track();
    let mut owner = embedded_owner(&config, media, &fixture);

    // Позиция неизвестна: фоновая дорожка ничего не вытесняет (нечего сравнивать).
    owner.deliver_feed_messages(blocks(
        &fixture,
        media,
        background,
        0,
        &[FIRST_PICTURE, FIRST_CLEAR, SECOND_PICTURE, SECOND_CLEAR],
    ));

    assert_eq!(stored_compositions(&owner, background), 4);
    assert_eq!(active_events(&owner, background, 1_000), 1);
}

#[test]
fn pgs_track_is_auto_selected_by_language_over_a_non_matching_text_track() {
    let media = instance(2);
    let mut fixture = demux_fixture("pgs-basic.mkv");
    // Перед английской PGS-дорожкой ставим немецкую текстовую: автовыбор по языку обязан
    // перешагнуть её.
    let mut german = fixture.subtitle_row(0).clone();
    german.id = TrackId::new(70);
    german.codec_id = subtitle_codec::TEXT_UTF8.to_owned();
    german.labels.language = Some(media_core::TrackLanguage::new("deu"));
    fixture.tracks.insert(0, german);
    let config = SubtitlesConfig {
        preferred_languages: "en".to_owned(),
        ..enabled_config()
    };

    let owner = embedded_owner(&config, media, &fixture);

    let view = owner.menu_view();
    let selected: Vec<_> = view
        .tracks
        .iter()
        .filter(|track| track.is_selected)
        .collect();
    assert_eq!(selected.len(), 1);
    assert_eq!(
        selected[0].label.format,
        Some(subtitle_core::SubtitleFormat::Pgs)
    );
}

#[test]
fn a_broken_block_is_counted_and_skipped_while_the_track_keeps_working() {
    let (mut owner, media, fixture) = selected_pgs_owner(&enabled_config());
    let track = fixture.subtitle_row(0).id;
    let mut messages = vec![raw_packet(media, track, 300, None, &[1, 2, 3])];
    messages.extend(fixture.feed_messages(media));

    owner.deliver_feed_messages(messages);

    let state = owner
        .media
        .as_ref()
        .and_then(|media| media.embedded_pgs.track(owner_track_id(&owner, track)))
        .expect("pgs state");
    assert!(
        state.decode_statistics().blocks_with_framing_errors >= 1,
        "the broken block is counted, not silently ignored"
    );
    assert_eq!(
        active_events(&owner, track, 1_000),
        1,
        "the real pictures still show"
    );
    assert_eq!(active_events(&owner, track, 2_300), 1);
}
